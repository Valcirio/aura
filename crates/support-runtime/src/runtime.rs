use crate::{
    AppError, Result, audio,
    config::{Paths, Settings},
    google::{Database, GoogleClient, database},
    http,
    integrations::{self, Integration, IntegrationRegistry, ToolContext},
    knowledge,
    provider::{ProbeResult, Provider, ToolCall},
    secrets::SecretStore,
    speech::{self, SpeechBackend},
};
use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use support_core::{
    calendar::GOOGLE_CALENDAR,
    memory::{Memory, Message, Session},
    store::Operation,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppEvent {
    pub session_id: i64,
    pub turn_id: String,
    pub generation: u64,
    pub sequence: u64,
    pub kind: String,
    pub payload: Value,
}
pub type EventSink = Arc<dyn Fn(AppEvent) + Send + Sync>;
#[derive(Clone)]
pub(crate) struct Emitter {
    pub(crate) session: i64,
    pub(crate) turn: String,
    pub(crate) generation: u64,
    pub(crate) sequence: Arc<AtomicU64>,
    pub(crate) sink: EventSink,
    pub(crate) cancel: CancellationToken,
    pub(crate) backend: Arc<SpeechBackend>,
    pub(crate) current: Arc<AtomicU64>,
    pub(crate) generated: Arc<Mutex<String>>,
    pub(crate) played: Arc<Mutex<String>>,
}
impl Emitter {
    pub(crate) fn send(&self, kind: &str, payload: Value) {
        if self.current.load(Ordering::SeqCst) != self.generation {
            return;
        }
        if !self.cancel.is_cancelled() || kind == "turn.cancelled" {
            if kind == "assistant.delta" {
                if let Some(text) = payload["text"].as_str() {
                    if let Ok(mut generated) = self.generated.lock() {
                        generated.push_str(text);
                    }
                }
            }
            (self.sink)(AppEvent {
                session_id: self.session,
                turn_id: self.turn.clone(),
                generation: self.generation,
                sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
                kind: kind.into(),
                payload,
            });
        }
    }
    pub(crate) fn state(&self, name: &str) {
        self.send("state.changed", json!({"state":name}));
    }
    pub(crate) fn level(&self, level: f32, source: &str) {
        self.send("audio.level", json!({"rms":level,"source":source}));
    }
    pub(crate) fn feedback(&self, reason: &str) {
        self.send("input.feedback", json!({"reason":reason}));
    }
    pub(crate) fn turn(&self) -> Self {
        Self {
            turn: uuid::Uuid::new_v4().simple().to_string(),
            generated: Arc::new(Mutex::new(String::new())),
            played: Arc::new(Mutex::new(String::new())),
            ..self.clone()
        }
    }
    pub(crate) fn trace(
        &self,
        stage: &str,
        messages: Option<usize>,
        deltas: Option<usize>,
        code: Option<&str>,
    ) {
        if !trace_turns_enabled() {
            return;
        }
        eprintln!(
            "{}",
            trace_line(
                stage,
                &TurnTrace {
                    session: self.session,
                    turn: &self.turn,
                    generation: self.generation,
                    sequence: self.sequence.load(Ordering::Relaxed),
                    messages,
                    deltas,
                    code,
                },
            )
        );
    }
}

pub(crate) const TRACE_TURNS_ENV: &str = "AURA_TRACE_TURNS";

fn capture_feedback(error: &AppError) -> Option<&'static str> {
    (error.code == "EMPTY_SPEECH").then_some("empty_speech")
}

fn transcript_feedback(text: &str) -> Option<&'static str> {
    text.trim().is_empty().then_some("empty_transcript")
}

pub(crate) struct TurnTrace<'a> {
    pub(crate) session: i64,
    pub(crate) turn: &'a str,
    pub(crate) generation: u64,
    pub(crate) sequence: u64,
    pub(crate) messages: Option<usize>,
    pub(crate) deltas: Option<usize>,
    pub(crate) code: Option<&'a str>,
}

pub(crate) fn trace_turns_enabled() -> bool {
    std::env::var(TRACE_TURNS_ENV).as_deref() == Ok("1")
}

pub(crate) fn trace_line(stage: &str, trace: &TurnTrace<'_>) -> String {
    let mut line = format!(
        "[turn-trace] stage={} session={} turn={} generation={} sequence={}",
        stage, trace.session, trace.turn, trace.generation, trace.sequence
    );
    if let Some(messages) = trace.messages {
        line.push_str(&format!(" messages={messages}"));
    }
    if let Some(deltas) = trace.deltas {
        line.push_str(&format!(" deltas={deltas}"));
    }
    if let Some(code) = trace.code {
        line.push_str(&format!(" code={code}"));
    }
    line
}
#[derive(Debug, Clone, Serialize)]
pub struct RunTicket {
    pub session_id: i64,
    pub turn_id: String,
    pub generation: u64,
}
struct ActiveRun {
    cancel: CancellationToken,
    finish: Arc<AtomicBool>,
    session: i64,
    backend: Arc<SpeechBackend>,
    generation: u64,
}
struct SpeechState {
    settings: Settings,
    backend: Arc<SpeechBackend>,
}

/// Cached view of default-speech preparation. Reading it never touches the
/// filesystem, the network or the models.
#[derive(Debug, Clone, Serialize)]
struct SpeechSnapshot {
    /// `idle`, `starting`, `verifying`, `copying`, `downloading`, `loading`,
    /// `ready`, `failed` or `unsupported`.
    phase: String,
    ready: bool,
    /// A background job is still running (possibly the optional Turbo install).
    active: bool,
    artifact: Option<String>,
    file: Option<String>,
    downloaded: u64,
    total: u64,
    error: Option<String>,
    code: Option<String>,
    warning: Option<String>,
    generation: u64,
}
impl SpeechSnapshot {
    fn idle(generation: u64) -> Self {
        Self {
            phase: "idle".into(),
            ready: false,
            active: false,
            artifact: None,
            file: None,
            downloaded: 0,
            total: 0,
            error: None,
            code: None,
            warning: None,
            generation,
        }
    }
    fn clear_progress(&mut self) {
        self.artifact = None;
        self.file = None;
        self.downloaded = 0;
        self.total = 0;
    }
}

/// Provisioning operations of the native speech models; replaceable in tests.
type InstallFn = Arc<
    dyn Fn(String, Arc<dyn Fn(Value) + Send + Sync>) -> BoxFuture<'static, Result<Value>>
        + Send
        + Sync,
>;
type PreloadFn = Arc<dyn Fn(Arc<SpeechBackend>) -> Result<()> + Send + Sync>;
pub(crate) struct SpeechHooks {
    supported: bool,
    install: InstallFn,
    preload: PreloadFn,
}
impl SpeechHooks {
    #[cfg(feature = "local-speech")]
    fn native(paths: Paths) -> Self {
        Self {
            supported: true,
            install: Arc::new(move |id, progress| {
                let root = paths.models();
                let local = paths.model_source_root();
                async move {
                    let manifest =
                        crate::models::install_managed(&root, local.as_deref(), &id, |report| {
                            progress(serde_json::to_value(&report).unwrap_or(Value::Null))
                        })
                        .await
                        .map_err(model_app_error)?;
                    Ok(serde_json::to_value(manifest)?)
                }
                .boxed()
            }),
            preload: Arc::new(|backend| {
                backend.preload_voice()?;
                backend.preload_tts()
            }),
        }
    }
    #[cfg(not(feature = "local-speech"))]
    fn native(_paths: Paths) -> Self {
        Self {
            supported: false,
            install: Arc::new(|_, _| async { Err(speech_unsupported()) }.boxed()),
            preload: Arc::new(|_| Err(speech_unsupported())),
        }
    }
}
fn speech_unsupported() -> AppError {
    AppError::new(
        "SPEECH_UNSUPPORTED",
        "Esta versão não inclui o áudio local nativo; modelos não podem ser preparados.",
    )
}
#[cfg(feature = "local-speech")]
fn model_app_error(error: crate::models::ModelError) -> AppError {
    use crate::models::ModelError as E;
    let code = match &error {
        E::Network(_) => "MODEL_NETWORK",
        E::TooLarge(_) | E::Integrity(_) => "MODEL_INTEGRITY",
        E::Io(_) => "MODEL_IO",
        E::NotInstalled(_) => "MODEL_NOT_INSTALLED",
        E::UnknownId(_) => "INVALID_INPUT",
        E::InvalidPath(_) => "MODEL_INVALID",
    };
    AppError::new(code, error.to_string())
}
/// Catalog id of the ASR artifact for a settings value.
fn asr_artifact(asr_model: &str) -> &'static str {
    if asr_model == "small" {
        "whisper-small"
    } else {
        "whisper-turbo"
    }
}
const VAD_ARTIFACT: &str = "silero-vad";
/// Fixed prerecorded pt-BR notices embedded in the binary. The only thing a caller can pick
/// is the variant; there is no text, path or audio input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    Provider,
    Models,
    Capture,
    Synthesis,
}
const NOTICE_SAMPLE_RATE: u32 = 24_000;
impl NoticeKind {
    fn wav(self) -> &'static [u8] {
        match self {
            Self::Provider => include_bytes!("../assets/notices/provider.wav"),
            Self::Models => include_bytes!("../assets/notices/models.wav"),
            Self::Capture => include_bytes!("../assets/notices/capture.wav"),
            Self::Synthesis => include_bytes!("../assets/notices/synthesis.wav"),
        }
    }
}

pub(crate) type PlayFn = Arc<
    dyn Fn(speech::Pcm, Option<&str>, &CancellationToken, &mut dyn FnMut(f32)) -> Result<()>
        + Send
        + Sync,
>;
/// Serializes every native playback (speech, preview, notices) so CPAL outputs never overlap.
/// Waiting for the turn and the playback itself both observe cancellation. Only call it from
/// blocking threads.
pub(crate) struct Playback {
    busy: std::sync::Mutex<bool>,
    freed: std::sync::Condvar,
    player: PlayFn,
}
struct Turn<'a>(&'a Playback);
impl Drop for Turn<'_> {
    fn drop(&mut self) {
        *self.0.busy.lock().unwrap_or_else(|e| e.into_inner()) = false;
        self.0.freed.notify_all();
    }
}
impl Playback {
    fn native() -> Self {
        Self::with_player(Arc::new(|pcm, output, cancel, level| {
            audio::play(pcm, output, cancel, level)
        }))
    }
    pub(crate) fn with_player(player: PlayFn) -> Self {
        Self {
            busy: std::sync::Mutex::new(false),
            freed: std::sync::Condvar::new(),
            player,
        }
    }
    pub(crate) fn play(
        &self,
        pcm: speech::Pcm,
        output: Option<&str>,
        cancel: &CancellationToken,
        level: &mut dyn FnMut(f32),
    ) -> Result<()> {
        let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if cancel.is_cancelled() {
                return Err(AppError::cancelled());
            }
            if !*busy {
                break;
            }
            busy = self
                .freed
                .wait_timeout(busy, Duration::from_millis(10))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        *busy = true;
        drop(busy);
        let _turn = Turn(self);
        (self.player)(pcm, output, cancel, level)
    }
}

const TTS_ARTIFACT: &str = "kokoro-multi-lang-v1_0";
const KNOWN_ARTIFACTS: [&str; 4] = ["whisper-turbo", "whisper-small", VAD_ARTIFACT, TTS_ARTIFACT];

/// In-flight preparation job; at most one exists at a time.
struct SpeechJob {
    id: u64,
    generation: u64,
    abort: tokio::task::AbortHandle,
}

/// Upper bound for one complete provider validation (all probe requests together).
const PROVIDER_CHECK_TIMEOUT: Duration = Duration::from_secs(60);
type SharedCheck = Shared<BoxFuture<'static, Result<ProbeResult>>>;
struct InFlightCheck {
    id: u64,
    revision: u64,
    future: SharedCheck,
}

pub struct Runtime {
    pub db: Database,
    pub google: Arc<GoogleClient>,
    pub integrations: IntegrationRegistry,
    pub vault: Arc<SecretStore>,
    pub http: reqwest::Client,
    paths: Paths,
    state: Mutex<SpeechState>,
    pub(crate) speech_hooks: SpeechHooks,
    speech_status: Mutex<SpeechSnapshot>,
    /// Incremented whenever the selected ASR/backend changes.
    speech_gen: AtomicU64,
    speech_job: Mutex<Option<SpeechJob>>,
    speech_job_ids: AtomicU64,
    artifact_locks: Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    settings_commit: Mutex<()>,
    /// Serializes whole settings saves, including credential writes, with each other.
    save_serial: tokio::sync::Mutex<()>,
    /// Incremented whenever the provider configuration or its credential may have changed.
    provider_revision: AtomicU64,
    provider_check: Mutex<Option<InFlightCheck>>,
    check_ids: AtomicU64,
    active: Mutex<Option<ActiveRun>>,
    generation: Arc<AtomicU64>,
    audio_serial: tokio::sync::Mutex<()>,
    pub(crate) playback: Arc<Playback>,
    notice: Mutex<Option<(u64, CancellationToken)>>,
    notice_ids: AtomicU64,
    pub(crate) service_serial: tokio::sync::Mutex<()>,
}
impl Runtime {
    pub fn open(paths: Paths) -> Result<Arc<Self>> {
        std::fs::create_dir_all(&paths.data)?;
        let memory = Memory::open(paths.data.join("memory.db"))?;
        memory.recover_operations()?;
        let mut settings: Settings = memory
            .get_json("settings")?
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();
        settings.migrate_loaded();
        settings.validate()?;
        let backend = Arc::new(speech::backend(paths.clone(), &settings.asr_model));
        let db = Arc::new(Mutex::new(memory));
        let http = http::client()?;
        let vault = Arc::new(SecretStore::default());
        knowledge::seed(&db)?;
        let google = Arc::new(GoogleClient::new(http.clone(), vault.clone(), db.clone()));
        let mut integrations = IntegrationRegistry::default();
        integrations.register(Arc::new(integrations::MemoryIntegration))?;
        integrations.register(Arc::new(integrations::KnowledgeIntegration))?;
        integrations.register(Arc::new(integrations::WebIntegration))?;
        integrations.register(Arc::new(integrations::CalendarIntegration {
            client: google.clone(),
        }))?;
        Ok(Arc::new(Self {
            google,
            integrations,
            db,
            http,
            vault,
            paths: paths.clone(),
            state: Mutex::new(SpeechState { settings, backend }),
            speech_hooks: SpeechHooks::native(paths.clone()),
            speech_status: Mutex::new(SpeechSnapshot::idle(0)),
            speech_gen: AtomicU64::new(0),
            speech_job: Mutex::new(None),
            speech_job_ids: AtomicU64::new(0),
            artifact_locks: Mutex::new(std::collections::HashMap::new()),
            settings_commit: Mutex::new(()),
            save_serial: tokio::sync::Mutex::new(()),
            provider_revision: AtomicU64::new(0),
            provider_check: Mutex::new(None),
            check_ids: AtomicU64::new(0),
            active: Mutex::new(None),
            generation: Arc::new(AtomicU64::new(0)),
            audio_serial: tokio::sync::Mutex::new(()),
            playback: Arc::new(Playback::native()),
            notice: Mutex::new(None),
            notice_ids: AtomicU64::new(0),
            service_serial: tokio::sync::Mutex::new(()),
        }))
    }
    pub fn register_integration(&mut self, integration: Arc<dyn Integration>) -> Result<()> {
        self.integrations.register(integration)
    }
    pub fn settings(&self) -> Result<Settings> {
        self.state
            .lock()
            .map(|v| v.settings.clone())
            .map_err(|_| AppError::new("SETTINGS_UNAVAILABLE", "Configurações indisponíveis"))
    }
    pub async fn save_settings(
        &self,
        settings: Settings,
        provider_key: Option<String>,
        brave_key: Option<String>,
        google_secret: Option<String>,
    ) -> Result<Value> {
        settings.validate()?;
        for id in &settings.integrations_disabled {
            self.integrations.get(id)?;
        }
        let _serial = self.save_serial.lock().await;
        let provider_credential_changed = provider_key.is_some();
        if provider_credential_changed {
            // Any check running from now on must not publish for the previous credential.
            self.invalidate_provider()?;
        }
        let mut persistent = true;
        let written: Result<()> = async {
            for (key, value) in [
                (settings.provider.secret_key(), provider_key),
                ("brave".into(), brave_key),
                ("google_client_secret".into(), google_secret),
            ] {
                if let Some(value) = value {
                    if value.is_empty() {
                        self.vault.remove(&key).await?;
                    } else {
                        persistent &= self.vault.set(&key, &value).await?;
                    }
                }
            }
            Ok(())
        }
        .await;
        if provider_credential_changed {
            // Also on partial failure: the credential may have changed anyway.
            let invalidated = self.invalidate_provider();
            written?;
            invalidated?;
        } else {
            written?;
        }
        let unavailable = || AppError::new("SETTINGS_UNAVAILABLE", "Configurações indisponíveis");
        // Serialized, synchronous commit: no await while any guard below is held.
        let _commit = self.settings_commit.lock().map_err(|_| unavailable())?;
        let mut active = self.active.lock().map_err(|_| unavailable())?;
        let mut state = self.state.lock().map_err(|_| unavailable())?;
        let db = database(&self.db)?;
        let value = serde_json::to_value(&settings)?;
        if state.settings.provider != settings.provider || provider_credential_changed {
            db.set_json_many(&[("settings", &value), ("provider_probe", &Value::Null)])?;
            self.provider_revision.fetch_add(1, Ordering::SeqCst);
        } else {
            db.set_json_many(&[("settings", &value)])?;
        }
        drop(db);
        let mut replaced = None;
        if state.settings.asr_model != settings.asr_model {
            self.cancel_locked(&mut active, true);
            replaced = Some(std::mem::replace(
                &mut state.backend,
                Arc::new(speech::backend(self.paths.clone(), &settings.asr_model)),
            ));
            // Readiness of the previous backend must never carry over.
            let generation = self.speech_gen.fetch_add(1, Ordering::SeqCst) + 1;
            if let Ok(mut snapshot) = self.speech_status.lock() {
                *snapshot = SpeechSnapshot::idle(generation);
            }
        }
        state.settings = settings;
        drop(state);
        drop(active);
        if let Some(previous) = replaced {
            previous.cancel();
            self.abort_speech_job();
        }
        Ok(json!({"saved":true,"persistent_credentials":persistent}))
    }
    pub(crate) async fn provider(&self, require_probe: bool) -> Result<Provider> {
        let settings = self.settings()?;
        settings.provider.validate()?;
        let key = self.vault.get(&settings.provider.secret_key()).await?;
        let provider = Provider {
            client: self.http.clone(),
            config: settings.provider,
            key,
        };
        if require_probe {
            let probe = database(&self.db)?
                .get_json("provider_probe")?
                .filter(|v| !v.is_null())
                .ok_or_else(|| {
                    AppError::new(
                        "MODEL_NOT_VERIFIED",
                        "Teste a compatibilidade do modelo em Configurações",
                    )
                })?;
            let probe: ProbeResult = serde_json::from_value(probe)?;
            if probe.config != provider.config
                || !probe.tools
                || !probe.streaming
                || !probe.portuguese
                || !probe.cancellation
                || probe.fingerprint != provider.fingerprint().await?
            {
                return Err(AppError::new(
                    "MODEL_NOT_VERIFIED",
                    "O modelo ou sua configuração mudou. Execute o teste de compatibilidade novamente.",
                ));
            }
        }
        Ok(provider)
    }
    pub async fn provider_models(&self) -> Result<Vec<String>> {
        self.provider(false).await?.models().await
    }
    /// Bumps the provider revision and drops the stored probe, so no in-flight check can publish.
    fn invalidate_provider(&self) -> Result<()> {
        let _commit = self
            .settings_commit
            .lock()
            .map_err(|_| AppError::new("SETTINGS_UNAVAILABLE", "Configurações indisponíveis"))?;
        self.provider_revision.fetch_add(1, Ordering::SeqCst);
        database(&self.db)?.set_json("provider_probe", &Value::Null)?;
        Ok(())
    }
    /// Confirms, synchronized with settings commits, that `revision` is still current and
    /// optionally persists the probe.
    fn publish_probe(&self, revision: u64, result: Option<&ProbeResult>) -> Result<()> {
        let _commit = self
            .settings_commit
            .lock()
            .map_err(|_| AppError::new("SETTINGS_UNAVAILABLE", "Configurações indisponíveis"))?;
        let changed = || {
            AppError::new(
                "SETTINGS_CHANGED",
                "Configuração mudou durante o teste; teste novamente",
            )
        };
        if self.provider_revision.load(Ordering::SeqCst) != revision {
            return Err(changed());
        }
        if let Some(result) = result {
            if self.settings()?.provider != result.config {
                return Err(changed());
            }
            database(&self.db)?.set_json("provider_probe", &serde_json::to_value(result)?)?;
        }
        Ok(())
    }
    /// Returns the stored probe only if every `provider(true)` safeguard still passes.
    async fn verified_probe(&self) -> Result<ProbeResult> {
        self.provider(true).await?;
        let stored = database(&self.db)?
            .get_json("provider_probe")?
            .filter(|v| !v.is_null())
            .ok_or_else(|| {
                AppError::new(
                    "MODEL_NOT_VERIFIED",
                    "Teste a compatibilidade do modelo em Configurações",
                )
            })?;
        Ok(serde_json::from_value(stored)?)
    }
    async fn check_operation(&self, revision: u64) -> Result<ProbeResult> {
        if let Ok(probe) = self.verified_probe().await {
            self.publish_probe(revision, None)?;
            return Ok(probe);
        }
        let provider = self.provider(false).await?;
        let result = provider.probe().await?;
        self.publish_probe(revision, Some(&result))?;
        Ok(result)
    }
    /// Validates the configured provider: reuses a still-valid stored probe, otherwise runs the
    /// full probe. Concurrent callers for the same revision share one bounded operation.
    pub async fn provider_check(self: &Arc<Self>) -> Result<ProbeResult> {
        self.check_with(PROVIDER_CHECK_TIMEOUT, |runtime, revision| {
            async move { runtime.check_operation(revision).await }.boxed()
        })
        .await
    }
    async fn check_with<F>(self: &Arc<Self>, limit: Duration, operation: F) -> Result<ProbeResult>
    where
        F: FnOnce(Arc<Runtime>, u64) -> BoxFuture<'static, Result<ProbeResult>>,
    {
        let future = {
            let mut slot = self.provider_check.lock().map_err(|_| {
                AppError::new("SETTINGS_UNAVAILABLE", "Configurações indisponíveis")
            })?;
            let revision = self.provider_revision.load(Ordering::SeqCst);
            if let Some(running) = slot.as_ref().filter(|c| c.revision == revision) {
                running.future.clone()
            } else {
                let id = self.check_ids.fetch_add(1, Ordering::SeqCst) + 1;
                let runtime = self.clone();
                let work = operation(self.clone(), revision);
                let future: SharedCheck = async move {
                    let result = match tokio::time::timeout(limit, work).await {
                        Ok(result) => result,
                        Err(_) => {
                            let mut error = AppError::new(
                                "PROVIDER_TIMEOUT",
                                format!(
                                    "A verificação do provedor excedeu {} segundos. Confirme que o provedor está em execução e que o modelo escolhido está disponível, depois tente novamente.",
                                    limit.as_secs()
                                ),
                            );
                            error.retryable = true;
                            Err(error)
                        }
                    };
                    if let Ok(mut slot) = runtime.provider_check.lock() {
                        if slot.as_ref().is_some_and(|c| c.id == id) {
                            *slot = None;
                        }
                    }
                    result
                }
                .boxed()
                .shared();
                *slot = Some(InFlightCheck {
                    id,
                    revision,
                    future: future.clone(),
                });
                future
            }
        };
        future.await
    }
    pub async fn provider_probe(self: &Arc<Self>) -> Result<ProbeResult> {
        self.provider_check().await
    }
    pub fn probe_status(&self) -> Result<Value> {
        Ok(database(&self.db)?
            .get_json("provider_probe")?
            .unwrap_or(Value::Null))
    }
    pub fn session_create(&self, title: &str) -> Result<Session> {
        if title.trim().is_empty() || title.len() > 240 {
            return Err(AppError::invalid("Informe um título de até 240 bytes"));
        }
        let db = database(&self.db)?;
        let session = db.create_session(title)?;
        let account = db.connection_info(GOOGLE_CALENDAR)?.map(|c| c.account_id);
        db.attach_service(session.id, GOOGLE_CALENDAR, account.as_deref())?;
        Ok(session)
    }
    pub fn session_list(&self, before: Option<i64>) -> Result<Vec<Session>> {
        database(&self.db)?.sessions_page(before, 30)
    }
    pub fn session_messages(&self, id: i64, after: Option<i64>) -> Result<Vec<Message>> {
        database(&self.db)?.messages_page(id, after, 30)
    }
    pub fn session_delete(&self, id: i64) -> Result<bool> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| AppError::new("CONVERSATION_BUSY", "Conversa indisponível"))?;
        if active.as_ref().is_some_and(|a| a.session == id) {
            self.cancel_locked(&mut active, true);
        }
        let db = database(&self.db)?;
        if db
            .pending_operation(id)?
            .is_some_and(|op| op.status == "executing")
        {
            return Err(AppError::new(
                "ACTION_IN_PROGRESS",
                "Espere a criação em andamento terminar antes de excluir a conversa",
            ));
        }
        Ok(db.delete_session(id)?)
    }
    pub fn memory_search(&self, query: &str, session: Option<i64>) -> Result<Vec<Message>> {
        if query.len() > 2048 {
            return Err(AppError::invalid("Busca muito longa"));
        }
        Ok(database(&self.db)?.search(query, session, 8)?)
    }
    /// Takes and cancels the active entry (and its own backend). Must hold `active`.
    fn cancel_locked(&self, active: &mut Option<ActiveRun>, bump: bool) {
        if let Some(run) = active.take() {
            run.cancel.cancel();
            run.backend.cancel();
            if bump {
                self.generation.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
    pub fn cancel(&self) {
        if let Ok(mut active) = self.active.lock() {
            self.cancel_locked(&mut active, true);
        }
        if let Ok(mut notice) = self.notice.lock() {
            if let Some((_, token)) = notice.take() {
                token.cancel();
            }
        }
    }
    /// Plays one embedded notice off the UI thread on the configured output device. A newer
    /// notice replaces an older one; `cancel()` stops an active or queued notice (returns Ok).
    pub async fn alert_notice(&self, kind: NoticeKind) -> Result<()> {
        let output = self.settings()?.output_device;
        let cancel = CancellationToken::new();
        let id = self.notice_ids.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut slot = self
                .notice
                .lock()
                .map_err(|_| AppError::new("AUDIO_ERROR", "Aviso indisponível"))?;
            if let Some((_, older)) = slot.replace((id, cancel.clone())) {
                older.cancel();
            }
        }
        let playback = self.playback.clone();
        let token = cancel.clone();
        let result = tokio::task::spawn_blocking(move || {
            let pcm = audio::decode_wav_pcm16(kind.wav(), NOTICE_SAMPLE_RATE)?;
            playback.play(pcm, output.as_deref(), &token, &mut |_| {})
        })
        .await
        .map_err(|_| AppError::new("AUDIO_ERROR", "Aviso interrompido"));
        if let Ok(mut slot) = self.notice.lock() {
            if slot.as_ref().is_some_and(|(current, _)| *current == id) {
                *slot = None;
            }
        }
        match result.and_then(|r| r) {
            Err(error) if error.code == "CANCELLED" => Ok(()),
            other => other,
        }
    }
    /// Atomically replaces any active entry, in lock order active -> state.
    fn register(
        &self,
        session: i64,
        cancel: CancellationToken,
        finish: Arc<AtomicBool>,
    ) -> Result<(u64, Arc<SpeechBackend>)> {
        self.register_checked(session, cancel, finish, false)
    }
    /// Like `register`; when `require_session`, existence is checked under `active`.
    fn register_checked(
        &self,
        session: i64,
        cancel: CancellationToken,
        finish: Arc<AtomicBool>,
        require_session: bool,
    ) -> Result<(u64, Arc<SpeechBackend>)> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| AppError::new("CONVERSATION_BUSY", "Conversa indisponível"))?;
        if require_session && !database(&self.db)?.session_exists(session)? {
            return Err(AppError::invalid("Conversa não encontrada"));
        }
        self.cancel_locked(&mut active, false);
        let backend = self
            .state
            .lock()
            .map_err(|_| AppError::new("SETTINGS_UNAVAILABLE", "Configurações indisponíveis"))?
            .backend
            .clone();
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *active = Some(ActiveRun {
            cancel,
            finish,
            session,
            backend: backend.clone(),
            generation,
        });
        Ok((generation, backend))
    }
    fn clear_if(&self, generation: u64) {
        if let Ok(mut active) = self.active.lock() {
            if active.as_ref().is_some_and(|a| a.generation == generation) {
                *active = None;
            }
        }
    }
    pub fn finish_input(&self) {
        if let Ok(active) = self.active.lock() {
            if let Some(run) = active.as_ref() {
                run.finish.store(true, Ordering::Relaxed);
            }
        }
    }
    pub async fn start(
        self: &Arc<Self>,
        session: i64,
        text: Option<String>,
        sink: EventSink,
    ) -> Result<RunTicket> {
        if text
            .as_ref()
            .is_some_and(|t| t.trim().is_empty() || t.len() > 12000)
        {
            return Err(AppError::invalid("Digite uma mensagem de até 12000 bytes"));
        }
        let cancel = CancellationToken::new();
        let finish = Arc::new(AtomicBool::new(false));
        let (generation, backend) =
            self.register_checked(session, cancel.clone(), finish.clone(), true)?;
        let emitter = Emitter {
            session,
            turn: uuid::Uuid::new_v4().simple().to_string(),
            generation,
            sequence: Arc::new(AtomicU64::new(0)),
            sink,
            cancel: cancel.clone(),
            backend,
            current: self.generation.clone(),
            generated: Arc::new(Mutex::new(String::new())),
            played: Arc::new(Mutex::new(String::new())),
        };
        let verification = tokio::select! {biased;_=cancel.cancelled()=>Err(AppError::cancelled()),result=self.provider(true)=>result.map(|_|())};
        if let Err(error) = verification {
            self.clear_if(generation);
            return Err(error);
        }
        let ticket = RunTicket {
            session_id: session,
            turn_id: emitter.turn.clone(),
            generation,
        };
        let this = Arc::clone(self);
        tokio::spawn(async move {
            emitter.send("run.started", json!({"voice":text.is_none()}));
            let result = if let Some(text) = text {
                this.turn(&text, &emitter).await
            } else {
                this.voice_loop(&emitter, &finish).await
            };
            if let Err(error) = result {
                if error.code == "CANCELLED" {
                    emitter.send("turn.cancelled", json!({}));
                } else {
                    emitter.send("error", serde_json::to_value(error).unwrap_or_default());
                    emitter.state("error");
                }
            }
            if !cancel.is_cancelled() {
                emitter.send("run.completed", json!({}));
                this.clear_if(generation);
            }
        });
        Ok(ticket)
    }
    async fn voice_loop(
        self: &Arc<Self>,
        emitter: &Emitter,
        finish: &Arc<AtomicBool>,
    ) -> Result<()> {
        let _audio = self.audio_serial.lock().await;
        let mut current = emitter.clone();
        if current.cancel.is_cancelled() {
            return Err(AppError::cancelled());
        }
        current.state("preparing");
        {
            let worker = current.backend.clone();
            tokio::task::spawn_blocking(move || worker.preload_voice())
                .await
                .map_err(|_| {
                    AppError::new("AUDIO_ERROR", "Carregamento de áudio interrompido")
                })??;
            if current.cancel.is_cancelled() {
                return Err(AppError::cancelled());
            }
        }
        loop {
            if current.cancel.is_cancelled() {
                return Err(AppError::cancelled());
            }
            current.state("preparing");
            let worker = current.backend.clone();
            let input = self.settings()?.input_device;
            let cancel = current.cancel.clone();
            let finish = finish.clone();
            let levels = current.clone();
            let ready = current.clone();
            let capture = tokio::task::spawn_blocking(move || {
                audio::capture(
                    &worker,
                    input.as_deref(),
                    &cancel,
                    &finish,
                    |rms| levels.level(rms, "input"),
                    || ready.state("listening"),
                )
            })
            .await;
            current.level(0., "input");
            let capture = capture
                .map_err(|_| AppError::new("AUDIO_ERROR", "Captura de áudio interrompida"))?;
            let samples = match capture {
                Ok(samples) => samples,
                Err(error) => {
                    if let Some(reason) = capture_feedback(&error) {
                        current.feedback(reason);
                        continue;
                    }
                    return Err(error);
                }
            };
            current.state("transcribing");
            let worker = current.backend.clone();
            let text = tokio::task::spawn_blocking(move || worker.transcribe(&samples))
                .await
                .map_err(|_| AppError::new("AUDIO_ERROR", "Transcrição interrompida"))??;
            if current.cancel.is_cancelled() {
                return Err(AppError::cancelled());
            }
            match transcript_feedback(&text) {
                Some(reason) => current.feedback(reason),
                None => self.turn(&text, &current).await?,
            }
            current = current.turn();
        }
    }
    async fn turn(self: &Arc<Self>, text: &str, emitter: &Emitter) -> Result<()> {
        if emitter.cancel.is_cancelled() {
            return Err(AppError::cancelled());
        }
        {
            let db = database(&self.db)?;
            db.create_turn(
                &emitter.turn,
                emitter.session,
                emitter.generation,
                crate::now(),
            )?;
            db.append(emitter.session, "user", text)?;
        }
        emitter.send("transcript.final", json!({"text":text}));
        emitter.trace("turn_start", None, None, None);
        let result = crate::agent::answer(self, text, emitter).await;
        let status = match &result {
            Ok(_) => "completed",
            Err(e) if e.code == "CANCELLED" => "interrupted",
            Err(_) => "failed",
        };
        let generated = emitter
            .generated
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default();
        let played = emitter.played.lock().map(|v| v.clone()).unwrap_or_default();
        database(&self.db)?.finish_turn(&emitter.turn, status, &generated, &played)?;
        if let Ok((answer, _)) = &result {
            database(&self.db)?.append(emitter.session, "assistant", answer)?;
            emitter.send("turn.completed", json!({"text":answer}));
        }
        emitter.trace(
            "turn_end",
            None,
            None,
            result.as_ref().err().map(|error| error.code.as_str()),
        );
        result.map(|_| ())
    }
    pub(crate) async fn say(&self, text: &str, emitter: &Emitter) -> Result<String> {
        if emitter.cancel.is_cancelled() {
            return Err(AppError::cancelled());
        }
        if !self.settings()?.speak_text {
            return Ok(String::new());
        }
        let settings = self.settings()?;
        let worker = emitter.backend.clone();
        let text = text.to_string();
        let event = emitter.clone();
        let playback = self.playback.clone();
        let result: std::result::Result<String, (&'static str, AppError)> =
            tokio::task::spawn_blocking(move || {
                if event.cancel.is_cancelled() {
                    return Err(("synthesis", AppError::cancelled()));
                }
                worker.preload_tts().map_err(|e| ("synthesis", e))?;
                if event.cancel.is_cancelled() {
                    return Err(("synthesis", AppError::cancelled()));
                }
                let pcm = worker
                    .synthesize(&text, &settings.voice, settings.speed)
                    .map_err(|e| ("synthesis", e))?;
                event.state("speaking");
                playback
                    .play(
                        pcm,
                        settings.output_device.as_deref(),
                        &event.cancel,
                        &mut |rms| event.level(rms, "output"),
                    )
                    .map_err(|e| ("playback", e))?;
                if let Ok(mut played) = event.played.lock() {
                    played.push_str(&text);
                }
                Ok(text)
            })
            .await
            .map_err(|_| AppError::new("AUDIO_ERROR", "Reprodução interrompida"))?;
        match result {
            Ok(text) => Ok(text),
            Err((_, error)) if error.code == "CANCELLED" => Err(error),
            Err((stage, error)) => {
                emitter.send("audio.error", say_error_payload(&error, stage));
                Ok(String::new())
            }
        }
    }
    pub fn tools(&self) -> Result<Vec<Value>> {
        Ok(self
            .integrations
            .available(&self.db, &self.settings()?)?
            .iter()
            .map(|s| s.definition())
            .collect())
    }
    pub(crate) async fn execute_tool(&self, call: &ToolCall, emitter: &Emitter) -> Result<Value> {
        if emitter.cancel.is_cancelled() {
            return Err(AppError::cancelled());
        }
        let settings = self.settings()?;
        let (integration, _) = self.integrations.spec(&call.name, &self.db, &settings)?;
        emitter.state(integration.state(&call.name));
        let ctx = ToolContext {
            db: &self.db,
            http: &self.http,
            vault: &self.vault,
            settings: &settings,
            session_id: emitter.session,
            cancel: &emitter.cancel,
        };
        let outcome = self
            .integrations
            .dispatch(&call.name, call.arguments.clone(), &ctx)
            .await?;
        if let Some(action) = outcome.action {
            emitter.send("action.changed", serde_json::to_value(action)?);
            emitter.state("awaiting_confirmation");
        }
        Ok(outcome.value)
    }
    pub async fn confirm(&self, id: &str, session: i64) -> Result<Operation> {
        let op = database(&self.db)?.operation(id)?;
        let integration = self.integrations.get(&op.integration)?;
        let _guard = self.service_serial.lock().await;
        let settings = self.settings()?;
        let cancel = CancellationToken::new();
        let ctx = ToolContext {
            db: &self.db,
            http: &self.http,
            vault: &self.vault,
            settings: &settings,
            session_id: session,
            cancel: &cancel,
        };
        integration.commit(&op, &ctx).await
    }
    pub async fn reconcile(&self, id: &str) -> Result<Operation> {
        let op = database(&self.db)?.operation(id)?;
        let integration = self.integrations.get(&op.integration)?;
        let _guard = self.service_serial.lock().await;
        let settings = self.settings()?;
        let cancel = CancellationToken::new();
        let ctx = ToolContext {
            db: &self.db,
            http: &self.http,
            vault: &self.vault,
            settings: &settings,
            session_id: op.session_id.unwrap_or(0),
            cancel: &cancel,
        };
        integration.reconcile(&op, &ctx).await
    }
    pub async fn retry(&self, id: &str, session: i64) -> Result<Operation> {
        let op = database(&self.db)?.operation(id)?;
        let integration = self.integrations.get(&op.integration)?;
        let _guard = self.service_serial.lock().await;
        let settings = self.settings()?;
        let cancel = CancellationToken::new();
        let ctx = ToolContext {
            db: &self.db,
            http: &self.http,
            vault: &self.vault,
            settings: &settings,
            session_id: session,
            cancel: &cancel,
        };
        integration.retry(&op, &ctx).await
    }
    pub async fn model_status(&self) -> Result<Value> {
        let backend = self
            .state
            .lock()
            .map_err(|_| AppError::new("SETTINGS_UNAVAILABLE", "Configurações indisponíveis"))?
            .backend
            .clone();
        tokio::task::spawn_blocking(move || backend.status())
            .await
            .map_err(|_| AppError::new("AUDIO_ERROR", "Diagnóstico interrompido"))?
    }
    /// Installs one catalog artifact under its shared artifact lock. Never
    /// cancels active text or speech.
    pub async fn model_install(
        &self,
        id: String,
        progress: Arc<dyn Fn(Value) + Send + Sync>,
    ) -> Result<Value> {
        self.install_artifact(&id, progress).await
    }
    async fn install_artifact(
        &self,
        id: &str,
        progress: Arc<dyn Fn(Value) + Send + Sync>,
    ) -> Result<Value> {
        if !self.speech_hooks.supported {
            return Err(speech_unsupported());
        }
        if !KNOWN_ARTIFACTS.contains(&id) {
            return Err(AppError::invalid("Modelo fora do catálogo"));
        }
        let lock = self
            .artifact_locks
            .lock()
            .map_err(|_| AppError::new("MODEL_INSTALL", "Instalação indisponível"))?
            .entry(id.to_string())
            .or_default()
            .clone();
        let _guard = lock.lock().await;
        (self.speech_hooks.install)(id.to_string(), progress).await
    }
    /// Cached snapshot only: no hashing, filesystem or network work.
    pub fn speech_status(&self) -> Result<Value> {
        let snapshot = self
            .speech_status
            .lock()
            .map_err(|_| AppError::new("SPEECH_UNAVAILABLE", "Estado da fala indisponível"))?
            .clone();
        Ok(serde_json::to_value(snapshot)?)
    }
    /// Starts (at most one) background preparation of the selected speech models
    /// and returns the current snapshot immediately. Already running or ready
    /// states are returned unchanged; after a failure a new call retries.
    pub async fn speech_prepare(self: &Arc<Self>) -> Result<Value> {
        let unavailable = || AppError::new("SPEECH_UNAVAILABLE", "Estado da fala indisponível");
        if !self.speech_hooks.supported {
            let mut snapshot = self.speech_status.lock().map_err(|_| unavailable())?;
            snapshot.phase = "unsupported".into();
            snapshot.ready = false;
            snapshot.active = false;
            let error = speech_unsupported();
            snapshot.error = Some(error.message);
            snapshot.code = Some(error.code);
            return Ok(serde_json::to_value(&*snapshot)?);
        }
        let mut job = self.speech_job.lock().map_err(|_| unavailable())?;
        let (generation, asr, backend) = {
            let state = self.state.lock().map_err(|_| unavailable())?;
            let generation = self.speech_gen.load(Ordering::SeqCst);
            let mut snapshot = self.speech_status.lock().map_err(|_| unavailable())?;
            let running = job.as_ref().is_some_and(|j| j.generation == generation);
            if running || (snapshot.ready && snapshot.generation == generation) {
                return Ok(serde_json::to_value(&*snapshot)?);
            }
            *snapshot = SpeechSnapshot {
                phase: "starting".into(),
                active: true,
                ..SpeechSnapshot::idle(generation)
            };
            (
                generation,
                state.settings.asr_model.clone(),
                state.backend.clone(),
            )
        };
        if let Some(stale) = job.take() {
            stale.abort.abort();
        }
        let id = self.speech_job_ids.fetch_add(1, Ordering::SeqCst) + 1;
        let runtime = self.clone();
        let inner =
            tokio::spawn(async move { runtime.speech_job_body(generation, &asr, backend).await });
        *job = Some(SpeechJob {
            id,
            generation,
            abort: inner.abort_handle(),
        });
        drop(job);
        let runtime = self.clone();
        tokio::spawn(async move {
            let outcome = inner.await;
            runtime.finish_speech_job(id, generation, outcome);
        });
        self.speech_status()
    }
    fn abort_speech_job(&self) {
        let job = self
            .speech_job
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(job) = job {
            job.abort.abort();
        }
    }
    /// Applies `update` unless the snapshot belongs to another generation.
    fn publish_speech(&self, generation: u64, update: impl FnOnce(&mut SpeechSnapshot)) -> bool {
        let Ok(mut snapshot) = self.speech_status.lock() else {
            return false;
        };
        if snapshot.generation != generation || self.speech_gen.load(Ordering::SeqCst) != generation
        {
            return false;
        }
        update(&mut snapshot);
        true
    }
    /// Terminal bookkeeping, atomic with respect to `speech_prepare`: the
    /// terminal state is published and the in-flight marker cleared together,
    /// for success, failure, cancellation and panic alike.
    fn finish_speech_job(
        &self,
        id: u64,
        generation: u64,
        outcome: std::result::Result<Result<()>, tokio::task::JoinError>,
    ) {
        let mut job = self.speech_job.lock().unwrap_or_else(|e| e.into_inner());
        if !job.as_ref().is_some_and(|j| j.id == id) {
            return; // superseded or aborted by a settings change
        }
        *job = None;
        let failure = match outcome {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error),
            Err(join) if join.is_panic() => Some(AppError::new(
                "SPEECH_INTERRUPTED",
                "A preparação da fala falhou inesperadamente. Tente novamente.",
            )),
            Err(_) => Some(AppError::new(
                "SPEECH_INTERRUPTED",
                "A preparação da fala foi interrompida. Tente novamente.",
            )),
        };
        self.publish_speech(generation, |snapshot| {
            snapshot.active = false;
            match failure {
                None => {
                    snapshot.clear_progress();
                    if snapshot.ready {
                        snapshot.phase = "ready".into();
                    }
                }
                Some(error) if snapshot.ready => {
                    // Only the optional Turbo stage can fail after readiness.
                    snapshot.clear_progress();
                    snapshot.phase = "ready".into();
                    snapshot.warning = Some(format!(
                        "Não foi possível preparar o modelo Turbo (opcional): {}",
                        error.message
                    ));
                }
                Some(error) => {
                    snapshot.phase = "failed".into();
                    snapshot.error = Some(error.message);
                    snapshot.code = Some(error.code);
                }
            }
        });
    }
    async fn speech_job_body(
        self: Arc<Self>,
        generation: u64,
        asr: &str,
        backend: Arc<SpeechBackend>,
    ) -> Result<()> {
        for id in [asr_artifact(asr), VAD_ARTIFACT, TTS_ARTIFACT] {
            self.provision_for_job(generation, id).await?;
            if self.speech_gen.load(Ordering::SeqCst) != generation {
                return Ok(());
            }
        }
        self.publish_speech(generation, |snapshot| {
            snapshot.phase = "loading".into();
            snapshot.clear_progress();
        });
        let preload = self.speech_hooks.preload.clone();
        let loaded = backend.clone();
        tokio::task::spawn_blocking(move || preload(loaded))
            .await
            .map_err(|_| {
                AppError::new(
                    "SPEECH_INTERRUPTED",
                    "O carregamento dos modelos de fala foi interrompido.",
                )
            })??;
        // Readiness only if neither the selection nor the backend changed meanwhile.
        {
            let state = self
                .state
                .lock()
                .map_err(|_| AppError::new("SPEECH_UNAVAILABLE", "Estado da fala indisponível"))?;
            if Arc::ptr_eq(&state.backend, &backend) {
                self.publish_speech(generation, |snapshot| {
                    snapshot.ready = true;
                    snapshot.phase = "ready".into();
                    snapshot.error = None;
                    snapshot.code = None;
                    snapshot.clear_progress();
                });
            } else {
                return Ok(());
            }
        }
        if asr == "small" {
            // Optional default model: its failure is a warning, never lost readiness.
            if let Err(error) = self.provision_for_job(generation, "whisper-turbo").await {
                self.publish_speech(generation, |snapshot| {
                    snapshot.warning = Some(format!(
                        "Não foi possível preparar o modelo Turbo (opcional): {}",
                        error.message
                    ));
                });
            }
            self.publish_speech(generation, |snapshot| {
                snapshot.phase = "ready".into();
                snapshot.clear_progress();
            });
        }
        Ok(())
    }
    async fn provision_for_job(self: &Arc<Self>, generation: u64, id: &str) -> Result<()> {
        let runtime = Arc::downgrade(self);
        let progress: Arc<dyn Fn(Value) + Send + Sync> = Arc::new(move |report: Value| {
            if let Some(runtime) = runtime.upgrade() {
                runtime.publish_speech(generation, |snapshot| {
                    if let Some(phase) = report["phase"].as_str() {
                        snapshot.phase = phase.into();
                    }
                    snapshot.artifact = report["artifact"].as_str().map(str::to_string);
                    snapshot.file = report["file"].as_str().map(str::to_string);
                    snapshot.downloaded = report["downloaded"].as_u64().unwrap_or(0);
                    snapshot.total = report["total"].as_u64().unwrap_or(0);
                });
            }
        });
        self.publish_speech(generation, |snapshot| {
            snapshot.artifact = Some(id.to_string());
            snapshot.file = None;
            snapshot.downloaded = 0;
            snapshot.total = 0;
        });
        self.install_artifact(id, progress).await.map(|_| ())
    }
    pub async fn preview(&self, voice: String) -> Result<()> {
        if voice != "pf_dora" {
            return Err(AppError::invalid("Voz desconhecida"));
        }
        self.cancel();
        let _guard = self.audio_serial.lock().await;
        let settings = self.settings()?;
        let cancel = CancellationToken::new();
        let (generation, backend) =
            self.register(0, cancel.clone(), Arc::new(AtomicBool::new(false)))?;
        let playback = self.playback.clone();
        let result = tokio::task::spawn_blocking(move || {
            if cancel.is_cancelled() {
                return Err(AppError::cancelled());
            }
            backend.preload_tts()?;
            if cancel.is_cancelled() {
                return Err(AppError::cancelled());
            }
            let pcm = backend.synthesize(
                "Olá, eu sou a Aura. Como posso ajudar você hoje?",
                &voice,
                settings.speed,
            )?;
            playback.play(pcm, settings.output_device.as_deref(), &cancel, &mut |_| {})
        })
        .await
        .map_err(|_| AppError::new("AUDIO_ERROR", "Amostra interrompida"))?;
        self.clear_if(generation);
        result
    }
}

const SENTENCE_QUEUE: usize = 2;
const PCM_QUEUE: usize = 1;

/// Per-turn sink for spoken text: one bounded sentence queue feeding the
/// synthesis -> playback pipeline.
pub(crate) struct Speaker {
    tx: Option<tokio::sync::mpsc::Sender<String>>,
    task: Option<tokio::task::JoinHandle<Result<SpeechOutcome>>>,
}
impl Speaker {
    pub(crate) fn sender(&self) -> Option<tokio::sync::mpsc::Sender<String>> {
        self.tx.clone()
    }
    pub(crate) async fn finish(mut self, emitter: &Emitter) -> Result<String> {
        self.tx = None;
        if let Some(task) = self.task.take() {
            let outcome = task
                .await
                .map_err(|_| AppError::new("AUDIO_ERROR", "Reprodução interrompida"))??;
            if let Some(error) = &outcome.playback {
                emitter.send("audio.error", playback_error_payload(error, &outcome));
            } else if !outcome.errors.is_empty() {
                emitter.send("audio.error", synthesis_error_payload(&outcome));
            }
        }
        Ok(emitter.played.lock().map(|s| s.clone()).unwrap_or_default())
    }
}
impl Runtime {
    pub(crate) fn speaker(&self, emitter: &Emitter, settings: &Arc<Settings>) -> Speaker {
        if !settings.speak_text {
            return Speaker {
                tx: None,
                task: None,
            };
        }
        let played = emitter.played.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(SENTENCE_QUEUE);
        let (synth_worker, synth_event, synth_settings) =
            (emitter.backend.clone(), emitter.clone(), settings.clone());
        let (play_event, play_settings) = (emitter.clone(), settings.clone());
        let playback = self.playback.clone();
        let task = tokio::spawn(speech_pipeline(
            rx,
            emitter.cancel.clone(),
            Arc::new(move |text: &str| {
                if synth_event.cancel.is_cancelled() {
                    return Err(AppError::cancelled());
                }
                synth_worker.preload_tts()?;
                if synth_event.cancel.is_cancelled() {
                    return Err(AppError::cancelled());
                }
                synth_worker.synthesize(text, &synth_settings.voice, synth_settings.speed)
            }),
            Arc::new(move |pcm: speech::Pcm| {
                play_event.state("speaking");
                playback.play(
                    pcm,
                    play_settings.output_device.as_deref(),
                    &play_event.cancel,
                    &mut |rms| play_event.level(rms, "output"),
                )
            }),
            played,
        ));
        Speaker {
            tx: Some(tx),
            task: Some(task),
        }
    }
}

/// Enqueues a sentence without ever stranding the caller: gives up on cancellation or closure.
pub(crate) fn send_sentence(
    tx: &tokio::sync::mpsc::Sender<String>,
    cancel: &CancellationToken,
    mut sentence: String,
) {
    use tokio::sync::mpsc::error::TrySendError;
    loop {
        match tx.try_send(sentence) {
            Ok(()) | Err(TrySendError::Closed(_)) => return,
            Err(TrySendError::Full(back)) => {
                if cancel.is_cancelled() {
                    return;
                }
                sentence = back;
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
}

#[derive(Debug, Default)]
struct SpeechOutcome {
    attempted: usize,
    synthesized: usize,
    played: usize,
    errors: Vec<AppError>,
    playback: Option<AppError>,
    cancelled: bool,
}

/// Two-stage ordered pipeline: synthesis -> bounded PCM queue -> playback.
async fn speech_pipeline<S, P>(
    mut sentences: tokio::sync::mpsc::Receiver<String>,
    cancel: CancellationToken,
    synth: Arc<S>,
    play: Arc<P>,
    played: Arc<Mutex<String>>,
) -> Result<SpeechOutcome>
where
    S: Fn(&str) -> Result<speech::Pcm> + Send + Sync + 'static,
    P: Fn(speech::Pcm) -> Result<()> + Send + Sync + 'static,
{
    let stop = cancel.child_token();
    let (pcm_tx, mut pcm_rx) = tokio::sync::mpsc::channel::<(String, speech::Pcm)>(PCM_QUEUE);
    let synth_stop = stop.clone();
    let synth_task = tokio::spawn(async move {
        let mut outcome = SpeechOutcome::default();
        loop {
            let text = tokio::select! {
                biased;
                _ = synth_stop.cancelled() => {
                    outcome.cancelled = true;
                    return outcome;
                }
                next = sentences.recv() => match next { Some(text) => text, None => return outcome },
            };
            outcome.attempted += 1;
            let (worker, input) = (synth.clone(), text.clone());
            let pcm = tokio::task::spawn_blocking(move || worker(&input)).await;
            if synth_stop.is_cancelled() {
                outcome.cancelled = true;
                return outcome;
            }
            match pcm {
                Err(_) => {
                    outcome
                        .errors
                        .push(AppError::new("AUDIO_ERROR", "Síntese interrompida"));
                    return outcome;
                }
                Ok(Err(error)) if error.code == "CANCELLED" => {
                    outcome.cancelled = true;
                    return outcome;
                }
                Ok(Err(error)) => outcome.errors.push(error),
                Ok(Ok(pcm)) if pcm.samples.is_empty() => {
                    outcome
                        .errors
                        .push(AppError::new("AUDIO_ERROR", "Síntese retornou áudio vazio"));
                }
                Ok(Ok(pcm)) => {
                    outcome.synthesized += 1;
                    tokio::select! {
                        biased;
                        _ = synth_stop.cancelled() => {
                            outcome.cancelled = true;
                            return outcome;
                        }
                        sent = pcm_tx.send((text, pcm)) => if sent.is_err() { return outcome },
                    }
                }
            }
        }
    });
    let play_stop = stop.clone();
    let play_task = tokio::spawn(async move {
        let mut outcome = SpeechOutcome::default();
        loop {
            let (text, pcm) = tokio::select! {
                biased;
                _ = play_stop.cancelled() => {
                    outcome.cancelled = true;
                    return outcome;
                }
                next = pcm_rx.recv() => match next { Some(item) => item, None => return outcome },
            };
            let player = play.clone();
            let result = tokio::task::spawn_blocking(move || player(pcm)).await;
            if play_stop.is_cancelled() {
                outcome.cancelled = true;
                return outcome;
            }
            if let Err(error) = result
                .map_err(|_| AppError::new("AUDIO_ERROR", "Reprodução interrompida"))
                .and_then(|r| r)
            {
                play_stop.cancel();
                outcome.playback = Some(error);
                return outcome;
            }
            outcome.played += 1;
            if let Ok(mut played) = played.lock() {
                played.push_str(&text);
            }
        }
    });
    let playback = play_task.await.unwrap_or_else(|_| SpeechOutcome {
        playback: Some(AppError::new("AUDIO_ERROR", "Reprodução interrompida")),
        ..Default::default()
    });
    if playback.playback.is_some() {
        stop.cancel();
    }
    let synthesis = synth_task.await.unwrap_or_else(|_| SpeechOutcome {
        errors: vec![AppError::new("AUDIO_ERROR", "Síntese interrompida")],
        ..Default::default()
    });
    let mut outcome = SpeechOutcome {
        attempted: synthesis.attempted,
        synthesized: synthesis.synthesized,
        played: playback.played,
        errors: synthesis.errors,
        playback: playback.playback,
        cancelled: synthesis.cancelled || playback.cancelled,
    };
    if outcome.playback.is_some() {
        outcome.errors.clear();
        outcome.cancelled = false;
    }
    if outcome.cancelled {
        return Err(AppError::cancelled());
    }
    Ok(outcome)
}

fn synthesis_error_payload(outcome: &SpeechOutcome) -> Value {
    let first = outcome
        .errors
        .first()
        .cloned()
        .unwrap_or_else(|| AppError::new("AUDIO_ERROR", "Síntese interrompida"));
    let mut payload = serde_json::to_value(first).unwrap_or_default();
    payload["stage"] = json!("synthesis");
    payload["partial"] = json!(outcome.played > 0);
    payload["attempted"] = json!(outcome.attempted);
    payload["synthesized"] = json!(outcome.synthesized);
    payload["played"] = json!(outcome.played);
    payload["errors"] = json!(&outcome.errors);
    payload
}

fn playback_error_payload(error: &AppError, outcome: &SpeechOutcome) -> Value {
    let mut payload = serde_json::to_value(error).unwrap_or_default();
    payload["stage"] = json!("playback");
    payload["attempted"] = json!(outcome.attempted);
    payload["synthesized"] = json!(outcome.synthesized);
    payload["played"] = json!(outcome.played);
    payload
}

fn say_error_payload(error: &AppError, stage: &str) -> Value {
    let mut payload = serde_json::to_value(error).unwrap_or_default();
    payload["stage"] = json!(stage);
    if stage == "synthesis" {
        payload["partial"] = json!(false);
        payload["attempted"] = json!(1);
        payload["synthesized"] = json!(0);
        payload["played"] = json!(0);
    }
    payload
}

pub(crate) fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::{Integration, IntegrationManifest, ToolContext};
    use support_core::tools::{Effect, ToolSpec};
    struct ExampleIntegration;
    #[async_trait::async_trait]
    impl Integration for ExampleIntegration {
        fn manifest(&self) -> IntegrationManifest {
            IntegrationManifest {
                id: "example".into(),
                name: "Outro produto".into(),
                description: "Teste de extensibilidade".into(),
                documentation: vec![],
                builtin: true,
            }
        }
        fn tools(&self) -> Vec<ToolSpec> {
            vec![ToolSpec::new(
                "example",
                "example_lookup",
                "Consultando outro produto",
                "Consulta de outro produto",
                Effect::Read,
                json!({"type":"object","properties":{},"additionalProperties":false}),
            )]
        }
        async fn execute(&self, name: &str, _: Value, _: &ToolContext<'_>) -> Result<Value> {
            if name != "example_lookup" {
                return Err(AppError::invalid("Ferramenta incorreta"));
            }
            Ok(json!({"product":"example","answer":42}))
        }
    }
    fn test_paths(dir: &std::path::Path) -> Paths {
        Paths {
            data: dir.join("data"),
            resources: dir.join("resources"),
            python: dir.join("nonexistent-python"),
        }
    }
    fn emitter(session: i64, cancel: CancellationToken, sink: EventSink) -> Emitter {
        let dir = std::env::temp_dir();
        Emitter {
            backend: Arc::new(speech::backend(test_paths(&dir), "turbo")),
            current: Arc::new(AtomicU64::new(1)),
            session,
            turn: "test".into(),
            generation: 1,
            sequence: Arc::new(AtomicU64::new(0)),
            sink,
            cancel,
            generated: Arc::new(Mutex::new(String::new())),
            played: Arc::new(Mutex::new(String::new())),
        }
    }
    #[tokio::test]
    async fn another_product_registers_without_changing_coordinator_or_audio() {
        let directory = tempfile::tempdir().unwrap();
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut runtime = Runtime::open(Paths::new(directory.path().into(), root)).unwrap();
        Arc::get_mut(&mut runtime)
            .unwrap()
            .register_integration(Arc::new(ExampleIntegration))
            .unwrap();
        let tools = runtime.tools().unwrap();
        let names = tools
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"knowledge_search"));
        assert!(names.contains(&"example_lookup"));
        assert!(!names.contains(&"calendar_create_event"));
        let events = emitter(1, CancellationToken::new(), Arc::new(|_| {}));
        let result = runtime
            .execute_tool(
                &ToolCall {
                    id: "call".into(),
                    name: "example_lookup".into(),
                    arguments: json!({}),
                },
                &events,
            )
            .await
            .unwrap();
        assert_eq!(result["answer"], 42);
        let forbidden = runtime
            .execute_tool(
                &ToolCall {
                    id: "call".into(),
                    name: "action_confirm".into(),
                    arguments: json!({}),
                },
                &events,
            )
            .await;
        assert_eq!(forbidden.unwrap_err().code, "TOOL_NOT_ALLOWED");
    }
    #[test]
    fn delayed_events_cannot_restart_cancelled_output_over_100_turns() {
        let events = Arc::new(Mutex::new(Vec::new()));
        for _ in 0..100 {
            let output = events.clone();
            let token = CancellationToken::new();
            let event = emitter(
                1,
                token.clone(),
                Arc::new(move |e| output.lock().unwrap().push(e)),
            );
            token.cancel();
            event.send("assistant.delta", json!({"text":"áudio antigo"}));
            event.level(0.7, "output");
            assert!(event.generated.lock().unwrap().is_empty());
        }
        assert!(events.lock().unwrap().is_empty());
    }

    #[test]
    fn empty_capture_and_empty_transcript_have_distinct_nonspoken_feedback() {
        let empty_speech = AppError::new("EMPTY_SPEECH", "Nenhuma fala detectada.");
        assert_eq!(capture_feedback(&empty_speech), Some("empty_speech"));
        for code in [
            "CANCELLED",
            "DEVICE_UNAVAILABLE",
            "AUDIO_FORMAT",
            "AUDIO_ERROR",
        ] {
            assert_eq!(capture_feedback(&AppError::new(code, "x")), None, "{code}");
        }
        assert_eq!(transcript_feedback(""), Some("empty_transcript"));
        assert_eq!(transcript_feedback(" \n\t "), Some("empty_transcript"));
        assert_eq!(transcript_feedback("olá"), None);
        assert_eq!(transcript_feedback("  olá  "), None);
        assert_ne!(capture_feedback(&empty_speech), transcript_feedback(" "));
    }

    #[test]
    fn empty_feedback_is_visual_only_and_creates_no_turn() {
        let events: Events = Default::default();
        let output = events.clone();
        let token = CancellationToken::new();
        let emitter = emitter(
            7,
            token.clone(),
            Arc::new(move |event| output.lock().unwrap().push(event)),
        );
        emitter.feedback(capture_feedback(&AppError::new("EMPTY_SPEECH", "x")).unwrap());
        emitter.feedback(transcript_feedback("   ").unwrap());
        let log = events.lock().unwrap();
        assert_eq!(log.len(), 2);
        assert!(
            log.iter()
                .all(|event| event.kind == "input.feedback" && event.turn_id == "test")
        );
        assert_eq!(log[0].payload, json!({"reason":"empty_speech"}));
        assert_eq!(log[1].payload, json!({"reason":"empty_transcript"}));
        assert!(!log.iter().any(|event| {
            [
                "transcript.final",
                "turn.completed",
                "run.started",
                "error",
                "audio.error",
            ]
            .contains(&event.kind.as_str())
        }));
        drop(log);
        token.cancel();
        emitter.feedback("empty_speech");
        assert_eq!(events.lock().unwrap().len(), 2);
    }

    // ---- speech pipeline (fake synthesis/playback) ----
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;
    fn pcm() -> speech::Pcm {
        speech::Pcm {
            sample_rate: 24000,
            samples: vec![0.0; 4],
        }
    }
    fn wait_until(condition: impl Fn() -> bool) -> bool {
        for _ in 0..1000 {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }
    type Texts = Arc<Mutex<Vec<String>>>;
    type Events = Arc<Mutex<Vec<AppEvent>>>;
    fn event_log() -> (Events, EventSink) {
        let events: Events = Default::default();
        let output = events.clone();
        (
            events,
            Arc::new(move |event| output.lock().unwrap().push(event)),
        )
    }
    fn audio_errors(events: &Events) -> Vec<Value> {
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.kind == "audio.error")
            .map(|event| event.payload.clone())
            .collect()
    }
    fn speaker(
        tx: tokio::sync::mpsc::Sender<String>,
        task: tokio::task::JoinHandle<Result<SpeechOutcome>>,
    ) -> Speaker {
        Speaker {
            tx: Some(tx),
            task: Some(task),
        }
    }
    fn pipeline<S, P>(
        rx: tokio::sync::mpsc::Receiver<String>,
        cancel: CancellationToken,
        synth: Arc<S>,
        play: Arc<P>,
        played: Arc<Mutex<String>>,
    ) -> tokio::task::JoinHandle<Result<SpeechOutcome>>
    where
        S: Fn(&str) -> Result<speech::Pcm> + Send + Sync + 'static,
        P: Fn(speech::Pcm) -> Result<()> + Send + Sync + 'static,
    {
        tokio::spawn(speech_pipeline(rx, cancel, synth, play, played))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn synthesis_overlaps_playback_in_order_with_accounting() {
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(SENTENCE_QUEUE);
        let started: Texts = Default::default();
        let order: Texts = Default::default();
        let played = Arc::new(Mutex::new(String::new()));
        let current = Arc::new(Mutex::new(String::new()));
        let synth = {
            let (started, current) = (started.clone(), current.clone());
            Arc::new(move |text: &str| {
                started.lock().unwrap().push(text.to_string());
                *current.lock().unwrap() = text.to_string();
                Ok(pcm())
            })
        };
        // Playing "a" only returns once synthesis of "b" has started: proves overlap.
        let play = {
            let (started, order, current) = (started.clone(), order.clone(), current.clone());
            let first = Arc::new(AtomicBool::new(true));
            Arc::new(move |_: speech::Pcm| {
                if first.swap(false, Ordering::SeqCst) {
                    assert!(wait_until(|| started.lock().unwrap().len() >= 2));
                }
                order.lock().unwrap().push(current.lock().unwrap().clone());
                Ok(())
            })
        };
        let task = pipeline(
            rx,
            CancellationToken::new(),
            synth.clone(),
            play.clone(),
            played.clone(),
        );
        for text in ["a", "b", "c"] {
            tx.send(text.to_string()).await.unwrap();
        }
        drop(tx);
        let outcome = task.await.unwrap().unwrap();
        assert_eq!(*started.lock().unwrap(), ["a", "b", "c"]);
        assert_eq!(outcome.attempted, 3);
        assert_eq!(outcome.synthesized, 3);
        assert_eq!(outcome.played, 3);
        assert!(outcome.errors.is_empty());
        assert!(outcome.playback.is_none());
        assert!(!outcome.cancelled);
        assert_eq!(*played.lock().unwrap(), "abc");
        assert_eq!(Arc::strong_count(&synth), 1);
        assert_eq!(Arc::strong_count(&play), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn queues_are_bounded_while_playback_is_blocked() {
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(SENTENCE_QUEUE);
        let synthesized = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(AtomicBool::new(false));
        let played = Arc::new(Mutex::new(String::new()));
        let synth = {
            let synthesized = synthesized.clone();
            Arc::new(move |_: &str| {
                synthesized.fetch_add(1, Ordering::SeqCst);
                Ok(pcm())
            })
        };
        let play = {
            let release = release.clone();
            Arc::new(move |_: speech::Pcm| {
                assert!(wait_until(|| release.load(Ordering::SeqCst)));
                Ok(())
            })
        };
        let task = pipeline(rx, CancellationToken::new(), synth, play, played.clone());
        let mut accepted = 0;
        for _ in 0..10 {
            if tx.try_send("x".into()).is_ok() {
                accepted += 1;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // In flight: 1 playing + 1 queued PCM + 1 synthesized and blocked sending; sentence queue 2.
        assert!(synthesized.load(Ordering::SeqCst) <= 3);
        assert!(accepted <= 3 + SENTENCE_QUEUE);
        release.store(true, Ordering::SeqCst);
        drop(tx);
        let outcome = task.await.unwrap().unwrap();
        assert_eq!(outcome.attempted, accepted);
        assert_eq!(outcome.synthesized, accepted);
        assert_eq!(outcome.played, accepted);
        assert_eq!(played.lock().unwrap().len(), accepted);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancellation_with_full_queues_joins_workers() {
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(SENTENCE_QUEUE);
        let cancel = CancellationToken::new();
        let played = Arc::new(Mutex::new(String::new()));
        let entered = Arc::new(AtomicBool::new(false));
        let synth = Arc::new(|_: &str| Ok(pcm()));
        let play = {
            let (cancel, entered) = (cancel.clone(), entered.clone());
            Arc::new(move |_: speech::Pcm| {
                entered.store(true, Ordering::SeqCst);
                wait_until(|| cancel.is_cancelled());
                Err(AppError::cancelled())
            })
        };
        let task = pipeline(
            rx,
            cancel.clone(),
            synth.clone(),
            play.clone(),
            played.clone(),
        );
        for _ in 0..10 {
            let _ = tx.try_send("x".into());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(entered.load(Ordering::SeqCst));
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("pipeline must join after cancellation")
            .unwrap();
        assert_eq!(result.unwrap_err().code, "CANCELLED");
        assert!(played.lock().unwrap().is_empty());
        assert!(tx.is_closed());
        assert_eq!(Arc::strong_count(&synth), 1);
        assert_eq!(Arc::strong_count(&play), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn partial_synthesis_failure_emits_one_summary() {
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(SENTENCE_QUEUE);
        let (events, sink) = event_log();
        let emitter = emitter(1, CancellationToken::new(), sink);
        let task = pipeline(
            rx,
            emitter.cancel.clone(),
            Arc::new(|text: &str| {
                if text == "b" {
                    Err(AppError::new("AUDIO_ERROR", "falha"))
                } else {
                    Ok(pcm())
                }
            }),
            Arc::new(|_| Ok(())),
            emitter.played.clone(),
        );
        let speaker = speaker(tx.clone(), task);
        for text in ["a", "b", "c"] {
            tx.send(text.into()).await.unwrap();
        }
        drop(tx);
        let spoken = tokio::time::timeout(Duration::from_secs(5), speaker.finish(&emitter))
            .await
            .expect("pipeline must join")
            .unwrap();
        assert_eq!(spoken, "ac");
        let errors = audio_errors(&events);
        assert_eq!(errors.len(), 1);
        let payload = &errors[0];
        assert_eq!(payload["stage"], "synthesis");
        assert_eq!(payload["code"], "AUDIO_ERROR");
        assert_eq!(payload["message"], "falha");
        assert_eq!(payload["partial"], true);
        assert_eq!(payload["attempted"], 3);
        assert_eq!(payload["synthesized"], 2);
        assert_eq!(payload["played"], 2);
        let details = payload["errors"].as_array().unwrap();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0]["message"], "falha");
        assert!(payload.get("text").is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn total_synthesis_failure_emits_one_nonpartial_summary() {
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(SENTENCE_QUEUE);
        let (events, sink) = event_log();
        let emitter = emitter(1, CancellationToken::new(), sink);
        let task = pipeline(
            rx,
            emitter.cancel.clone(),
            Arc::new(|_: &str| Err(AppError::new("AUDIO_ERROR", "falha total").retryable())),
            Arc::new(|_| panic!("playback must not run")),
            emitter.played.clone(),
        );
        let speaker = speaker(tx.clone(), task);
        for text in ["a", "b"] {
            tx.send(text.into()).await.unwrap();
        }
        drop(tx);
        let spoken = tokio::time::timeout(Duration::from_secs(5), speaker.finish(&emitter))
            .await
            .expect("pipeline must join")
            .unwrap();
        assert!(spoken.is_empty());
        let errors = audio_errors(&events);
        assert_eq!(errors.len(), 1);
        let payload = &errors[0];
        assert_eq!(payload["stage"], "synthesis");
        assert_eq!(payload["message"], "falha total");
        assert_eq!(payload["retryable"], true);
        assert_eq!(payload["partial"], false);
        assert_eq!(payload["attempted"], 2);
        assert_eq!(payload["synthesized"], 0);
        assert_eq!(payload["played"], 0);
        assert_eq!(payload["errors"].as_array().unwrap().len(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn empty_pcm_is_a_synthesis_failure_not_success() {
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(SENTENCE_QUEUE);
        let (events, sink) = event_log();
        let emitter = emitter(1, CancellationToken::new(), sink);
        let task = pipeline(
            rx,
            emitter.cancel.clone(),
            Arc::new(|_: &str| {
                Ok(speech::Pcm {
                    sample_rate: 24000,
                    samples: Vec::new(),
                })
            }),
            Arc::new(|_| panic!("playback must not run")),
            emitter.played.clone(),
        );
        let speaker = speaker(tx.clone(), task);
        tx.send("a".into()).await.unwrap();
        drop(tx);
        let spoken = tokio::time::timeout(Duration::from_secs(5), speaker.finish(&emitter))
            .await
            .expect("pipeline must join")
            .unwrap();
        assert!(spoken.is_empty());
        let errors = audio_errors(&events);
        assert_eq!(errors.len(), 1);
        let payload = &errors[0];
        assert_eq!(payload["stage"], "synthesis");
        assert_eq!(payload["code"], "AUDIO_ERROR");
        assert!(payload["message"].as_str().unwrap().contains("vazio"));
        assert_eq!(payload["partial"], false);
        assert_eq!(payload["attempted"], 1);
        assert_eq!(payload["synthesized"], 0);
        assert_eq!(payload["played"], 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn playback_failure_wins_over_blocked_synthesis() {
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(SENTENCE_QUEUE);
        let (events, sink) = event_log();
        let emitter = emitter(1, CancellationToken::new(), sink);
        let b_started = Arc::new(AtomicBool::new(false));
        let playback_failed = Arc::new(AtomicBool::new(false));
        let synth = {
            let (b_started, playback_failed) = (b_started.clone(), playback_failed.clone());
            Arc::new(move |text: &str| {
                if text == "b" {
                    b_started.store(true, Ordering::SeqCst);
                    assert!(wait_until(|| playback_failed.load(Ordering::SeqCst)));
                    return Err(AppError::new("AUDIO_ERROR", "síntese tardia"));
                }
                Ok(pcm())
            })
        };
        let play = {
            let (b_started, playback_failed) = (b_started.clone(), playback_failed.clone());
            Arc::new(move |_: speech::Pcm| {
                assert!(wait_until(|| b_started.load(Ordering::SeqCst)));
                playback_failed.store(true, Ordering::SeqCst);
                Err(AppError::new("DEVICE_UNAVAILABLE", "saída indisponível"))
            })
        };
        let task = pipeline(
            rx,
            emitter.cancel.clone(),
            synth.clone(),
            play.clone(),
            emitter.played.clone(),
        );
        let speaker = speaker(tx.clone(), task);
        tx.send("a".into()).await.unwrap();
        tx.send("b".into()).await.unwrap();
        drop(tx);
        let spoken = tokio::time::timeout(Duration::from_secs(5), speaker.finish(&emitter))
            .await
            .expect("pipeline must join")
            .unwrap();
        assert!(spoken.is_empty());
        let errors = audio_errors(&events);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["stage"], "playback");
        assert_eq!(errors[0]["code"], "DEVICE_UNAVAILABLE");
        assert_eq!(errors[0]["message"], "saída indisponível");
        assert_eq!(errors[0]["attempted"], 2);
        assert_eq!(errors[0]["synthesized"], 1);
        assert_eq!(errors[0]["played"], 0);
        assert!(errors[0].get("errors").is_none());
        assert_eq!(Arc::strong_count(&synth), 1);
        assert_eq!(Arc::strong_count(&play), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn synthesis_panic_is_reported_as_synthesis() {
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(SENTENCE_QUEUE);
        let (events, sink) = event_log();
        let emitter = emitter(1, CancellationToken::new(), sink);
        let synth = Arc::new(|_: &str| -> Result<speech::Pcm> { panic!("falha sintética") });
        let play = Arc::new(|_| panic!("playback must not run"));
        let task = pipeline(
            rx,
            emitter.cancel.clone(),
            synth.clone(),
            play.clone(),
            emitter.played.clone(),
        );
        let speaker = speaker(tx.clone(), task);
        tx.send("a".into()).await.unwrap();
        drop(tx);
        let spoken = tokio::time::timeout(Duration::from_secs(5), speaker.finish(&emitter))
            .await
            .expect("pipeline must join")
            .unwrap();
        assert!(spoken.is_empty());
        let errors = audio_errors(&events);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["stage"], "synthesis");
        assert_eq!(errors[0]["code"], "AUDIO_ERROR");
        assert_eq!(errors[0]["message"], "Síntese interrompida");
        assert_eq!(errors[0]["partial"], false);
        assert_eq!(errors[0]["attempted"], 1);
        assert_eq!(errors[0]["synthesized"], 0);
        assert_eq!(errors[0]["played"], 0);
        assert_eq!(Arc::strong_count(&synth), 1);
        assert_eq!(Arc::strong_count(&play), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancellation_while_synthesis_is_blocked_is_silent() {
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(SENTENCE_QUEUE);
        let (events, sink) = event_log();
        let cancel = CancellationToken::new();
        let emitter = emitter(1, cancel.clone(), sink);
        let started = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let synth = {
            let (started, release) = (started.clone(), release.clone());
            Arc::new(move |_: &str| {
                started.store(true, Ordering::SeqCst);
                assert!(wait_until(|| release.load(Ordering::SeqCst)));
                Err(AppError::new("AUDIO_ERROR", "síntese tardia"))
            })
        };
        let play = Arc::new(|_| Ok(()));
        let task = pipeline(
            rx,
            cancel.clone(),
            synth.clone(),
            play.clone(),
            emitter.played.clone(),
        );
        let speaker = speaker(tx.clone(), task);
        tx.send("a".into()).await.unwrap();
        assert!(wait_until(|| started.load(Ordering::SeqCst)));
        cancel.cancel();
        release.store(true, Ordering::SeqCst);
        let result = tokio::time::timeout(Duration::from_secs(5), speaker.finish(&emitter))
            .await
            .expect("pipeline must join after cancellation")
            .unwrap_err();
        assert_eq!(result.code, "CANCELLED");
        assert!(audio_errors(&events).is_empty());
        assert!(emitter.played.lock().unwrap().is_empty());
        assert_eq!(Arc::strong_count(&synth), 1);
        assert_eq!(Arc::strong_count(&play), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn generation_replacement_before_release_emits_no_stale_error() {
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(SENTENCE_QUEUE);
        let (events, sink) = event_log();
        let emitter = emitter(1, CancellationToken::new(), sink);
        let started = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let synth = {
            let (started, release) = (started.clone(), release.clone());
            Arc::new(move |_: &str| {
                started.store(true, Ordering::SeqCst);
                assert!(wait_until(|| release.load(Ordering::SeqCst)));
                Err(AppError::new("AUDIO_ERROR", "falha"))
            })
        };
        let play = Arc::new(|_| Ok(()));
        let task = pipeline(
            rx,
            emitter.cancel.clone(),
            synth.clone(),
            play.clone(),
            emitter.played.clone(),
        );
        let speaker = speaker(tx.clone(), task);
        tx.send("a".into()).await.unwrap();
        assert!(wait_until(|| started.load(Ordering::SeqCst)));
        emitter.current.store(2, Ordering::SeqCst);
        release.store(true, Ordering::SeqCst);
        drop(tx);
        let spoken = tokio::time::timeout(Duration::from_secs(5), speaker.finish(&emitter))
            .await
            .expect("pipeline must join")
            .unwrap();
        assert!(spoken.is_empty());
        assert!(events.lock().unwrap().is_empty());
        assert_eq!(Arc::strong_count(&synth), 1);
        assert_eq!(Arc::strong_count(&play), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn playback_error_stops_pipeline_and_closes_input() {
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(SENTENCE_QUEUE);
        let played = Arc::new(Mutex::new(String::new()));
        let current = Arc::new(Mutex::new(String::new()));
        let synth = {
            let current = current.clone();
            Arc::new(move |text: &str| {
                *current.lock().unwrap() = text.to_string();
                Ok(pcm())
            })
        };
        let play =
            Arc::new(move |_: speech::Pcm| Err(AppError::new("AUDIO_ERROR", "saída indisponível")));
        let task = pipeline(
            rx,
            CancellationToken::new(),
            synth.clone(),
            play.clone(),
            played.clone(),
        );
        tx.send("a".into()).await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("pipeline must stop")
            .unwrap();
        let outcome = result.unwrap();
        assert_eq!(outcome.playback.unwrap().code, "AUDIO_ERROR");
        assert!(outcome.errors.is_empty());
        assert!(played.lock().unwrap().is_empty());
        assert!(tx.is_closed());
        assert_eq!(Arc::strong_count(&synth), 1);
        assert_eq!(Arc::strong_count(&play), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_sentence_gives_up_on_cancel_or_close() {
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(1);
        let cancel = CancellationToken::new();
        tx.try_send("a".into()).unwrap();
        let (sender, token) = (tx.clone(), cancel.clone());
        let blocked =
            tokio::task::spawn_blocking(move || send_sentence(&sender, &token, "b".into()));
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), blocked)
            .await
            .unwrap()
            .unwrap();
        drop(rx);
        send_sentence(&tx, &CancellationToken::new(), "c".into());
    }

    #[test]
    fn stale_generation_emits_no_events() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let output = events.clone();
        let mut event = emitter(
            1,
            CancellationToken::new(),
            Arc::new(move |e| output.lock().unwrap().push(e)),
        );
        event.current = Arc::new(AtomicU64::new(2));
        event.send("audio.error", json!({}));
        event.state("speaking");
        event.level(0.5, "output");
        assert!(events.lock().unwrap().is_empty());
    }

    // ---- settings and session races ----
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_settings_saves_keep_db_and_memory_consistent() {
        let directory = tempfile::tempdir().unwrap();
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let runtime = Runtime::open(Paths::new(directory.path().into(), root)).unwrap();
        let mut tasks = Vec::new();
        for i in 0..16 {
            let runtime = runtime.clone();
            tasks.push(tokio::spawn(async move {
                let mut settings = Settings::default();
                settings.speed = 1.0 + (i as f32) / 100.0;
                settings.asr_model = if i % 2 == 0 { "turbo" } else { "small" }.into();
                runtime.save_settings(settings, None, None, None).await
            }));
        }
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        let memory = runtime.settings().unwrap();
        let stored: Settings = serde_json::from_value(
            database(&runtime.db)
                .unwrap()
                .get_json("settings")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&memory).unwrap(),
            serde_json::to_value(&stored).unwrap()
        );
    }

    #[test]
    fn deleting_unrelated_session_keeps_active_run_and_blocks_stale_registration() {
        let directory = tempfile::tempdir().unwrap();
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let runtime = Runtime::open(Paths::new(directory.path().into(), root)).unwrap();
        let a = runtime.session_create("a").unwrap().id;
        let b = runtime.session_create("b").unwrap().id;
        let cancel = CancellationToken::new();
        runtime
            .register_checked(a, cancel.clone(), Arc::new(AtomicBool::new(false)), true)
            .unwrap();
        assert!(runtime.session_delete(b).unwrap());
        assert!(!cancel.is_cancelled());
        assert!(runtime.active.lock().unwrap().is_some());
        let error = runtime
            .register_checked(
                b,
                CancellationToken::new(),
                Arc::new(AtomicBool::new(false)),
                true,
            )
            .err()
            .unwrap();
        assert_eq!(error.code, "INVALID_INPUT");
        assert!(!cancel.is_cancelled());
        assert!(runtime.session_delete(a).unwrap());
        assert!(cancel.is_cancelled());
        assert!(runtime.active.lock().unwrap().is_none());
    }

    fn check_runtime(dir: &std::path::Path) -> Arc<Runtime> {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        Runtime::open(Paths::new(dir.into(), root)).unwrap()
    }
    fn compatible_settings(model: &str) -> Settings {
        let mut settings = Settings::default();
        settings.provider.kind = crate::config::ProviderKind::CompatibleApi;
        settings.provider.base_url = "http://127.0.0.1:9/v1".into();
        settings.provider.model = model.into();
        settings
    }
    fn probe_for(settings: &Settings) -> ProbeResult {
        ProbeResult {
            config: settings.provider.clone(),
            verified_at: 7,
            fingerprint: settings.provider.model.clone(),
            streaming: true,
            tools: true,
            portuguese: true,
            cancellation: true,
        }
    }
    fn store_probe(runtime: &Runtime, probe: &ProbeResult) {
        database(&runtime.db)
            .unwrap()
            .set_json("provider_probe", &serde_json::to_value(probe).unwrap())
            .unwrap();
    }
    #[tokio::test]
    async fn runtime_start_still_rejects_capability_fingerprint_and_config_changes() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = check_runtime(directory.path());
        let settings = compatible_settings("modelo");
        runtime
            .save_settings(settings.clone(), None, None, None)
            .await
            .unwrap();
        let valid = probe_for(&settings);
        store_probe(&runtime, &valid);
        assert!(runtime.provider(true).await.is_ok());
        let mut variants = Vec::new();
        for field in 0..4 {
            let mut probe = valid.clone();
            match field {
                0 => probe.tools = false,
                1 => probe.streaming = false,
                2 => probe.portuguese = false,
                _ => probe.cancellation = false,
            }
            variants.push(probe);
        }
        let mut other_fingerprint = valid.clone();
        other_fingerprint.fingerprint = "outro".into();
        variants.push(other_fingerprint);
        let mut other_config = valid.clone();
        other_config.config.context_tokens = 16384;
        variants.push(other_config);
        for probe in variants {
            store_probe(&runtime, &probe);
            let error = runtime.provider(true).await.err().unwrap();
            assert_eq!(error.code, "MODEL_NOT_VERIFIED");
        }
    }
    #[tokio::test]
    async fn check_reuses_only_a_fully_valid_stored_probe() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = check_runtime(directory.path());
        let settings = compatible_settings("modelo");
        runtime
            .save_settings(settings.clone(), None, None, None)
            .await
            .unwrap();
        store_probe(&runtime, &probe_for(&settings));
        let result = runtime.provider_check().await.unwrap();
        assert_eq!(result.verified_at, 7);
        let mut stale = probe_for(&settings);
        stale.tools = false;
        store_probe(&runtime, &stale);
        // Falls back to the full probe, which fails against the closed local port.
        assert!(runtime.provider_check().await.is_err());
        assert!(!runtime.probe_status().unwrap()["tools"].as_bool().unwrap());
    }
    #[tokio::test]
    async fn concurrent_checks_share_one_operation_including_failures_and_then_retry() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = check_runtime(directory.path());
        let probe = probe_for(&compatible_settings("modelo"));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let operation =
            |calls: Arc<std::sync::atomic::AtomicUsize>, fail: bool, probe: ProbeResult| {
                move |_: Arc<Runtime>, _: u64| {
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        if fail {
                            Err(AppError::new("PROVIDER_ERROR", "falhou"))
                        } else {
                            Ok(probe)
                        }
                    }
                    .boxed()
                }
            };
        let limit = Duration::from_secs(5);
        let (a, b, c) = tokio::join!(
            runtime.check_with(limit, operation(calls.clone(), true, probe.clone())),
            runtime.check_with(limit, operation(calls.clone(), true, probe.clone())),
            runtime.check_with(limit, operation(calls.clone(), true, probe.clone())),
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        for result in [a, b, c] {
            assert_eq!(result.unwrap_err().code, "PROVIDER_ERROR");
        }
        let (a, b) = tokio::join!(
            runtime.check_with(limit, operation(calls.clone(), false, probe.clone())),
            runtime.check_with(limit, operation(calls.clone(), false, probe.clone())),
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(a.unwrap().verified_at, 7);
        assert_eq!(b.unwrap().verified_at, 7);
        assert!(runtime.provider_check.lock().unwrap().is_none());
    }
    #[tokio::test]
    async fn check_times_out_with_actionable_error_and_can_be_retried() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = check_runtime(directory.path());
        let probe = probe_for(&compatible_settings("modelo"));
        let error = runtime
            .check_with(Duration::from_millis(20), |_, _| {
                std::future::pending::<Result<ProbeResult>>().boxed()
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "PROVIDER_TIMEOUT");
        assert!(error.retryable);
        assert!(error.message.contains("tente novamente"));
        assert!(runtime.provider_check.lock().unwrap().is_none());
        let expected = probe.clone();
        let result = runtime
            .check_with(Duration::from_secs(5), move |_, _| {
                async move { Ok(expected) }.boxed()
            })
            .await
            .unwrap();
        assert_eq!(result.verified_at, 7);
    }
    #[tokio::test]
    async fn check_cannot_publish_after_provider_or_credential_changes() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = check_runtime(directory.path());
        let first = compatible_settings("modelo");
        runtime
            .save_settings(first.clone(), None, None, None)
            .await
            .unwrap();
        // Provider configuration changes while a check is in flight.
        let changed = compatible_settings("outro");
        let probe = probe_for(&first);
        let error = runtime
            .check_with(Duration::from_secs(5), {
                let probe = probe.clone();
                let changed = changed.clone();
                move |runtime, revision| {
                    async move {
                        runtime
                            .save_settings(changed, None, None, None)
                            .await
                            .unwrap();
                        runtime.publish_probe(revision, Some(&probe))?;
                        Ok(probe)
                    }
                    .boxed()
                }
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "SETTINGS_CHANGED");
        assert!(runtime.probe_status().unwrap().is_null());
        // Credential changes (even if the vault write fails) also supersede the check.
        let revision = runtime.provider_revision.load(Ordering::SeqCst);
        let _ = runtime
            .save_settings(changed.clone(), Some(String::new()), None, None)
            .await;
        assert_ne!(runtime.provider_revision.load(Ordering::SeqCst), revision);
        let current = probe_for(&changed);
        assert_eq!(
            runtime
                .publish_probe(revision, Some(&current))
                .unwrap_err()
                .code,
            "SETTINGS_CHANGED"
        );
        assert!(runtime.probe_status().unwrap().is_null());
        // A check started for the new revision does not join the superseded one.
        let revision = runtime.provider_revision.load(Ordering::SeqCst);
        runtime.publish_probe(revision, Some(&current)).unwrap();
        assert_eq!(runtime.probe_status().unwrap()["verified_at"], 7);
    }

    #[test]
    fn turn_trace_line_is_metadata_only() {
        let synthetic = concat!("CONTEUDO", "_SINTETICO_DO_TESTE");
        let credential = concat!("sk-", "credencial-sintetica");
        let trace = TurnTrace {
            session: 7,
            turn: "turno-sintetico",
            generation: 3,
            sequence: 11,
            messages: Some(4),
            deltas: Some(5),
            code: Some("PROVIDER_ERROR"),
        };
        let line = trace_line("provider_round_end", &trace);
        assert!(line.contains("stage=provider_round_end"));
        assert!(line.contains("session=7"));
        assert!(line.contains("turn=turno-sintetico"));
        assert!(line.contains("generation=3"));
        assert!(line.contains("sequence=11"));
        assert!(line.contains("messages=4"));
        assert!(line.contains("deltas=5"));
        assert!(line.contains("code=PROVIDER_ERROR"));
        assert!(!line.contains(synthetic));
        assert!(!line.contains(credential));
        assert!(!line.contains("http"));
        assert!(
            !line.contains(&json!({"messages":[{"role":"user","content":synthetic}]}).to_string())
        );
    }
}

#[cfg(test)]
pub(crate) mod speech_fixture {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::Semaphore;

    /// Injected installer/preload: no network, no real models.
    #[derive(Default)]
    pub(crate) struct FakeSpeech {
        pub installs: Mutex<Vec<String>>,
        pub failing: Mutex<HashSet<String>>,
        pub panicking: Mutex<HashSet<String>>,
        /// Installs of these ids wait for a permit.
        pub gates: Mutex<HashMap<String, Arc<Semaphore>>>,
        pub running: Mutex<HashMap<String, usize>>,
        pub max_running: Mutex<HashMap<String, usize>>,
        pub preloads: AtomicUsize,
        pub preload_fail: std::sync::atomic::AtomicBool,
        pub preload_hold: std::sync::atomic::AtomicBool,
        pub preload_started: std::sync::atomic::AtomicBool,
    }
    impl FakeSpeech {
        pub fn installed(&self) -> Vec<String> {
            self.installs.lock().unwrap().clone()
        }
        pub fn gate(&self, id: &str) -> Arc<Semaphore> {
            let gate = Arc::new(Semaphore::new(0));
            self.gates.lock().unwrap().insert(id.into(), gate.clone());
            gate
        }
        pub fn hooks(self: &Arc<Self>) -> SpeechHooks {
            let install_state = self.clone();
            let preload_state = self.clone();
            SpeechHooks {
                supported: true,
                install: Arc::new(move |id, progress| {
                    let fake = install_state.clone();
                    async move {
                        fake.installs.lock().unwrap().push(id.clone());
                        {
                            let mut running = fake.running.lock().unwrap();
                            let now = running.entry(id.clone()).or_default();
                            *now += 1;
                            let now = *now;
                            let mut max = fake.max_running.lock().unwrap();
                            let top = max.entry(id.clone()).or_default();
                            *top = (*top).max(now);
                        }
                        progress(json!({
                            "artifact": id, "phase": "downloading", "file": "f.bin",
                            "downloaded": 1, "total": 2
                        }));
                        let gate = fake.gates.lock().unwrap().get(&id).cloned();
                        if let Some(gate) = gate {
                            gate.acquire().await.unwrap().forget();
                        }
                        *fake.running.lock().unwrap().get_mut(&id).unwrap() -= 1;
                        if fake.panicking.lock().unwrap().contains(&id) {
                            panic!("fixture panic");
                        }
                        if fake.failing.lock().unwrap().contains(&id) {
                            return Err(AppError::new("MODEL_NETWORK", "Falha de rede: offline"));
                        }
                        Ok(json!({"id": id}))
                    }
                    .boxed()
                }),
                preload: Arc::new(move |_backend| {
                    preload_state.preloads.fetch_add(1, Ordering::SeqCst);
                    preload_state.preload_started.store(true, Ordering::SeqCst);
                    while preload_state.preload_hold.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    if preload_state.preload_fail.load(Ordering::SeqCst) {
                        return Err(AppError::new("MODEL_INVALID", "Falha ao carregar a voz"));
                    }
                    Ok(())
                }),
            }
        }
    }
    pub(crate) fn runtime_with(dir: &std::path::Path, fake: &Arc<FakeSpeech>) -> Arc<Runtime> {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut runtime = Runtime::open(Paths::new(dir.into(), root)).unwrap();
        Arc::get_mut(&mut runtime).unwrap().speech_hooks = fake.hooks();
        runtime
    }
}

#[cfg(test)]
mod speech_tests {
    use super::speech_fixture::*;
    use super::*;

    async fn wait_for(what: &str, condition: impl Fn() -> bool) {
        for _ in 0..600 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timeout: {what}");
    }
    fn setup() -> (tempfile::TempDir, Arc<FakeSpeech>, Arc<Runtime>) {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeSpeech::default());
        let runtime = runtime_with(dir.path(), &fake);
        (dir, fake, runtime)
    }
    fn status(runtime: &Runtime) -> Value {
        runtime.speech_status().unwrap()
    }
    fn idle_job(runtime: &Runtime) -> bool {
        runtime.speech_job.lock().unwrap().is_none()
    }
    fn small(runtime: &Runtime) -> Settings {
        let mut settings = runtime.settings().unwrap();
        settings.asr_model = "small".into();
        settings
    }

    #[tokio::test]
    async fn status_is_cached_and_does_no_work() {
        let (_dir, fake, runtime) = setup();
        let first = status(&runtime);
        assert_eq!(first["phase"], "idle");
        assert_eq!(first["ready"], false);
        assert_eq!(first["active"], false);
        assert_eq!(status(&runtime), first);
        assert!(fake.installed().is_empty());
        assert_eq!(fake.preloads.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn prepare_returns_promptly_and_duplicates_share_one_job() {
        let (_dir, fake, runtime) = setup();
        let gate = fake.gate("whisper-turbo");
        let first = runtime.speech_prepare().await.unwrap();
        assert_eq!(first["active"], true);
        assert_eq!(first["ready"], false);
        let second = runtime.speech_prepare().await.unwrap();
        assert_eq!(second["generation"], first["generation"]);
        wait_for("download progress", || {
            status(&runtime)["phase"] == "downloading"
        })
        .await;
        let snapshot = status(&runtime);
        assert_eq!(snapshot["artifact"], "whisper-turbo");
        assert_eq!(snapshot["file"], "f.bin");
        assert_eq!(
            (snapshot["downloaded"].as_u64(), snapshot["total"].as_u64()),
            (Some(1), Some(2))
        );
        assert_eq!(fake.installed(), ["whisper-turbo"]);
        // Text stays usable while speech is still downloading.
        assert!(runtime.session_create("Texto").is_ok());
        gate.add_permits(1);
        wait_for("ready", || status(&runtime)["ready"] == true).await;
        wait_for("job cleared", || idle_job(&runtime)).await;
        assert_eq!(
            fake.installed(),
            ["whisper-turbo", "silero-vad", "kokoro-multi-lang-v1_0"]
        );
        assert_eq!(fake.preloads.load(Ordering::SeqCst), 1);
        let done = status(&runtime);
        assert_eq!(
            (done["phase"].as_str(), done["active"].as_bool()),
            (Some("ready"), Some(false))
        );
        // Ready is sticky: no new job, no new work.
        runtime.speech_prepare().await.unwrap();
        assert_eq!(fake.installs.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn failure_is_actionable_and_can_be_retried() {
        let (_dir, fake, runtime) = setup();
        fake.failing.lock().unwrap().insert("silero-vad".into());
        runtime.speech_prepare().await.unwrap();
        wait_for("failure", || status(&runtime)["phase"] == "failed").await;
        let failed = status(&runtime);
        assert_eq!(failed["ready"], false);
        assert_eq!(failed["active"], false);
        assert_eq!(failed["code"], "MODEL_NETWORK");
        assert_eq!(failed["artifact"], "silero-vad");
        assert!(failed["error"].as_str().unwrap().contains("rede"));
        assert_eq!(fake.preloads.load(Ordering::SeqCst), 0);
        assert!(idle_job(&runtime));
        fake.failing.lock().unwrap().clear();
        let retry = runtime.speech_prepare().await.unwrap();
        assert_eq!(retry["error"], Value::Null);
        wait_for("ready", || status(&runtime)["ready"] == true).await;
    }

    #[tokio::test]
    async fn preload_failure_prevents_readiness_even_with_installed_models() {
        let (_dir, fake, runtime) = setup();
        fake.preload_fail.store(true, Ordering::SeqCst);
        runtime.speech_prepare().await.unwrap();
        wait_for("failure", || status(&runtime)["phase"] == "failed").await;
        let failed = status(&runtime);
        assert_eq!(failed["ready"], false);
        assert_eq!(failed["code"], "MODEL_INVALID");
        assert_eq!(fake.installed().len(), 3);
        fake.preload_fail.store(false, Ordering::SeqCst);
        runtime.speech_prepare().await.unwrap();
        wait_for("ready", || status(&runtime)["ready"] == true).await;
        assert_eq!(fake.preloads.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn panic_clears_the_marker_and_permits_retry() {
        let (_dir, fake, runtime) = setup();
        fake.panicking
            .lock()
            .unwrap()
            .insert("whisper-turbo".into());
        runtime.speech_prepare().await.unwrap();
        wait_for("failure", || status(&runtime)["phase"] == "failed").await;
        assert_eq!(status(&runtime)["code"], "SPEECH_INTERRUPTED");
        assert!(idle_job(&runtime));
        fake.panicking.lock().unwrap().clear();
        runtime.speech_prepare().await.unwrap();
        wait_for("ready", || status(&runtime)["ready"] == true).await;
    }

    #[tokio::test]
    async fn small_is_ready_before_optional_turbo_and_turbo_failure_is_a_warning() {
        let (_dir, fake, runtime) = setup();
        let gate = fake.gate("whisper-turbo");
        fake.failing.lock().unwrap().insert("whisper-turbo".into());
        runtime
            .save_settings(small(&runtime), None, None, None)
            .await
            .unwrap();
        runtime.speech_prepare().await.unwrap();
        wait_for("ready while turbo pending", || {
            let s = status(&runtime);
            s["ready"] == true && s["artifact"] == "whisper-turbo"
        })
        .await;
        let pending = status(&runtime);
        assert_eq!(pending["active"], true);
        assert_eq!(runtime.settings().unwrap().asr_model, "small");
        assert_eq!(
            fake.installed(),
            [
                "whisper-small",
                "silero-vad",
                "kokoro-multi-lang-v1_0",
                "whisper-turbo"
            ]
        );
        gate.add_permits(1);
        wait_for("job done", || idle_job(&runtime)).await;
        let done = status(&runtime);
        assert_eq!(done["ready"], true);
        assert_eq!(done["active"], false);
        assert_eq!(done["error"], Value::Null);
        assert!(done["warning"].as_str().unwrap().contains("Turbo"));
        assert_eq!(runtime.settings().unwrap().asr_model, "small");
    }

    #[tokio::test]
    async fn settings_change_invalidates_readiness_and_stale_jobs_cannot_publish() {
        let (_dir, fake, runtime) = setup();
        fake.preload_hold.store(true, Ordering::SeqCst);
        let first = runtime.speech_prepare().await.unwrap();
        wait_for("preload", || fake.preload_started.load(Ordering::SeqCst)).await;
        assert_eq!(status(&runtime)["phase"], "loading");
        runtime
            .save_settings(small(&runtime), None, None, None)
            .await
            .unwrap();
        let reset = status(&runtime);
        assert_eq!(reset["phase"], "idle");
        assert_eq!(reset["ready"], false);
        assert_ne!(reset["generation"], first["generation"]);
        // The stale preload finishes successfully but must not mark readiness.
        fake.preload_hold.store(false, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(status(&runtime)["ready"], false);
        assert_eq!(status(&runtime)["phase"], "idle");
        // A fresh preparation for the new selection becomes ready.
        runtime.speech_prepare().await.unwrap();
        wait_for("ready", || status(&runtime)["ready"] == true).await;
        assert!(fake.installed().contains(&"whisper-small".to_string()));
        assert_eq!(status(&runtime)["generation"], reset["generation"]);
    }

    #[tokio::test]
    async fn settings_change_during_install_aborts_the_stale_job() {
        let (_dir, fake, runtime) = setup();
        let gate = fake.gate("silero-vad");
        runtime.speech_prepare().await.unwrap();
        wait_for("vad install", || {
            fake.installed().contains(&"silero-vad".to_string())
        })
        .await;
        runtime
            .save_settings(small(&runtime), None, None, None)
            .await
            .unwrap();
        gate.add_permits(8);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !fake
                .installed()
                .contains(&"kokoro-multi-lang-v1_0".to_string())
        );
        assert_eq!(status(&runtime)["phase"], "idle");
        assert!(idle_job(&runtime));
        runtime.speech_prepare().await.unwrap();
        wait_for("ready", || status(&runtime)["ready"] == true).await;
    }

    #[tokio::test]
    async fn manual_install_and_bootstrap_share_the_artifact_lock() {
        let (_dir, fake, runtime) = setup();
        let gate = fake.gate("whisper-turbo");
        runtime.speech_prepare().await.unwrap();
        wait_for("bootstrap install", || fake.installed().len() == 1).await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let manual = {
            let runtime = runtime.clone();
            tokio::spawn(async move {
                runtime
                    .model_install(
                        "whisper-turbo".into(),
                        Arc::new(move |v| sink.lock().unwrap().push(v)),
                    )
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            fake.installed(),
            ["whisper-turbo"],
            "manual install must wait"
        );
        gate.add_permits(2);
        manual.await.unwrap().unwrap();
        wait_for("ready", || status(&runtime)["ready"] == true).await;
        assert_eq!(fake.max_running.lock().unwrap()["whisper-turbo"], 1);
        assert_eq!(
            fake.installed()
                .iter()
                .filter(|id| *id == "whisper-turbo")
                .count(),
            2
        );
        // Manual progress keeps the artifact/downloaded/total contract.
        let events = events.lock().unwrap();
        assert_eq!(events[0]["artifact"], "whisper-turbo");
        assert!(events[0]["downloaded"].is_u64() && events[0]["total"].is_u64());
    }

    #[tokio::test]
    async fn manual_install_rejects_unknown_ids_and_never_cancels_text() {
        let (_dir, _fake, runtime) = setup();
        let error = runtime
            .model_install("../evil".into(), Arc::new(|_| {}))
            .await
            .unwrap_err();
        assert_eq!(error.code, "INVALID_INPUT");
        assert!(runtime.artifact_locks.lock().unwrap().is_empty());
        let cancel = CancellationToken::new();
        let (generation, _) = runtime
            .register(0, cancel.clone(), Arc::new(AtomicBool::new(false)))
            .unwrap();
        runtime
            .model_install("silero-vad".into(), Arc::new(|_| {}))
            .await
            .unwrap();
        assert!(!cancel.is_cancelled());
        assert_eq!(runtime.generation.load(Ordering::SeqCst), generation);
    }
}

#[cfg(test)]
mod notice_tests {
    use super::speech_fixture::*;
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Hardware-independent player: records calls and concurrency.
    #[derive(Default)]
    struct FakePlayer {
        calls: AtomicUsize,
        active: AtomicUsize,
        max_active: AtomicUsize,
        hold: AtomicBool,
        fail: AtomicBool,
        rates: Mutex<Vec<u32>>,
    }
    impl FakePlayer {
        fn playback(self: &Arc<Self>) -> Playback {
            let this = self.clone();
            Playback::with_player(Arc::new(move |pcm, _output, cancel, _level| {
                this.calls.fetch_add(1, Ordering::SeqCst);
                let now = this.active.fetch_add(1, Ordering::SeqCst) + 1;
                this.max_active.fetch_max(now, Ordering::SeqCst);
                this.rates.lock().unwrap().push(pcm.sample_rate);
                while this.hold.load(Ordering::SeqCst) && !cancel.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(2));
                }
                this.active.fetch_sub(1, Ordering::SeqCst);
                if cancel.is_cancelled() {
                    return Err(AppError::cancelled());
                }
                if this.fail.load(Ordering::SeqCst) {
                    return Err(AppError::new("DEVICE_UNAVAILABLE", "Saída indisponível"));
                }
                Ok(())
            }))
        }
    }
    fn pcm() -> speech::Pcm {
        speech::Pcm {
            sample_rate: 24000,
            samples: vec![0.0; 4],
        }
    }
    fn wait_until(condition: impl Fn() -> bool) -> bool {
        for _ in 0..1000 {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }
    fn runtime(player: &Arc<FakePlayer>) -> (tempfile::TempDir, Arc<Runtime>) {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeSpeech::default());
        let mut runtime = runtime_with(dir.path(), &fake);
        Arc::get_mut(&mut runtime).unwrap().playback = Arc::new(player.playback());
        (dir, runtime)
    }

    #[test]
    fn embedded_notices_decode_without_models_or_hardware() {
        for kind in [
            NoticeKind::Provider,
            NoticeKind::Models,
            NoticeKind::Capture,
            NoticeKind::Synthesis,
        ] {
            let pcm = audio::decode_wav_pcm16(kind.wav(), NOTICE_SAMPLE_RATE).unwrap();
            assert_eq!(pcm.sample_rate, 24000);
            assert!(pcm.samples.len() > 12_000, "{kind:?} is too short");
            assert!(audio::rms(&pcm.samples) > 0.005, "{kind:?} is silent");
        }
    }

    #[test]
    fn notice_kind_is_a_closed_set() {
        for (text, kind) in [
            ("provider", NoticeKind::Provider),
            ("models", NoticeKind::Models),
            ("capture", NoticeKind::Capture),
            ("synthesis", NoticeKind::Synthesis),
        ] {
            let parsed: NoticeKind = serde_json::from_value(json!(text)).unwrap();
            assert_eq!(parsed, kind);
        }
        for bad in ["Provider", "../x.wav", "", "custom"] {
            assert!(serde_json::from_value::<NoticeKind>(json!(bad)).is_err());
        }
    }

    #[test]
    fn playback_is_serialized_across_threads() {
        let player = Arc::new(FakePlayer::default());
        let playback = Arc::new(player.playback());
        let handles: Vec<_> = (0..6)
            .map(|_| {
                let playback = playback.clone();
                std::thread::spawn(move || {
                    playback
                        .play(pcm(), None, &CancellationToken::new(), &mut |_| {})
                        .unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(player.calls.load(Ordering::SeqCst), 6);
        assert_eq!(player.max_active.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn queued_playback_observes_cancellation_without_playing() {
        let player = Arc::new(FakePlayer::default());
        player.hold.store(true, Ordering::SeqCst);
        let playback = Arc::new(player.playback());
        let first_cancel = CancellationToken::new();
        let first = {
            let (playback, cancel) = (playback.clone(), first_cancel.clone());
            std::thread::spawn(move || playback.play(pcm(), None, &cancel, &mut |_| {}))
        };
        assert!(wait_until(|| player.active.load(Ordering::SeqCst) == 1));
        let queued_cancel = CancellationToken::new();
        let queued = {
            let (playback, cancel) = (playback.clone(), queued_cancel.clone());
            std::thread::spawn(move || playback.play(pcm(), None, &cancel, &mut |_| {}))
        };
        std::thread::sleep(Duration::from_millis(40));
        queued_cancel.cancel();
        assert_eq!(queued.join().unwrap().unwrap_err().code, "CANCELLED");
        assert_eq!(player.calls.load(Ordering::SeqCst), 1);
        first_cancel.cancel();
        assert_eq!(first.join().unwrap().unwrap_err().code, "CANCELLED");
        // The slot is released: a later playback runs.
        player.hold.store(false, Ordering::SeqCst);
        playback
            .play(pcm(), None, &CancellationToken::new(), &mut |_| {})
            .unwrap();
        assert_eq!(player.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn notices_play_natively_and_never_overlap() {
        let player = Arc::new(FakePlayer::default());
        let (_dir, runtime) = runtime(&player);
        let tasks: Vec<_> = [NoticeKind::Provider, NoticeKind::Models]
            .into_iter()
            .map(|kind| {
                let runtime = runtime.clone();
                tokio::spawn(async move { runtime.alert_notice(kind).await })
            })
            .collect();
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        assert_eq!(player.max_active.load(Ordering::SeqCst), 1);
        assert!(
            player
                .rates
                .lock()
                .unwrap()
                .iter()
                .all(|rate| *rate == 24000)
        );
        assert!(runtime.notice.lock().unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stop_cancels_an_active_notice() {
        let player = Arc::new(FakePlayer::default());
        player.hold.store(true, Ordering::SeqCst);
        let (_dir, runtime) = runtime(&player);
        let active = {
            let runtime = runtime.clone();
            tokio::spawn(async move { runtime.alert_notice(NoticeKind::Capture).await })
        };
        assert!(wait_until(|| player.active.load(Ordering::SeqCst) == 1));
        runtime.cancel();
        active.await.unwrap().unwrap();
        assert_eq!(player.active.load(Ordering::SeqCst), 0);
        assert!(runtime.notice.lock().unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stop_cancels_a_notice_queued_behind_ordinary_playback() {
        let player = Arc::new(FakePlayer::default());
        player.hold.store(true, Ordering::SeqCst);
        let (_dir, runtime) = runtime(&player);
        let ordinary_cancel = CancellationToken::new();
        let ordinary = {
            let (playback, cancel) = (runtime.playback.clone(), ordinary_cancel.clone());
            tokio::task::spawn_blocking(move || playback.play(pcm(), None, &cancel, &mut |_| {}))
        };
        assert!(wait_until(|| player.active.load(Ordering::SeqCst) == 1));
        let queued = {
            let runtime = runtime.clone();
            tokio::spawn(async move { runtime.alert_notice(NoticeKind::Synthesis).await })
        };
        assert!(wait_until(|| runtime.notice.lock().unwrap().is_some()));
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(player.calls.load(Ordering::SeqCst), 1);
        runtime.cancel();
        queued.await.unwrap().unwrap();
        // The notice never reached the player and did not overlap the ordinary playback.
        assert_eq!(player.calls.load(Ordering::SeqCst), 1);
        assert_eq!(player.max_active.load(Ordering::SeqCst), 1);
        ordinary_cancel.cancel();
        assert_eq!(ordinary.await.unwrap().unwrap_err().code, "CANCELLED");
    }

    #[tokio::test]
    async fn output_failure_is_structured_and_never_recurses() {
        let player = Arc::new(FakePlayer::default());
        player.fail.store(true, Ordering::SeqCst);
        let (_dir, runtime) = runtime(&player);
        let error = runtime
            .alert_notice(NoticeKind::Synthesis)
            .await
            .unwrap_err();
        assert_eq!(error.code, "DEVICE_UNAVAILABLE");
        assert_eq!(player.calls.load(Ordering::SeqCst), 1);
        assert!(runtime.notice.lock().unwrap().is_none());
    }
}

/// Real native speech bootstrap (catalog install + preload) with the network refused at the
/// transport boundary. Needs the pinned benchmark models; prints `SKIP` when they are absent.
#[cfg(all(test, feature = "local-speech"))]
mod native_bootstrap_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    const IDS: [&str; 3] = ["whisper-turbo", VAD_ARTIFACT, TTS_ARTIFACT];

    /// Hardlinks the benchmark files (selected by the manifest) into `<source>/<id>/<name>`.
    /// Returns `false` after printing why when benchmark files are absent.
    fn link_sources(bench: &std::path::Path, source: &std::path::Path) -> bool {
        let manifest = match std::fs::read(bench.join("manifest.json")) {
            Ok(bytes) => serde_json::from_slice::<Value>(&bytes).unwrap(),
            Err(_) => {
                println!("SKIP: {} not found", bench.join("manifest.json").display());
                return false;
            }
        };
        let entries = manifest["files"].as_array().expect("manifest files");
        let mut plan = Vec::new();
        for id in IDS {
            let artifact = crate::models::lookup(id).expect("catalog id");
            for file in artifact.files {
                let entry = entries
                    .iter()
                    .find(|e| e["revision"] == artifact.revision && e["file"] == file.source_file)
                    .unwrap_or_else(|| panic!("bench manifest lacks {id}/{}", file.name));
                assert_eq!(
                    entry["size"].as_u64(),
                    Some(file.size),
                    "{} size",
                    file.name
                );
                assert_eq!(
                    entry["sha256"].as_str(),
                    Some(file.sha256),
                    "{} sha256",
                    file.name
                );
                let from = bench.join(entry["path"].as_str().expect("path"));
                if !from.is_file() {
                    println!("SKIP: missing benchmark file {}", from.display());
                    return false;
                }
                plan.push((artifact.id, file.name, from));
            }
        }
        for (id, name, from) in plan {
            let dir = source.join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::hard_link(&from, dir.join(name)).unwrap_or_else(|e| {
                panic!("hardlink {} failed (cross-device?): {e}", from.display())
            });
        }
        true
    }

    #[tokio::test]
    #[ignore = "needs pinned benchmark models under tools/audio-bench/data"]
    async fn native_bootstrap_reaches_ready_offline_from_verified_local_files() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let bench = root.join("tools/audio-bench/data");
        if !bench.join("out").is_dir() {
            println!("SKIP: {} not found", bench.join("out").display());
            return;
        }
        // Same filesystem as the benchmark data so the source tree can use hardlinks.
        let dir = tempfile::Builder::new()
            .prefix("aura-bootstrap-")
            .tempdir_in(bench.join("out"))
            .unwrap();
        let source = dir.path().join("source");
        if !link_sources(&bench, &source) {
            return;
        }
        let data = dir.path().join("data");
        let mut runtime = Runtime::open(Paths::new(data.clone(), root.clone())).unwrap();

        let attempts = Arc::new(AtomicUsize::new(0));
        let installs = Arc::new(AtomicUsize::new(0));
        let (models_root, local_root) = (data.join("models"), source.clone());
        let (hook_attempts, hook_installs) = (attempts.clone(), installs.clone());
        Arc::get_mut(&mut runtime).unwrap().speech_hooks = SpeechHooks {
            supported: true,
            install: Arc::new(move |id, progress| {
                let (models_root, local_root) = (models_root.clone(), local_root.clone());
                let (attempts, installs) = (hook_attempts.clone(), hook_installs.clone());
                async move {
                    installs.fetch_add(1, Ordering::SeqCst);
                    let manifest = crate::models::install_local_only(
                        &models_root,
                        &local_root,
                        &id,
                        attempts,
                        |report| progress(serde_json::to_value(&report).unwrap_or(Value::Null)),
                    )
                    .await
                    .map_err(model_app_error)?;
                    Ok(serde_json::to_value(manifest)?)
                }
                .boxed()
            }),
            // The real preload: loads the installed VAD/ASR and TTS models.
            preload: Arc::new(|backend| {
                backend.preload_voice()?;
                backend.preload_tts()
            }),
        };

        let started = runtime.speech_prepare().await.unwrap();
        assert_eq!(started["active"], true);
        let deadline = std::time::Instant::now() + Duration::from_secs(600);
        let mut last_phase = String::new();
        let ready = loop {
            let snapshot = runtime.speech_status().unwrap();
            let phase = snapshot["phase"].as_str().unwrap_or_default().to_string();
            if phase != last_phase {
                println!("phase: {phase} artifact={}", snapshot["artifact"]);
                last_phase = phase.clone();
            }
            assert_ne!(phase, "failed", "bootstrap failed: {snapshot}");
            if snapshot["ready"] == true && snapshot["active"] == false {
                break snapshot;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timeout waiting for ready: {snapshot}"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
        println!("final: {ready}");
        assert_eq!(ready["phase"], "ready");
        assert_eq!(ready["error"], Value::Null);
        assert_eq!(ready["warning"], Value::Null);
        assert_eq!(installs.load(Ordering::SeqCst), IDS.len());
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            0,
            "network fetch attempted"
        );
        assert!(runtime.speech_job.lock().unwrap().is_none());

        // Idempotent: ready is sticky and triggers no new work.
        let again = runtime.speech_prepare().await.unwrap();
        assert_eq!(again["ready"], true);
        assert_eq!(again["active"], false);
        assert_eq!(again["generation"], ready["generation"]);
        assert_eq!(installs.load(Ordering::SeqCst), IDS.len());
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            0,
            "network fetch attempted"
        );
        println!(
            "network fetch attempts: {}",
            attempts.load(Ordering::SeqCst)
        );
    }
}
