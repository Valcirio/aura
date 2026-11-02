//! Pinned catalog and installer for native local-speech models.
//!
//! The catalog is compiled in: revisions, URLs, sizes and SHA-256 hashes are
//! constants. The public API accepts only an artifact id, never URLs or
//! arbitrary metadata.
//!
//! Layout: `<root>/<id>/<revision>/<files>` plus `<root>/<id>/current.json`.
//!
//! Concurrency note: callers serialize installers per artifact. Should
//! installers still race at revision publication (both stage, one renames
//! first and the other verifies the winner's directory), a pointer is only ever
//! published after every file of the revision verified against the catalog.
//! Installers delete only their own staging/quarantine directories, never other
//! `.partial-*` or `.corrupt-*` directories. Hashing, large copies, syncs and
//! publication run on tracked blocking workers that cleanup cancels and joins.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Per-file download ceiling (accommodates the 674,716,297-byte turbo encoder).
pub const MAX_FILE_BYTES: u64 = 700_000_000;
const MAX_REDIRECTS: usize = 5;
const MAX_POINTER_BYTES: u64 = 64 * 1024;

pub const DEFAULT_ASR_ID: &str = "whisper-turbo";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogFile {
    /// Path of the file in the upstream repository.
    pub source_file: &'static str,
    pub url: &'static str,
    /// Installed basename.
    pub name: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Artifact {
    pub id: &'static str,
    pub label: &'static str,
    /// `asr`, `vad` or `tts`.
    pub kind: &'static str,
    pub description: &'static str,
    /// Settings value selecting this model (ASR only).
    pub asr_model: Option<&'static str>,
    pub recommended: bool,
    pub warning: Option<&'static str>,
    pub license: &'static str,
    pub revision: &'static str,
    pub files: &'static [CatalogFile],
    /// Recommended inference threads (ASR only).
    pub threads: Option<u32>,
    /// Default voice name (TTS only).
    pub voice: Option<&'static str>,
    /// Default speaker id (TTS only).
    pub speaker: Option<u32>,
    /// Planned phonemizer for pt-BR (TTS only); not implemented here.
    pub phonemizer: Option<&'static str>,
}

static WHISPER_TURBO_FILES: [CatalogFile; 3] = [
    CatalogFile {
        source_file: "turbo-encoder.int8.onnx",
        url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-turbo/resolve/2ca6ff69fc878651b770880507669577ac41c2ff/turbo-encoder.int8.onnx",
        name: "turbo-encoder.int8.onnx",
        size: 674716297,
        sha256: "b02dcdf54f348741e93fe732b67d933c8dcb6735655f710640143081db38878b",
    },
    CatalogFile {
        source_file: "turbo-decoder.int8.onnx",
        url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-turbo/resolve/2ca6ff69fc878651b770880507669577ac41c2ff/turbo-decoder.int8.onnx",
        name: "turbo-decoder.int8.onnx",
        size: 361080764,
        sha256: "20accd02388482eb3a46bd615631adfdc85e1eb2c7db9ea3f02a40ffe6b81547",
    },
    CatalogFile {
        source_file: "turbo-tokens.txt",
        url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-turbo/resolve/2ca6ff69fc878651b770880507669577ac41c2ff/turbo-tokens.txt",
        name: "turbo-tokens.txt",
        size: 816730,
        sha256: "b34b360dbb493e781e479794586d661700670d65564001f23024971d1f2fa126",
    },
];

static WHISPER_SMALL_FILES: [CatalogFile; 3] = [
    CatalogFile {
        source_file: "small-encoder.int8.onnx",
        url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-small/resolve/8f3c18b358db4d1f2fc1eae49d75cd20989e4309/small-encoder.int8.onnx",
        name: "small-encoder.int8.onnx",
        size: 112442483,
        sha256: "4cbe7b22fa9026b843b60a68640c747de05bafb1a11b57edc0e66c232d9f33a9",
    },
    CatalogFile {
        source_file: "small-decoder.int8.onnx",
        url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-small/resolve/8f3c18b358db4d1f2fc1eae49d75cd20989e4309/small-decoder.int8.onnx",
        name: "small-decoder.int8.onnx",
        size: 262226114,
        sha256: "acad50b5c782696e91b55914cc5ab4f756f1532f76e22aa6fc615f39fb69a8ee",
    },
    CatalogFile {
        source_file: "small-tokens.txt",
        url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-small/resolve/8f3c18b358db4d1f2fc1eae49d75cd20989e4309/small-tokens.txt",
        name: "small-tokens.txt",
        size: 816730,
        sha256: "b34b360dbb493e781e479794586d661700670d65564001f23024971d1f2fa126",
    },
];

static SILERO_VAD_FILES: [CatalogFile; 1] = [CatalogFile {
    source_file: "files/silero_vad.onnx",
    url: "https://raw.githubusercontent.com/snakers4/silero-vad/5cd2ba54db059f961e7545d4f211b44f005a6dfb/files/silero_vad.onnx",
    name: "silero_vad.onnx",
    size: 2313101,
    sha256: "6b99cbfd39246b6706f98ec13c7c50c6b299181f2474fa05cbc8046acc274396",
}];

static KOKORO_FILES: [CatalogFile; 4] = [
    CatalogFile {
        source_file: "model.onnx",
        url: "https://huggingface.co/csukuangfj/kokoro-multi-lang-v1_0/resolve/f7b96bb6bef5c5da4d3aa4f4e0498fbbf62dc78b/model.onnx",
        name: "model.onnx",
        size: 325560556,
        sha256: "b40f62b166ac8164b0627ef48a0b358eda0985e272fb03ef5252e7206305da11",
    },
    CatalogFile {
        source_file: "voices.bin",
        url: "https://huggingface.co/csukuangfj/kokoro-multi-lang-v1_0/resolve/f7b96bb6bef5c5da4d3aa4f4e0498fbbf62dc78b/voices.bin",
        name: "voices.bin",
        size: 28200960,
        sha256: "1c5a5b983d3d50d8586d437a51f3faa2da7919ce76a013c081e65671a3447c29",
    },
    CatalogFile {
        source_file: "tokens.txt",
        url: "https://huggingface.co/csukuangfj/kokoro-multi-lang-v1_0/resolve/f7b96bb6bef5c5da4d3aa4f4e0498fbbf62dc78b/tokens.txt",
        name: "tokens.txt",
        size: 687,
        sha256: "6ebb6bb288f20f3ae8d004d3c2ca27697da27c037d75e81a60e2a6a663f95425",
    },
    CatalogFile {
        source_file: "LICENSE",
        url: "https://huggingface.co/csukuangfj/kokoro-multi-lang-v1_0/resolve/f7b96bb6bef5c5da4d3aa4f4e0498fbbf62dc78b/LICENSE",
        name: "LICENSE",
        size: 11358,
        sha256: "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30",
    },
];

static CATALOG: [Artifact; 4] = [
    Artifact {
        id: "whisper-turbo",
        label: "Whisper Turbo",
        kind: "asr",
        description: "Reconhecimento de fala local, mais preciso. Recomendado.",
        asr_model: Some("turbo"),
        recommended: true,
        warning: None,
        license: "MIT",
        revision: "2ca6ff69fc878651b770880507669577ac41c2ff",
        files: &WHISPER_TURBO_FILES,
        threads: Some(8),
        voice: None,
        speaker: None,
        phonemizer: None,
    },
    Artifact {
        id: "whisper-small",
        label: "Whisper Small",
        kind: "asr",
        description: "Reconhecimento de fala local, mais leve e rápido.",
        asr_model: Some("small"),
        recommended: false,
        warning: Some("Modelo mais leve; pode reduzir a precisão da transcrição."),
        license: "MIT",
        revision: "8f3c18b358db4d1f2fc1eae49d75cd20989e4309",
        files: &WHISPER_SMALL_FILES,
        threads: Some(4),
        voice: None,
        speaker: None,
        phonemizer: None,
    },
    Artifact {
        id: "silero-vad",
        label: "Silero VAD",
        kind: "vad",
        description: "Detecção de atividade de voz para identificar quando você fala.",
        asr_model: None,
        recommended: false,
        warning: None,
        license: "MIT",
        revision: "5cd2ba54db059f961e7545d4f211b44f005a6dfb",
        files: &SILERO_VAD_FILES,
        threads: None,
        voice: None,
        speaker: None,
        phonemizer: None,
    },
    Artifact {
        id: "kokoro-multi-lang-v1_0",
        label: "Kokoro (pt-BR)",
        kind: "tts",
        description: "Síntese de voz local multilíngue com a voz pt-BR Dora.",
        asr_model: None,
        recommended: false,
        warning: None,
        license: "Apache-2.0",
        revision: "f7b96bb6bef5c5da4d3aa4f4e0498fbbf62dc78b",
        files: &KOKORO_FILES,
        threads: None,
        voice: Some("pf_dora"),
        speaker: Some(42),
        phonemizer: Some("piper-phonemize"),
    },
];

/// Read-only view of the compiled catalog.
pub fn catalog() -> &'static [Artifact] {
    &CATALOG
}

/// Read-only lookup of a catalog entry by id.
pub fn lookup(id: &str) -> Option<&'static Artifact> {
    CATALOG.iter().find(|artifact| artifact.id == id)
}

#[derive(Debug)]
pub enum ModelError {
    UnknownId(String),
    InvalidPath(String),
    NotInstalled(String),
    Integrity(String),
    TooLarge(String),
    Network(String),
    Io(String),
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownId(id) => write!(f, "Modelo fora do catálogo: {id}"),
            Self::InvalidPath(what) => write!(f, "Caminho inválido: {what}"),
            Self::NotInstalled(id) => {
                write!(f, "Instale {id} em Configurações > Modelos locais")
            }
            Self::Integrity(what) => write!(f, "Integridade não confere: {what}"),
            Self::TooLarge(what) => write!(f, "Download excede o limite: {what}"),
            Self::Network(what) => write!(f, "Falha de rede: {what}"),
            Self::Io(what) => write!(f, "Erro de E/S: {what}"),
        }
    }
}

impl std::error::Error for ModelError {}

impl From<std::io::Error> for ModelError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub artifact: String,
    pub downloaded: u64,
    pub total: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRecord {
    pub file: String,
    pub source: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallManifest {
    pub id: String,
    pub revision: String,
    pub license: String,
    pub files: Vec<FileRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactStatus {
    pub id: String,
    pub installed: bool,
    pub license: String,
    pub label: String,
    pub kind: String,
    pub description: String,
    pub asr_model: Option<String>,
    pub recommended: bool,
    pub warning: Option<String>,
    pub voice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// Always empty: no Python package probing applies to native models.
    pub packages: BTreeMap<String, bool>,
    pub artifacts: Vec<ArtifactStatus>,
}

fn manifest_for(artifact: &Artifact) -> InstallManifest {
    InstallManifest {
        id: artifact.id.to_string(),
        revision: artifact.revision.to_string(),
        license: artifact.license.to_string(),
        files: artifact
            .files
            .iter()
            .map(|file| FileRecord {
                file: file.name.to_string(),
                source: file.source_file.to_string(),
                sha256: file.sha256.to_string(),
                size: file.size,
            })
            .collect(),
    }
}

fn valid_component(name: &str) -> bool {
    if name.is_empty() || name.contains('\\') || name.contains('/') || name.contains('\0') {
        return false;
    }
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(part)) if part == name)
        && components.next().is_none()
}

fn is_symlink(path: &Path) -> Result<bool, ModelError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.file_type().is_symlink()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Requires `path` to be an existing real (non-symlink) directory.
fn require_real_dir(path: &Path) -> Result<(), ModelError> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(ModelError::InvalidPath(path.display().to_string()));
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Verifies a regular, non-symlink file against the pinned size and SHA-256.
fn verify_file(path: &Path, file: &CatalogFile) -> Result<(), ModelError> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(ModelError::InvalidPath(path.display().to_string()));
    }
    if meta.len() != file.size {
        return Err(ModelError::Integrity(format!("tamanho de {}", file.name)));
    }
    let mut source = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        hasher.update(&buffer[..read]);
    }
    if total != file.size || hex(&hasher.finalize()) != file.sha256 {
        return Err(ModelError::Integrity(format!("hash de {}", file.name)));
    }
    Ok(())
}

type ByteStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>, ModelError>> + Send>>;

#[async_trait::async_trait]
trait Source: Send + Sync {
    async fn open(&self, url: &str) -> Result<ByteStream, ModelError>;
}

struct HttpSource {
    client: reqwest::Client,
}

impl HttpSource {
    fn new() -> Result<Self, ModelError> {
        let policy = reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("redirecionamentos demais")
            } else if attempt.url().scheme() != "https" {
                attempt.error("redirecionamento para destino não HTTPS")
            } else {
                attempt.follow()
            }
        });
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(policy)
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(60))
            .timeout(Duration::from_secs(3600))
            .build()
            .map_err(|error| ModelError::Network(error.to_string()))?;
        Ok(Self { client })
    }
}

#[async_trait::async_trait]
impl Source for HttpSource {
    async fn open(&self, url: &str) -> Result<ByteStream, ModelError> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|error| ModelError::Network(error.to_string()))?;
        if !response.status().is_success() {
            return Err(ModelError::Network(format!("HTTP {}", response.status())));
        }
        Ok(Box::pin(response.bytes_stream().map(|chunk| {
            chunk
                .map(|bytes| bytes.to_vec())
                .map_err(|error| ModelError::Network(error.to_string()))
        })))
    }
}

/// Installation progress. Superset of [`Progress`]: the `artifact`, `downloaded`
/// and `total` keys keep their meaning (bytes of the current file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub artifact: String,
    /// `verifying`, `copying` or `downloading`.
    pub phase: String,
    pub file: Option<String>,
    pub downloaded: u64,
    pub total: u64,
}

/// Tracks blocking workers of one installation so cleanup can cancel them and
/// wait until none can still write into the staging directory.
struct Work {
    cancelled: AtomicBool,
    active: Mutex<usize>,
    idle: Condvar,
}

impl Work {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            cancelled: AtomicBool::new(false),
            active: Mutex::new(0),
            idle: Condvar::new(),
        })
    }

    fn check(&self) -> Result<(), ModelError> {
        if self.cancelled.load(Ordering::SeqCst) {
            Err(ModelError::Io("instalação cancelada".to_string()))
        } else {
            Ok(())
        }
    }

    /// Requests cancellation and blocks until every registered worker finished.
    fn cancel_and_wait(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        while *active > 0 {
            active = self.idle.wait(active).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// Registered before the worker is spawned and released when the worker
/// finishes or is discarded without running.
struct ActiveGuard(Arc<Work>);

impl ActiveGuard {
    fn enter(work: &Arc<Work>) -> Self {
        *work.active.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        Self(work.clone())
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        let mut active = self.0.active.lock().unwrap_or_else(|e| e.into_inner());
        *active -= 1;
        if *active == 0 {
            self.0.idle.notify_all();
        }
    }
}

/// Receives `(file, cumulative bytes of that file)` from blocking workers.
type Tick<'a> = &'a dyn Fn(&'static str, u64);

/// Runs `work_fn` on the blocking pool, forwarding its byte ticks to `on_tick`
/// on the calling task. Dropping the future never leaves the worker untracked.
async fn blocking<T: Send + 'static>(
    work: &Arc<Work>,
    work_fn: impl FnOnce(&Work, Tick<'_>) -> Result<T, ModelError> + Send + 'static,
    mut on_tick: impl FnMut(&'static str, u64),
) -> Result<T, ModelError> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(&'static str, u64)>();
    let guard = ActiveGuard::enter(work);
    let mut handle = tokio::task::spawn_blocking(move || {
        let guard = guard;
        work_fn(&guard.0, &|file, bytes| {
            let _ = tx.send((file, bytes));
        })
    });
    loop {
        tokio::select! {
            result = &mut handle => {
                while let Ok((file, bytes)) = rx.try_recv() {
                    on_tick(file, bytes);
                }
                return result
                    .map_err(|_| ModelError::Io("tarefa de instalação interrompida".to_string()))?;
            }
            Some((file, bytes)) = rx.recv() => on_tick(file, bytes),
        }
    }
}

/// Cancels and joins this invocation's blocking workers, then removes its
/// staging directory, quarantine directory and pointer temporary file.
struct Cleanup {
    work: Arc<Work>,
    staging: PathBuf,
    pointer_tmp: PathBuf,
    quarantine: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        self.work.cancel_and_wait();
        let _ = std::fs::remove_dir_all(&self.staging);
        let _ = std::fs::remove_dir_all(&self.quarantine);
        let _ = std::fs::remove_file(&self.pointer_tmp);
    }
}

/// Installs a catalog artifact by id, downloading only the pinned files.
pub async fn install(
    root: &Path,
    id: &str,
    progress: impl Fn(Progress) + Send + Sync,
) -> Result<InstallManifest, ModelError> {
    install_managed(root, None, id, |report| {
        progress(Progress {
            artifact: report.artifact,
            downloaded: report.downloaded,
            total: report.total,
        })
    })
    .await
}

/// Like [`install`], but reuses verified installed content and, per file,
/// verified copies under `<local_root>/<id>/<name>` before touching the network.
/// Callers serialize invocations per artifact; dropping the future cancels the
/// installation and removes its staging.
pub async fn install_managed(
    root: &Path,
    local_root: Option<&Path>,
    id: &str,
    progress: impl Fn(Report) + Send + Sync,
) -> Result<InstallManifest, ModelError> {
    // Unknown ids are rejected before any filesystem or network activity.
    if lookup(id).is_none() {
        return Err(ModelError::UnknownId(id.to_string()));
    }
    let source = HttpSource::new()?;
    install_with(root, local_root, &CATALOG, &source, id, &progress).await
}

/// Test-only transport that refuses every fetch and records the attempt.
#[cfg(test)]
pub(crate) struct NoNetworkSource {
    pub attempts: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
#[async_trait::async_trait]
impl Source for NoNetworkSource {
    async fn open(&self, _url: &str) -> Result<ByteStream, ModelError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Err(ModelError::Network("rede bloqueada no teste".into()))
    }
}

/// Test-only [`install_managed`] over the real catalog that can only use verified
/// installed/local content; any fetch attempt is counted in `attempts` and fails.
#[cfg(test)]
pub(crate) async fn install_local_only(
    root: &Path,
    local_root: &Path,
    id: &str,
    attempts: Arc<std::sync::atomic::AtomicUsize>,
    progress: impl Fn(Report) + Send + Sync,
) -> Result<InstallManifest, ModelError> {
    if lookup(id).is_none() {
        return Err(ModelError::UnknownId(id.to_string()));
    }
    let source = NoNetworkSource { attempts };
    install_with(root, Some(local_root), &CATALOG, &source, id, &progress).await
}

fn file_size(artifact: &Artifact, name: &str) -> u64 {
    artifact
        .files
        .iter()
        .find(|file| file.name == name)
        .map_or(0, |file| file.size)
}

async fn install_with(
    root: &Path,
    local_root: Option<&Path>,
    catalog: &'static [Artifact],
    source: &dyn Source,
    id: &str,
    progress: &(dyn Fn(Report) + Send + Sync),
) -> Result<InstallManifest, ModelError> {
    let artifact = catalog
        .iter()
        .find(|artifact| artifact.id == id)
        .ok_or_else(|| ModelError::UnknownId(id.to_string()))?;
    if !valid_component(artifact.id) || !valid_component(artifact.revision) {
        return Err(ModelError::InvalidPath(artifact.id.to_string()));
    }
    for file in artifact.files {
        if !valid_component(file.name) {
            return Err(ModelError::InvalidPath(file.name.to_string()));
        }
        if file.size > MAX_FILE_BYTES {
            return Err(ModelError::TooLarge(file.name.to_string()));
        }
    }
    std::fs::create_dir_all(root)?;
    let artifact_dir = root.join(artifact.id);
    if !artifact_dir.exists() && !is_symlink(&artifact_dir)? {
        std::fs::create_dir(&artifact_dir)?;
    }
    require_real_dir(&artifact_dir)?;

    let report = |phase: &str, file: &str, downloaded: u64| {
        progress(Report {
            artifact: artifact.id.to_string(),
            phase: phase.to_string(),
            file: Some(file.to_string()),
            downloaded,
            total: file_size(artifact, file),
        })
    };

    let work = Work::new();
    let token = uuid::Uuid::new_v4();
    let staging = artifact_dir.join(format!(".partial-{token}"));
    let pointer_tmp = artifact_dir.join(format!(".current-{token}.tmp"));
    let quarantine = artifact_dir.join(format!(".corrupt-{token}"));
    let _cleanup = Cleanup {
        work: work.clone(),
        staging: staging.clone(),
        pointer_tmp: pointer_tmp.clone(),
        quarantine: quarantine.clone(),
    };
    let manifest = manifest_for(artifact);
    let encoded =
        serde_json::to_vec(&manifest).map_err(|error| ModelError::Io(error.to_string()))?;
    let target = artifact_dir.join(artifact.revision);
    let pointer = artifact_dir.join("current.json");

    // An installed revision is verified first: valid ones only get their pointer
    // repaired, corrupt ones are removed before a verified replacement is staged.
    if target.exists() || is_symlink(&target)? {
        let valid = {
            let target = target.clone();
            blocking(
                &work,
                move |work, tick| inspect_revision(&target, artifact, work, tick),
                |file, bytes| report("verifying", file, bytes),
            )
            .await?
        };
        if valid {
            let (tmp, current, bytes) = (pointer_tmp.clone(), pointer.clone(), encoded.clone());
            blocking(
                &work,
                move |work, _| {
                    work.check()?;
                    write_pointer(&tmp, &current, &bytes)
                },
                |_, _| {},
            )
            .await?;
            return Ok(manifest);
        }
        let (current, revision, trash) = (pointer.clone(), target.clone(), quarantine.clone());
        blocking(
            &work,
            move |work, _| {
                work.check()?;
                match std::fs::remove_file(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                std::fs::rename(&revision, &trash)?;
                std::fs::remove_dir_all(&trash)?;
                Ok(())
            },
            |_, _| {},
        )
        .await?;
    }

    std::fs::create_dir(&staging)?;
    for file in artifact.files {
        let destination = staging.join(file.name);
        if let Some(local_root) = local_root {
            let candidate = local_root.join(artifact.id).join(file.name);
            let destination = destination.clone();
            let copied = blocking(
                &work,
                move |work, tick| copy_verified(&candidate, &destination, file, work, tick),
                |name, bytes| report("copying", name, bytes),
            )
            .await?;
            if copied {
                continue;
            }
        }
        download_file(source, file, &destination, &work, |bytes| {
            report("downloading", file.name, bytes)
        })
        .await?;
    }

    let (staging_dir, revision, dir, tmp, bytes) = (
        staging.clone(),
        target.clone(),
        artifact_dir.clone(),
        pointer_tmp.clone(),
        encoded,
    );
    blocking(
        &work,
        move |work, tick| {
            publish(
                &staging_dir,
                &revision,
                &dir,
                artifact,
                &tmp,
                &bytes,
                work,
                tick,
            )
        },
        |_, _| {},
    )
    .await?;
    Ok(manifest)
}

/// Streams one pinned file into `destination`. Hashing, writes and sync run on
/// tracked blocking workers in batches.
async fn download_file(
    source: &dyn Source,
    file: &'static CatalogFile,
    destination: &Path,
    work: &Arc<Work>,
    progress: impl Fn(u64),
) -> Result<(), ModelError> {
    const BATCH: usize = 1024 * 1024;
    let mut stream = source.open(file.url).await?;
    let out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut state = (out, Sha256::new());
    let mut pending: Vec<u8> = Vec::new();
    let mut total = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        total += chunk.len() as u64;
        if total > file.size || total > MAX_FILE_BYTES {
            return Err(ModelError::TooLarge(file.name.to_string()));
        }
        pending.extend_from_slice(&chunk);
        progress(total);
        if pending.len() >= BATCH {
            state = write_batch(work, state, std::mem::take(&mut pending)).await?;
        }
    }
    state = write_batch(work, state, std::mem::take(&mut pending)).await?;
    let digest = blocking(
        work,
        move |work, _| {
            work.check()?;
            let (mut out, hasher) = state;
            std::io::Write::flush(&mut out)?;
            out.sync_all()?;
            Ok(hex(&hasher.finalize()))
        },
        |_, _| {},
    )
    .await?;
    if total != file.size {
        return Err(ModelError::Integrity(format!("tamanho de {}", file.name)));
    }
    if digest != file.sha256 {
        return Err(ModelError::Integrity(format!("hash de {}", file.name)));
    }
    Ok(())
}

async fn write_batch(
    work: &Arc<Work>,
    state: (std::fs::File, Sha256),
    batch: Vec<u8>,
) -> Result<(std::fs::File, Sha256), ModelError> {
    if batch.is_empty() {
        return Ok(state);
    }
    blocking(
        work,
        move |work, _| {
            work.check()?;
            let (mut out, mut hasher) = state;
            hasher.update(&batch);
            std::io::Write::write_all(&mut out, &batch)?;
            Ok((out, hasher))
        },
        |_, _| {},
    )
    .await
}

/// Hashes a regular, non-symlink file. `Ok(false)` when it is absent, not a
/// regular file, or differs from the pinned size/hash.
fn hash_matches(
    path: &Path,
    file: &CatalogFile,
    work: &Work,
    tick: Tick<'_>,
) -> Result<bool, ModelError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() != file.size {
        return Ok(false);
    }
    let mut source = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        work.check()?;
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > file.size {
            return Ok(false);
        }
        hasher.update(&buffer[..read]);
        tick(file.name, total);
    }
    Ok(total == file.size && hex(&hasher.finalize()) == file.sha256)
}

/// `Ok(true)` when the revision directory and every catalog file verify;
/// `Ok(false)` when content is missing or corrupt; `Err` for a symlinked or
/// non-directory revision, which is rejected rather than touched.
fn inspect_revision(
    dir: &Path,
    artifact: &Artifact,
    work: &Work,
    tick: Tick<'_>,
) -> Result<bool, ModelError> {
    require_real_dir(dir)?;
    for file in artifact.files {
        if !hash_matches(&dir.join(file.name), file, work, tick)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Copies a local file into staging while hashing. Any problem with the source
/// discards the staged file and yields `Ok(false)` (fall back to the network);
/// failures writing staging are errors.
fn copy_verified(
    source: &Path,
    destination: &Path,
    file: &CatalogFile,
    work: &Work,
    tick: Tick<'_>,
) -> Result<bool, ModelError> {
    match std::fs::symlink_metadata(source) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() && meta.len() == file.size => {
        }
        _ => return Ok(false),
    }
    let Ok(mut input) = std::fs::File::open(source) else {
        return Ok(false);
    };
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let result = (|| -> Result<bool, ModelError> {
        let mut hasher = Sha256::new();
        let mut total = 0u64;
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            work.check()?;
            let read = match input.read(&mut buffer) {
                Ok(read) => read,
                Err(_) => return Ok(false),
            };
            if read == 0 {
                break;
            }
            total += read as u64;
            if total > file.size {
                return Ok(false);
            }
            hasher.update(&buffer[..read]);
            std::io::Write::write_all(&mut out, &buffer[..read])?;
            tick(file.name, total);
        }
        if total != file.size || hex(&hasher.finalize()) != file.sha256 {
            return Ok(false);
        }
        std::io::Write::flush(&mut out)?;
        out.sync_all()?;
        Ok(true)
    })();
    if !matches!(result, Ok(true)) {
        drop(out);
        let _ = std::fs::remove_file(destination);
    }
    result
}

/// Writes the manifest, publishes the staged revision (or accepts an
/// identical verified one that appeared meanwhile) and then the pointer.
#[allow(clippy::too_many_arguments)]
fn publish(
    staging: &Path,
    target: &Path,
    artifact_dir: &Path,
    artifact: &Artifact,
    pointer_tmp: &Path,
    manifest: &[u8],
    work: &Work,
    tick: Tick<'_>,
) -> Result<(), ModelError> {
    work.check()?;
    write_synced(&staging.join("manifest.json"), manifest)?;
    let ensure_valid = || -> Result<(), ModelError> {
        if inspect_revision(target, artifact, work, tick)? {
            Ok(())
        } else {
            Err(ModelError::Integrity(format!("revisão de {}", artifact.id)))
        }
    };
    if target.exists() || is_symlink(target)? {
        // Same revision already present: never overwrite, verify before reuse.
        ensure_valid()?;
    } else if let Err(error) = std::fs::rename(staging, target) {
        // Another installer may have published first; reuse only if it verifies.
        if target.exists() {
            ensure_valid()?;
        } else {
            return Err(error.into());
        }
    }
    work.check()?;
    write_pointer(pointer_tmp, &artifact_dir.join("current.json"), manifest)
}

fn write_pointer(tmp: &Path, current: &Path, manifest: &[u8]) -> Result<(), ModelError> {
    write_synced(tmp, manifest)?;
    std::fs::rename(tmp, current)?;
    Ok(())
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), ModelError> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

/// Reports which catalog artifacts have a pointer file. No network, no inference.
pub fn status(root: &Path) -> Status {
    status_in(root, &CATALOG)
}

fn status_in(root: &Path, catalog: &[Artifact]) -> Status {
    let artifacts = catalog
        .iter()
        .map(|artifact| {
            let pointer = root.join(artifact.id).join("current.json");
            let installed = !is_symlink(&root.join(artifact.id)).unwrap_or(true)
                && std::fs::symlink_metadata(&pointer)
                    .map(|meta| meta.is_file())
                    .unwrap_or(false);
            ArtifactStatus {
                id: artifact.id.to_string(),
                installed,
                license: artifact.license.to_string(),
                label: artifact.label.to_string(),
                kind: artifact.kind.to_string(),
                description: artifact.description.to_string(),
                asr_model: artifact.asr_model.map(str::to_string),
                recommended: artifact.recommended,
                warning: artifact.warning.map(str::to_string),
                voice: artifact.voice.map(str::to_string),
            }
        })
        .collect();
    Status {
        packages: BTreeMap::new(),
        artifacts,
    }
}

/// Resolves a verified installed file. `filename` is the installed basename.
pub fn artifact_path(root: &Path, id: &str, filename: &str) -> Result<PathBuf, ModelError> {
    artifact_path_in(root, &CATALOG, id, filename)
}

fn artifact_path_in(
    root: &Path,
    catalog: &[Artifact],
    id: &str,
    filename: &str,
) -> Result<PathBuf, ModelError> {
    let artifact = catalog
        .iter()
        .find(|artifact| artifact.id == id)
        .ok_or_else(|| ModelError::UnknownId(id.to_string()))?;
    if !valid_component(filename) {
        return Err(ModelError::InvalidPath(filename.to_string()));
    }
    let file = artifact
        .files
        .iter()
        .find(|file| file.name == filename)
        .ok_or_else(|| ModelError::InvalidPath(filename.to_string()))?;
    if !valid_component(artifact.revision) {
        return Err(ModelError::InvalidPath(artifact.revision.to_string()));
    }
    let artifact_dir = root.join(artifact.id);
    let pointer = artifact_dir.join("current.json");
    match std::fs::symlink_metadata(&pointer) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
            return Err(ModelError::InvalidPath(pointer.display().to_string()));
        }
        Ok(meta) if meta.len() > MAX_POINTER_BYTES => {
            return Err(ModelError::Integrity("ponteiro".to_string()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ModelError::NotInstalled(id.to_string()));
        }
        Err(error) => return Err(error.into()),
    }
    require_real_dir(&artifact_dir)?;
    let stored: InstallManifest = serde_json::from_slice(&std::fs::read(&pointer)?)
        .map_err(|_| ModelError::Integrity("ponteiro".to_string()))?;
    if stored != manifest_for(artifact) {
        return Err(ModelError::Integrity("ponteiro".to_string()));
    }
    let revision_dir = artifact_dir.join(artifact.revision);
    require_real_dir(&revision_dir)?;
    let path = revision_dir.join(file.name);
    verify_file(&path, file)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FixtureSource {
        data: HashMap<String, Vec<u8>>,
        fetches: AtomicUsize,
    }

    impl FixtureSource {
        fn new(entries: &[(&str, &[u8])]) -> Self {
            Self {
                data: entries
                    .iter()
                    .map(|(name, bytes)| (format!("https://fixture.test/{name}"), bytes.to_vec()))
                    .collect(),
                fetches: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl Source for FixtureSource {
        async fn open(&self, url: &str) -> Result<ByteStream, ModelError> {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            let bytes = self
                .data
                .get(url)
                .ok_or_else(|| ModelError::Network("404".into()))?
                .clone();
            let chunks: Vec<_> = bytes.chunks(3).map(|c| Ok(c.to_vec())).collect();
            Ok(Box::pin(futures_util::stream::iter(chunks)))
        }
    }

    const A: &[u8] = b"alpha-bytes";
    const B: &[u8] = b"beta";

    fn sha(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    fn leak(text: String) -> &'static str {
        Box::leak(text.into_boxed_str())
    }

    fn fixture_catalog() -> &'static [Artifact] {
        let files: Vec<CatalogFile> = [("a.bin", A), ("b.txt", B)]
            .iter()
            .map(|(name, bytes)| CatalogFile {
                source_file: leak(format!("sub/{name}")),
                url: leak(format!("https://fixture.test/{name}")),
                name,
                size: bytes.len() as u64,
                sha256: leak(sha(bytes)),
            })
            .collect();
        let files: &'static [CatalogFile] = Box::leak(files.into_boxed_slice());
        Box::leak(Box::new([Artifact {
            id: "fixture",
            label: "Fixture",
            kind: "asr",
            description: "Fixture",
            asr_model: None,
            recommended: false,
            warning: None,
            license: "MIT",
            revision: "rev1",
            files,
            threads: None,
            voice: None,
            speaker: None,
            phonemizer: None,
        }]))
    }

    #[test]
    fn status_exposes_catalog_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let status = status_in(dir.path(), catalog());
        let ids: Vec<&str> = status.artifacts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "whisper-turbo",
                "whisper-small",
                "silero-vad",
                "kokoro-multi-lang-v1_0"
            ]
        );
        assert!(status.packages.is_empty());
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["packages"], serde_json::json!({}));
        let a = &json["artifacts"];
        assert_eq!(a[0]["asr_model"], "turbo");
        assert_eq!(a[0]["recommended"], true);
        assert_eq!(a[0]["kind"], "asr");
        assert_eq!(a[1]["asr_model"], "small");
        assert_eq!(
            a[1]["warning"],
            "Modelo mais leve; pode reduzir a precisão da transcrição."
        );
        assert_eq!(a[2]["kind"], "vad");
        assert_eq!(a[3]["kind"], "tts");
        assert_eq!(a[3]["voice"], "pf_dora");
        for item in a.as_array().unwrap() {
            assert_eq!(item["installed"], false);
            assert!(item["license"].is_string());
            assert!(item["label"].as_str().unwrap().len() > 0);
            assert!(item["description"].as_str().unwrap().len() > 0);
        }
    }

    fn good_source() -> FixtureSource {
        FixtureSource::new(&[("a.bin", A), ("b.txt", B)])
    }

    async fn run(
        root: &Path,
        source: &FixtureSource,
        id: &str,
    ) -> Result<InstallManifest, ModelError> {
        install_with(root, None, fixture_catalog(), source, id, &|_| {}).await
    }

    fn partials(root: &Path) -> Vec<String> {
        std::fs::read_dir(root.join("fixture"))
            .map(|dir| {
                dir.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| {
                        n.starts_with(".partial-")
                            || n.starts_with(".current-")
                            || n.starts_with(".corrupt-")
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn successful_install_and_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let source = good_source();
        let manifest = run(dir.path(), &source, "fixture").await.unwrap();
        assert_eq!(manifest.revision, "rev1");
        assert_eq!(manifest.files[0].source, "sub/a.bin");
        let path = artifact_path_in(dir.path(), fixture_catalog(), "fixture", "a.bin").unwrap();
        assert_eq!(std::fs::read(path).unwrap(), A);
        let pointer: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.path().join("fixture/current.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(pointer["id"], "fixture");
        assert_eq!(pointer["files"][1]["size"], B.len());
        assert!(partials(dir.path()).is_empty());
        // Reinstall of a verified revision reuses it.
        run(dir.path(), &source, "fixture").await.unwrap();
    }

    #[tokio::test]
    async fn progress_values() {
        let dir = tempfile::tempdir().unwrap();
        let events = Mutex::new(Vec::new());
        install_with(
            dir.path(),
            None,
            fixture_catalog(),
            &good_source(),
            "fixture",
            &|p| events.lock().unwrap().push(p),
        )
        .await
        .unwrap();
        let events = events.into_inner().unwrap();
        let last_a = events
            .iter()
            .filter(|e| e.total == A.len() as u64)
            .last()
            .unwrap();
        assert_eq!(last_a.downloaded, A.len() as u64);
        assert_eq!(last_a.file.as_deref(), Some("a.bin"));
        assert_eq!(events.last().unwrap().downloaded, B.len() as u64);
        assert!(events.iter().all(|e| e.artifact == "fixture"));
        assert!(events.iter().all(|e| e.phase == "downloading"));
        let json = serde_json::to_value(&events[0]).unwrap();
        let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["artifact", "downloaded", "file", "phase", "total"]);
    }

    #[tokio::test]
    async fn same_size_corrupt_bytes_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let source = FixtureSource::new(&[("a.bin", b"alpha-bytez"), ("b.txt", B)]);
        let err = run(dir.path(), &source, "fixture").await.unwrap_err();
        assert!(matches!(err, ModelError::Integrity(_)));
        assert!(!dir.path().join("fixture/current.json").exists());
        assert!(partials(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn truncated_and_oversized_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let short = FixtureSource::new(&[("a.bin", b"alpha"), ("b.txt", B)]);
        assert!(matches!(
            run(dir.path(), &short, "fixture").await.unwrap_err(),
            ModelError::Integrity(_)
        ));
        let long = FixtureSource::new(&[("a.bin", b"alpha-bytes-extra"), ("b.txt", B)]);
        assert!(matches!(
            run(dir.path(), &long, "fixture").await.unwrap_err(),
            ModelError::TooLarge(_)
        ));
        assert!(!dir.path().join("fixture/current.json").exists());
        assert!(partials(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn unknown_id_does_not_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("models");
        let source = good_source();
        assert!(matches!(
            run(&root, &source, "../evil").await.unwrap_err(),
            ModelError::UnknownId(_)
        ));
        assert!(matches!(
            install(&root, "nope", |_| {}).await.unwrap_err(),
            ModelError::UnknownId(_)
        ));
        assert_eq!(source.fetches.load(Ordering::SeqCst), 0);
        assert!(!root.exists());
        assert!(artifact_path(&root, "x", "y").is_err());
    }

    #[tokio::test]
    async fn failed_download_preserves_pointer() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &good_source(), "fixture").await.unwrap();
        let pointer = dir.path().join("fixture/current.json");
        let before = std::fs::read(&pointer).unwrap();
        // Different (unreachable) revision with a failing source.
        let files = fixture_catalog()[0].files;
        let other: &'static [Artifact] = Box::leak(Box::new([Artifact {
            revision: "rev2",
            ..fixture_catalog()[0]
        }]));
        let _ = files;
        let failing = FixtureSource::new(&[("a.bin", A)]);
        assert!(
            install_with(dir.path(), None, other, &failing, "fixture", &|_| {})
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&pointer).unwrap(), before);
        assert!(partials(dir.path()).is_empty());
        assert!(!dir.path().join("fixture/rev2").exists());
    }

    #[tokio::test]
    async fn malicious_names_and_pointers_rejected() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &good_source(), "fixture").await.unwrap();
        let cat = fixture_catalog();
        for bad in [
            "../a.bin",
            "/etc/passwd",
            "sub/a.bin",
            "..",
            "",
            "c.bin",
            "a\\b",
        ] {
            assert!(
                artifact_path_in(dir.path(), cat, "fixture", bad).is_err(),
                "{bad}"
            );
        }
        let pointer = dir.path().join("fixture/current.json");
        let good = std::fs::read_to_string(&pointer).unwrap();
        let sha_a = sha(A);
        for tampered in [
            good.replace("rev1", "../evil"),
            good.replace(&sha_a, &sha(b"other")),
            good.replace("\"MIT\"", "\"GPL\""),
            good.replace("sub/a.bin", "sub/x.bin"),
            good.replace("\"size\":4", "\"size\":5"),
            "not json".to_string(),
        ] {
            std::fs::write(&pointer, tampered).unwrap();
            assert!(artifact_path_in(dir.path(), cat, "fixture", "a.bin").is_err());
        }
        std::fs::write(&pointer, good).unwrap();
        assert!(artifact_path_in(dir.path(), cat, "fixture", "a.bin").is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_rejected() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        // Artifact directory symlink.
        symlink(outside.path(), dir.path().join("fixture")).unwrap();
        assert!(run(dir.path(), &good_source(), "fixture").await.is_err());
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
        std::fs::remove_file(dir.path().join("fixture")).unwrap();

        run(dir.path(), &good_source(), "fixture").await.unwrap();
        let cat = fixture_catalog();
        // File symlink.
        let file = dir.path().join("fixture/rev1/a.bin");
        let real = outside.path().join("a.bin");
        std::fs::write(&real, A).unwrap();
        std::fs::remove_file(&file).unwrap();
        symlink(&real, &file).unwrap();
        assert!(artifact_path_in(dir.path(), cat, "fixture", "a.bin").is_err());
        // Pointer symlink.
        let pointer = dir.path().join("fixture/current.json");
        let copy = outside.path().join("current.json");
        std::fs::rename(&pointer, &copy).unwrap();
        symlink(&copy, &pointer).unwrap();
        assert!(artifact_path_in(dir.path(), cat, "fixture", "b.txt").is_err());
        // Revision directory symlink.
        std::fs::remove_file(&pointer).unwrap();
        std::fs::rename(&copy, &pointer).unwrap();
        let revision = dir.path().join("fixture/rev1");
        let moved = outside.path().join("rev1");
        std::fs::rename(&revision, &moved).unwrap();
        symlink(&moved, &revision).unwrap();
        assert!(artifact_path_in(dir.path(), cat, "fixture", "b.txt").is_err());
    }

    #[tokio::test]
    async fn status_schema_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let before = status(dir.path());
        assert!(before.packages.is_empty());
        assert_eq!(before.artifacts.len(), 4);
        assert!(before.artifacts.iter().all(|a| !a.installed));
        let json = serde_json::to_value(&before).unwrap();
        assert_eq!(json["packages"], serde_json::json!({}));
        let first = &json["artifacts"][0];
        assert_eq!(first["id"], "whisper-turbo");
        assert_eq!(first["installed"], false);
        assert_eq!(first["license"], "MIT");
        run(dir.path(), &good_source(), "fixture").await.unwrap();
        let after = status_in(dir.path(), fixture_catalog());
        assert!(after.artifacts[0].installed);
    }

    #[test]
    fn public_api_is_allowlisted() {
        let ids: Vec<_> = catalog().iter().map(|a| a.id).collect();
        assert_eq!(
            ids,
            [
                "whisper-turbo",
                "whisper-small",
                "silero-vad",
                "kokoro-multi-lang-v1_0"
            ]
        );
        assert_eq!(DEFAULT_ASR_ID, "whisper-turbo");
        assert!(lookup("python -c 'x'").is_none());
        let progress = Progress {
            artifact: "x".into(),
            downloaded: 1,
            total: 2,
        };
        let round: Progress =
            serde_json::from_value(serde_json::to_value(&progress).unwrap()).unwrap();
        assert_eq!(round, progress);
    }

    #[test]
    fn catalog_pins_match_local_manifest() {
        let expected: &[(&str, &str, &str, &str, u64, &str)] = &[
            (
                "whisper-turbo",
                "2ca6ff69fc878651b770880507669577ac41c2ff",
                "turbo-encoder.int8.onnx",
                "b02dcdf54f348741e93fe732b67d933c8dcb6735655f710640143081db38878b",
                674716297,
                "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-turbo/resolve/2ca6ff69fc878651b770880507669577ac41c2ff/turbo-encoder.int8.onnx",
            ),
            (
                "whisper-turbo",
                "2ca6ff69fc878651b770880507669577ac41c2ff",
                "turbo-decoder.int8.onnx",
                "20accd02388482eb3a46bd615631adfdc85e1eb2c7db9ea3f02a40ffe6b81547",
                361080764,
                "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-turbo/resolve/2ca6ff69fc878651b770880507669577ac41c2ff/turbo-decoder.int8.onnx",
            ),
            (
                "whisper-turbo",
                "2ca6ff69fc878651b770880507669577ac41c2ff",
                "turbo-tokens.txt",
                "b34b360dbb493e781e479794586d661700670d65564001f23024971d1f2fa126",
                816730,
                "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-turbo/resolve/2ca6ff69fc878651b770880507669577ac41c2ff/turbo-tokens.txt",
            ),
            (
                "whisper-small",
                "8f3c18b358db4d1f2fc1eae49d75cd20989e4309",
                "small-encoder.int8.onnx",
                "4cbe7b22fa9026b843b60a68640c747de05bafb1a11b57edc0e66c232d9f33a9",
                112442483,
                "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-small/resolve/8f3c18b358db4d1f2fc1eae49d75cd20989e4309/small-encoder.int8.onnx",
            ),
            (
                "whisper-small",
                "8f3c18b358db4d1f2fc1eae49d75cd20989e4309",
                "small-decoder.int8.onnx",
                "acad50b5c782696e91b55914cc5ab4f756f1532f76e22aa6fc615f39fb69a8ee",
                262226114,
                "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-small/resolve/8f3c18b358db4d1f2fc1eae49d75cd20989e4309/small-decoder.int8.onnx",
            ),
            (
                "whisper-small",
                "8f3c18b358db4d1f2fc1eae49d75cd20989e4309",
                "small-tokens.txt",
                "b34b360dbb493e781e479794586d661700670d65564001f23024971d1f2fa126",
                816730,
                "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-small/resolve/8f3c18b358db4d1f2fc1eae49d75cd20989e4309/small-tokens.txt",
            ),
            (
                "silero-vad",
                "5cd2ba54db059f961e7545d4f211b44f005a6dfb",
                "silero_vad.onnx",
                "6b99cbfd39246b6706f98ec13c7c50c6b299181f2474fa05cbc8046acc274396",
                2313101,
                "https://raw.githubusercontent.com/snakers4/silero-vad/5cd2ba54db059f961e7545d4f211b44f005a6dfb/files/silero_vad.onnx",
            ),
            (
                "kokoro-multi-lang-v1_0",
                "f7b96bb6bef5c5da4d3aa4f4e0498fbbf62dc78b",
                "model.onnx",
                "b40f62b166ac8164b0627ef48a0b358eda0985e272fb03ef5252e7206305da11",
                325560556,
                "https://huggingface.co/csukuangfj/kokoro-multi-lang-v1_0/resolve/f7b96bb6bef5c5da4d3aa4f4e0498fbbf62dc78b/model.onnx",
            ),
            (
                "kokoro-multi-lang-v1_0",
                "f7b96bb6bef5c5da4d3aa4f4e0498fbbf62dc78b",
                "voices.bin",
                "1c5a5b983d3d50d8586d437a51f3faa2da7919ce76a013c081e65671a3447c29",
                28200960,
                "https://huggingface.co/csukuangfj/kokoro-multi-lang-v1_0/resolve/f7b96bb6bef5c5da4d3aa4f4e0498fbbf62dc78b/voices.bin",
            ),
            (
                "kokoro-multi-lang-v1_0",
                "f7b96bb6bef5c5da4d3aa4f4e0498fbbf62dc78b",
                "tokens.txt",
                "6ebb6bb288f20f3ae8d004d3c2ca27697da27c037d75e81a60e2a6a663f95425",
                687,
                "https://huggingface.co/csukuangfj/kokoro-multi-lang-v1_0/resolve/f7b96bb6bef5c5da4d3aa4f4e0498fbbf62dc78b/tokens.txt",
            ),
            (
                "kokoro-multi-lang-v1_0",
                "f7b96bb6bef5c5da4d3aa4f4e0498fbbf62dc78b",
                "LICENSE",
                "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30",
                11358,
                "https://huggingface.co/csukuangfj/kokoro-multi-lang-v1_0/resolve/f7b96bb6bef5c5da4d3aa4f4e0498fbbf62dc78b/LICENSE",
            ),
        ];
        let actual: Vec<_> = catalog()
            .iter()
            .flat_map(|a| {
                a.files
                    .iter()
                    .map(move |f| (a.id, a.revision, f.name, f.sha256, f.size, f.url))
            })
            .collect();
        assert_eq!(actual.len(), 11);
        assert_eq!(actual, expected);
        for file in catalog().iter().flat_map(|a| a.files) {
            assert!(file.url.starts_with("https://"));
            assert_eq!(file.sha256.len(), 64);
            assert!(file.size <= MAX_FILE_BYTES);
        }
        let kokoro = lookup("kokoro-multi-lang-v1_0").unwrap();
        assert_eq!(kokoro.license, "Apache-2.0");
        assert_eq!((kokoro.voice, kokoro.speaker), (Some("pf_dora"), Some(42)));
        assert_eq!(lookup("whisper-turbo").unwrap().threads, Some(8));
        assert_eq!(lookup("whisper-small").unwrap().threads, Some(4));
        assert_eq!(lookup("silero-vad").unwrap().license, "MIT");
    }

    struct HangSource;

    #[async_trait::async_trait]
    impl Source for HangSource {
        async fn open(&self, _url: &str) -> Result<ByteStream, ModelError> {
            let first = futures_util::stream::iter(vec![Ok(b"alp".to_vec())]);
            let hang = futures_util::stream::pending::<Result<Vec<u8>, ModelError>>();
            Ok(Box::pin(first.chain(hang)))
        }
    }

    fn write_local(local: &Path, a: &[u8], b: &[u8]) {
        std::fs::create_dir_all(local.join("fixture")).unwrap();
        std::fs::write(local.join("fixture/a.bin"), a).unwrap();
        std::fs::write(local.join("fixture/b.txt"), b).unwrap();
    }

    async fn run_local(
        root: &Path,
        local: &Path,
        source: &dyn Source,
        events: &Mutex<Vec<Report>>,
    ) -> Result<InstallManifest, ModelError> {
        install_with(
            root,
            Some(local),
            fixture_catalog(),
            source,
            "fixture",
            &|p| events.lock().unwrap().push(p),
        )
        .await
    }

    #[tokio::test]
    async fn verified_local_files_avoid_the_network() {
        let dir = tempfile::tempdir().unwrap();
        let local = tempfile::tempdir().unwrap();
        write_local(local.path(), A, B);
        let source = FixtureSource::new(&[]);
        let events = Mutex::new(Vec::new());
        run_local(dir.path(), local.path(), &source, &events)
            .await
            .unwrap();
        assert_eq!(source.fetches.load(Ordering::SeqCst), 0);
        let path = artifact_path_in(dir.path(), fixture_catalog(), "fixture", "b.txt").unwrap();
        assert_eq!(std::fs::read(path).unwrap(), B);
        assert!(partials(dir.path()).is_empty());
        let events = events.into_inner().unwrap();
        assert!(!events.is_empty() && events.iter().all(|e| e.phase == "copying"));
        // The local files are copied, never moved or linked.
        assert!(local.path().join("fixture/a.bin").exists());
    }

    #[tokio::test]
    async fn corrupt_or_missing_local_files_fall_back_to_pinned_urls() {
        let dir = tempfile::tempdir().unwrap();
        let local = tempfile::tempdir().unwrap();
        // Same size, different bytes; the second file is absent.
        write_local(local.path(), b"alpha-bytez", B);
        std::fs::remove_file(local.path().join("fixture/b.txt")).unwrap();
        let source = good_source();
        let events = Mutex::new(Vec::new());
        run_local(dir.path(), local.path(), &source, &events)
            .await
            .unwrap();
        assert_eq!(source.fetches.load(Ordering::SeqCst), 2);
        let path = artifact_path_in(dir.path(), fixture_catalog(), "fixture", "a.bin").unwrap();
        assert_eq!(std::fs::read(path).unwrap(), A);
        assert!(partials(dir.path()).is_empty());

        // A truncated local file is also rejected; the intact one is reused.
        let dir = tempfile::tempdir().unwrap();
        write_local(local.path(), b"alpha", B);
        let source = good_source();
        run_local(dir.path(), local.path(), &source, &events)
            .await
            .unwrap();
        assert_eq!(source.fetches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn corrupt_local_file_never_published_when_network_also_fails() {
        let dir = tempfile::tempdir().unwrap();
        let local = tempfile::tempdir().unwrap();
        write_local(local.path(), b"alpha-bytez", B);
        let source = FixtureSource::new(&[]);
        let events = Mutex::new(Vec::new());
        assert!(
            run_local(dir.path(), local.path(), &source, &events)
                .await
                .is_err()
        );
        assert!(!dir.path().join("fixture/current.json").exists());
        assert!(!dir.path().join("fixture/rev1").exists());
        assert!(partials(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn verified_installed_revision_is_reused_and_pointer_repaired() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &good_source(), "fixture").await.unwrap();
        std::fs::remove_file(dir.path().join("fixture/current.json")).unwrap();
        assert!(artifact_path_in(dir.path(), fixture_catalog(), "fixture", "a.bin").is_err());
        let offline = FixtureSource::new(&[]);
        run(dir.path(), &offline, "fixture").await.unwrap();
        assert_eq!(offline.fetches.load(Ordering::SeqCst), 0);
        assert!(artifact_path_in(dir.path(), fixture_catalog(), "fixture", "a.bin").is_ok());
        assert!(partials(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn corrupt_installed_revision_is_replaced_with_verified_content() {
        for damage in ["corrupt", "missing"] {
            let dir = tempfile::tempdir().unwrap();
            run(dir.path(), &good_source(), "fixture").await.unwrap();
            let file = dir.path().join("fixture/rev1/a.bin");
            if damage == "corrupt" {
                std::fs::write(&file, b"alpha-bytez").unwrap();
            } else {
                std::fs::remove_file(&file).unwrap();
            }
            assert!(artifact_path_in(dir.path(), fixture_catalog(), "fixture", "a.bin").is_err());
            let source = good_source();
            run(dir.path(), &source, "fixture").await.unwrap();
            assert_eq!(source.fetches.load(Ordering::SeqCst), 2);
            let path = artifact_path_in(dir.path(), fixture_catalog(), "fixture", "a.bin").unwrap();
            assert_eq!(std::fs::read(path).unwrap(), A);
            assert!(partials(dir.path()).is_empty());
        }
    }

    #[tokio::test]
    async fn corrupt_installed_revision_with_failing_network_publishes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &good_source(), "fixture").await.unwrap();
        std::fs::write(dir.path().join("fixture/rev1/a.bin"), b"alpha-bytez").unwrap();
        assert!(
            run(dir.path(), &FixtureSource::new(&[]), "fixture")
                .await
                .is_err()
        );
        assert!(!dir.path().join("fixture/current.json").exists());
        assert!(!dir.path().join("fixture/rev1").exists());
        assert!(partials(dir.path()).is_empty());
        // A later retry succeeds.
        run(dir.path(), &good_source(), "fixture").await.unwrap();
        assert!(artifact_path_in(dir.path(), fixture_catalog(), "fixture", "a.bin").is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_revision_is_rejected_not_removed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("a.bin"), A).unwrap();
        std::fs::create_dir_all(dir.path().join("fixture")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("fixture/rev1")).unwrap();
        assert!(matches!(
            run(dir.path(), &good_source(), "fixture")
                .await
                .unwrap_err(),
            ModelError::InvalidPath(_)
        ));
        assert!(outside.path().join("a.bin").exists());
        assert!(partials(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn dropped_install_leaves_no_staging_or_publication() {
        let dir = tempfile::tempdir().unwrap();
        let future = install_with(
            dir.path(),
            None,
            fixture_catalog(),
            &HangSource,
            "fixture",
            &|_| {},
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(200), future)
                .await
                .is_err()
        );
        assert!(partials(dir.path()).is_empty());
        assert!(!dir.path().join("fixture/current.json").exists());
        assert!(!dir.path().join("fixture/rev1").exists());
        // Retry after cancellation works.
        run(dir.path(), &good_source(), "fixture").await.unwrap();
    }

    #[tokio::test]
    async fn cleanup_cancels_and_joins_blocking_workers() {
        let work = Work::new();
        let finished = Arc::new(AtomicBool::new(false));
        let flag = finished.clone();
        let future = blocking(
            &work,
            move |work, _| {
                while work.check().is_ok() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                std::thread::sleep(Duration::from_millis(50));
                flag.store(true, Ordering::SeqCst);
                Ok(())
            },
            |_, _| {},
        );
        let _ = tokio::time::timeout(Duration::from_millis(20), future).await;
        work.cancel_and_wait();
        assert!(finished.load(Ordering::SeqCst));
    }
}
