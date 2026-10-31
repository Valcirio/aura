//! Native (in-process) speech backend, parallel to the Python `SpeechWorker`.
//!
//! * ASR (Whisper) and TTS (Kokoro) run through sherpa-onnx's bundled static ONNX Runtime.
//! * VAD is *scored*: Silero v5 runs through `ort` (rc.10, `disable-linking`), which resolves
//!   `OrtGetApiBase` from the same static `libonnxruntime.a` that sherpa-onnx links. No
//!   dynamic ONNX Runtime library is required. Probabilities mirror `workers/speech_worker.py`
//!   (512 samples + 64-sample context, recurrent state `[2, 1, 128]`).
//! * Cancellation: `cancel` bumps a generation counter without touching model mutexes. TTS stops
//!   through the sherpa callback; ASR has no interrupt callback, so decoding may finish first and
//!   its result is then discarded.
use crate::{AppError, Result, config::Paths, models, speech::Pcm};
use base64::{Engine, engine::general_purpose::STANDARD};
use ort::{session::Session, value::Tensor};
use serde_json::{Value, json};
use sherpa_onnx::{
    GenerationConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineTts, OfflineTtsConfig,
    OfflineTtsKokoroModelConfig, OfflineTtsModelConfig, OfflineWhisperModelConfig,
};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

const VAD_ID: &str = "silero-vad";
const TTS_ID: &str = "kokoro-multi-lang-v1_0";
const FALLBACK_ASR_THREADS: i32 = 8;
const VAD_FRAME: usize = 512;
const VAD_CONTEXT: usize = 64;
const VAD_STATE: usize = 2 * 128;
const MAX_ASR_SAMPLES: usize = 480_000;
const MAX_TTS_CHARS: usize = 2_000;
const MAX_TTS_SAMPLES: usize = 4_000_000;
const KOKORO_VOICE: &str = "pf_dora";
const KOKORO_SID: i32 = 42;
const PHONEMIZER_LANG: &str = "pt-br";

struct VadModel {
    session: Option<Session>,
    state: Vec<f32>,
    context: Vec<f32>,
}
impl VadModel {
    fn new() -> Self {
        Self {
            session: None,
            state: vec![0.; VAD_STATE],
            context: vec![0.; VAD_CONTEXT],
        }
    }
    fn clear(&mut self) {
        self.state.iter_mut().for_each(|v| *v = 0.);
        self.context.iter_mut().for_each(|v| *v = 0.);
    }
}

/// Whisper model used for transcription. `Turbo` is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsrModel {
    Turbo,
    Small,
}
impl AsrModel {
    fn id(self) -> &'static str {
        match self {
            AsrModel::Turbo => models::DEFAULT_ASR_ID,
            AsrModel::Small => "whisper-small",
        }
    }
    fn prefix(self) -> &'static str {
        match self {
            AsrModel::Turbo => "turbo",
            AsrModel::Small => "small",
        }
    }
    fn threads(self) -> i32 {
        models::lookup(self.id())
            .and_then(|a| a.threads)
            .and_then(|t| i32::try_from(t).ok())
            .filter(|t| *t > 0)
            .unwrap_or(FALLBACK_ASR_THREADS)
    }
}

/// True while `captured` is still the current generation (TTS callback predicate).
fn is_current(generation: &AtomicU64, captured: u64) -> bool {
    generation.load(Ordering::SeqCst) == captured
}

pub struct LocalSpeech {
    paths: Paths,
    asr_model: AsrModel,
    generation: Arc<AtomicU64>,
    asr: Mutex<Option<OfflineRecognizer>>,
    tts: Mutex<Option<OfflineTts>>,
    vad: Mutex<VadModel>,
}

fn audio_error(message: impl Into<String>) -> AppError {
    AppError::new("AUDIO_ERROR", message)
}
fn busy() -> AppError {
    AppError::new("AUDIO_ERROR", "Estado de áudio indisponível")
}
fn model_error(error: models::ModelError) -> AppError {
    match error {
        models::ModelError::NotInstalled(_) => {
            AppError::new("MODEL_NOT_INSTALLED", error.to_string())
        }
        other => AppError::new("MODEL_INVALID", other.to_string()),
    }
}
fn path_string(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| AppError::new("MODEL_INVALID", "Caminho de modelo inválido"))
}

/// Decodes little-endian f32 PCM, rejecting empty, oversized, misaligned and nonfinite input.
pub(crate) fn decode_pcm_f32le(encoded: &str, max_samples: usize) -> Result<Vec<f32>> {
    let raw = STANDARD
        .decode(encoded)
        .map_err(|_| AppError::invalid("PCM inválido"))?;
    if raw.is_empty() || raw.len() % 4 != 0 || raw.len() > max_samples * 4 {
        return Err(AppError::invalid("PCM inválido ou muito longo"));
    }
    let samples: Vec<f32> = raw
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    check_samples(&samples, max_samples)?;
    Ok(samples)
}
fn check_samples(samples: &[f32], max_samples: usize) -> Result<()> {
    if samples.is_empty() || samples.len() > max_samples {
        return Err(AppError::invalid("PCM inválido ou muito longo"));
    }
    if samples.iter().any(|v| !v.is_finite()) {
        return Err(AppError::invalid("PCM contém valores inválidos"));
    }
    Ok(())
}
pub(crate) fn validate_synthesis(text: &str, voice: &str, speed: f32) -> Result<()> {
    if text.trim().is_empty()
        || text.chars().count() > MAX_TTS_CHARS
        || !speed.is_finite()
        || !(0.85..=1.15).contains(&speed)
    {
        return Err(AppError::invalid(
            "Texto ou velocidade de síntese inválidos",
        ));
    }
    if voice != KOKORO_VOICE {
        return Err(AppError::invalid("Voz desconhecida"));
    }
    Ok(())
}
/// Kokoro IPA remap used by the benchmark: `tʃ→ʧ`, `dʒ→ʤ`.
pub(crate) fn remap_codepoints(cps: &[i32]) -> Vec<i32> {
    let s: String = cps
        .iter()
        .filter_map(|&c| u32::try_from(c).ok().and_then(char::from_u32))
        .collect();
    s.replace("tʃ", "ʧ")
        .replace("dʒ", "ʤ")
        .chars()
        .map(|c| c as i32)
        .collect()
}
fn encode_s16le(samples: &[f32]) -> String {
    let mut raw = Vec::with_capacity(samples.len() * 2);
    for v in samples {
        let q = (v.clamp(-1., 1.) * 32767.).round() as i16;
        raw.extend_from_slice(&q.to_le_bytes());
    }
    STANDARD.encode(raw)
}

impl LocalSpeech {
    pub fn new(paths: Paths) -> Self {
        Self::with_asr_model(paths, AsrModel::Turbo)
    }
    /// Selects the ASR model; nothing is loaded until the first transcription.
    pub fn with_asr_model(paths: Paths, model: AsrModel) -> Self {
        Self {
            paths,
            asr_model: model,
            generation: Arc::new(AtomicU64::new(0)),
            asr: Mutex::new(None),
            tts: Mutex::new(None),
            vad: Mutex::new(VadModel::new()),
        }
    }
    /// Invalidates in-flight operations. Never waits for a model mutex.
    pub fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
    fn check(&self, captured: u64) -> Result<()> {
        if !is_current(&self.generation, captured) {
            Err(AppError::cancelled())
        } else {
            Ok(())
        }
    }
    /// Waits for a model mutex, then re-checks cancellation.
    fn lock_checked<'a, T>(
        &self,
        mutex: &'a Mutex<T>,
        captured: u64,
    ) -> Result<std::sync::MutexGuard<'a, T>> {
        let guard = mutex.lock().map_err(|_| busy())?;
        self.check(captured)?;
        Ok(guard)
    }
    fn artifact(&self, id: &str, file: &str) -> Result<String> {
        path_string(&models::artifact_path(&self.paths.models(), id, file).map_err(model_error)?)
    }

    pub fn status(&self) -> Result<Value> {
        Ok(serde_json::to_value(models::status(&self.paths.models()))?)
    }

    pub fn vad_reset(&self) -> Result<()> {
        self.vad.lock().map_err(|_| busy())?.clear();
        Ok(())
    }

    /// Silero v5 speech probability for exactly 512 finite 16-kHz samples.
    pub fn vad(&self, samples: &[f32]) -> Result<f32> {
        if samples.len() != VAD_FRAME {
            return Err(AppError::invalid(
                "VAD requer exatamente 512 amostras a 16 kHz",
            ));
        }
        let captured = self.generation.load(Ordering::SeqCst);
        check_samples(samples, VAD_FRAME)?;
        let mut vad = self.lock_checked(&self.vad, captured)?;
        if vad.session.is_none() {
            let path = self.artifact(VAD_ID, "silero_vad.onnx")?;
            let session = Session::builder()
                .and_then(|b| b.with_intra_threads(1))
                .and_then(|b| b.with_inter_threads(1))
                .and_then(|b| b.commit_from_file(path))
                .map_err(|e| audio_error(format!("Falha ao carregar VAD: {e}")))?;
            vad.session = Some(session);
        }
        self.check(captured)?;
        let mut joined = Vec::with_capacity(VAD_CONTEXT + VAD_FRAME);
        joined.extend_from_slice(&vad.context);
        joined.extend_from_slice(samples);
        let fail = |e: ort::Error| audio_error(format!("Falha na inferência de VAD: {e}"));
        let input = Tensor::from_array(([1usize, VAD_CONTEXT + VAD_FRAME], joined.clone()))
            .map_err(fail)?;
        let state = Tensor::from_array(([2usize, 1, 128], vad.state.clone())).map_err(fail)?;
        let sr = Tensor::from_array(((), vec![16000i64])).map_err(fail)?;
        let session = vad.session.as_mut().ok_or_else(busy)?;
        let (probability, next_state) = {
            let out = session
                .run(ort::inputs!["input" => input, "state" => state, "sr" => sr])
                .map_err(fail)?;
            let (_, p) = out[0].try_extract_tensor::<f32>().map_err(fail)?;
            let (_, s) = out[1].try_extract_tensor::<f32>().map_err(fail)?;
            (validate_vad_output(p, s)?, s.to_vec())
        };
        self.check(captured)?;
        vad.state = next_state;
        vad.context = joined[joined.len() - VAD_CONTEXT..].to_vec();
        Ok(probability)
    }

    /// Portuguese transcription with the selected Whisper model (default `whisper-turbo`).
    pub fn transcribe(&self, samples: &[f32]) -> Result<String> {
        let captured = self.generation.load(Ordering::SeqCst);
        check_samples(samples, MAX_ASR_SAMPLES)?;
        self.check(captured)?;
        let mut guard = self.lock_checked(&self.asr, captured)?;
        if guard.is_none() {
            let (id, prefix) = (self.asr_model.id(), self.asr_model.prefix());
            let encoder = self.artifact(id, &format!("{prefix}-encoder.int8.onnx"))?;
            let decoder = self.artifact(id, &format!("{prefix}-decoder.int8.onnx"))?;
            let tokens = self.artifact(id, &format!("{prefix}-tokens.txt"))?;
            let mut cfg = OfflineRecognizerConfig::default();
            cfg.model_config.whisper = OfflineWhisperModelConfig {
                encoder: Some(encoder),
                decoder: Some(decoder),
                language: Some("pt".into()),
                task: Some("transcribe".into()),
                tail_paddings: 0,
                enable_token_timestamps: false,
                enable_segment_timestamps: false,
            };
            cfg.model_config.tokens = Some(tokens);
            cfg.model_config.provider = Some("cpu".into());
            cfg.model_config.debug = false;
            cfg.model_config.num_threads = self.asr_model.threads();
            *guard = Some(
                OfflineRecognizer::create(&cfg)
                    .ok_or_else(|| audio_error("Falha ao carregar o reconhecedor"))?,
            );
        }
        self.check(captured)?;
        let recognizer = guard.as_ref().ok_or_else(busy)?;
        let stream = recognizer.create_stream();
        stream.accept_waveform(16000, samples);
        recognizer.decode(&stream);
        let result = stream.get_result();
        self.check(captured)?;
        let result = result.ok_or_else(|| audio_error("Reconhecimento sem resultado"))?;
        Ok(result.text.trim().to_string())
    }

    /// Kokoro `pf_dora` synthesis (sid 42) with benchmark IPA remapping.
    pub fn synthesize(&self, text: &str, voice: &str, speed: f32) -> Result<Pcm> {
        let captured = self.generation.load(Ordering::SeqCst);
        validate_synthesis(text, voice, speed)?;
        self.check(captured)?;
        let mut guard = self.lock_checked(&self.tts, captured)?;
        if guard.is_none() {
            let cfg = OfflineTtsConfig {
                model: OfflineTtsModelConfig {
                    num_threads: 4,
                    debug: false,
                    provider: Some("cpu".into()),
                    kokoro: OfflineTtsKokoroModelConfig {
                        model: Some(self.artifact(TTS_ID, "model.onnx")?),
                        voices: Some(self.artifact(TTS_ID, "voices.bin")?),
                        tokens: Some(self.artifact(TTS_ID, "tokens.txt")?),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            };
            *guard = Some(
                OfflineTts::create(&cfg).ok_or_else(|| audio_error("Falha ao carregar a voz"))?,
            );
        }
        self.check(captured)?;
        let tts = guard.as_ref().ok_or_else(busy)?;
        let phonemes = piper_phonemize::phonemize(text, PHONEMIZER_LANG)
            .ok_or_else(|| audio_error("Falha ao fonemizar o texto"))?;
        let mut sentences = Vec::new();
        for i in 0..phonemes.num_sentences() {
            let cps = phonemes.get_phonemes(i).unwrap_or_default();
            sentences.push(remap_codepoints(&cps));
        }
        drop(phonemes);
        if sentences.iter().all(|s| s.is_empty()) {
            return Err(audio_error("O fonemizador não retornou fonemas"));
        }
        self.check(captured)?;
        let config = GenerationConfig {
            sid: KOKORO_SID,
            speed,
            phoneme_codepoints: Some(sentences),
            ..Default::default()
        };
        let generation = Arc::clone(&self.generation);
        let audio = tts.generate_with_config(
            "",
            &config,
            Some(move |_: &[f32], _: f32| is_current(&generation, captured)),
        );
        self.check(captured)?;
        let audio = audio.ok_or_else(|| audio_error("Falha na síntese de voz"))?;
        let rate = u32::try_from(audio.sample_rate())
            .ok()
            .filter(|r| (8000..=96000).contains(r))
            .ok_or_else(|| audio_error("Frequência inválida"))?;
        let samples = audio.samples().to_vec();
        if samples.is_empty() || samples.len() > MAX_TTS_SAMPLES {
            return Err(audio_error("Áudio sintetizado vazio ou muito longo"));
        }
        if samples.iter().any(|v| !v.is_finite()) {
            return Err(audio_error("Áudio sintetizado inválido"));
        }
        self.check(captured)?;
        Ok(Pcm {
            sample_rate: rate,
            samples,
        })
    }

    /// JSON command entry point using the `SpeechWorker` wire shapes.
    pub fn request(&self, command: Value, mut progress: impl FnMut(Value)) -> Result<Value> {
        let kind = command
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::invalid("Comando de áudio ausente"))?;
        let text_field = |name: &str| {
            command
                .get(name)
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::invalid(format!("Campo '{name}' ausente ou inválido")))
        };
        match kind {
            "status" => self.status(),
            "install" => self.install(text_field("artifact")?, &mut progress),
            "vad_reset" => {
                self.vad_reset()?;
                Ok(json!({"ready": true}))
            }
            "vad" => {
                let samples = decode_pcm_f32le(text_field("pcm_f32le")?, VAD_FRAME)?;
                Ok(json!({"probability": self.vad(&samples)?}))
            }
            "transcribe" => {
                let samples = decode_pcm_f32le(text_field("pcm_f32le")?, MAX_ASR_SAMPLES)?;
                Ok(json!({"text": self.transcribe(&samples)?}))
            }
            "synthesize" => {
                let speed = command
                    .get("speed")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| AppError::invalid("Campo 'speed' ausente ou inválido"))?;
                let pcm =
                    self.synthesize(text_field("text")?, text_field("voice")?, speed as f32)?;
                Ok(json!({
                    "sample_rate": pcm.sample_rate,
                    "pcm_s16le": encode_s16le(&pcm.samples),
                }))
            }
            _ => Err(AppError::invalid("Comando de áudio desconhecido")),
        }
    }

    fn install(&self, artifact: &str, progress: &mut impl FnMut(Value)) -> Result<Value> {
        let root = self.paths.models();
        let manifest = forward_progress(
            |tx| {
                let tx = Mutex::new(tx);
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| audio_error("Runtime de instalação indisponível"))?;
                runtime
                    .block_on(models::install(&root, artifact, |p| {
                        if let Ok(tx) = tx.lock() {
                            let _ = tx.send(p);
                        }
                    }))
                    .map_err(model_error)
            },
            progress,
        )?;
        Ok(serde_json::to_value(manifest)?)
    }
}

/// Runs `worker` on a scoped thread that solely owns the progress sender, forwarding
/// events until the channel closes, then joins and returns the worker's result.
fn forward_progress<T: Send>(
    worker: impl FnOnce(std::sync::mpsc::Sender<models::Progress>) -> Result<T> + Send,
    progress: &mut impl FnMut(Value),
) -> Result<T> {
    let (tx, rx) = std::sync::mpsc::channel::<models::Progress>();
    let outcome = std::thread::scope(|scope| {
        let handle = scope.spawn(move || worker(tx));
        for event in rx.iter() {
            progress(serde_json::to_value(event).unwrap_or(Value::Null));
        }
        handle.join()
    });
    outcome.map_err(|_| audio_error("Instalação interrompida"))?
}

fn validate_vad_output(probability: &[f32], next_state: &[f32]) -> Result<f32> {
    if probability.len() != 1
        || next_state.len() != VAD_STATE
        || !probability[0].is_finite()
        || !(0.0..=1.0).contains(&probability[0])
        || next_state.iter().any(|v| !v.is_finite())
    {
        return Err(audio_error("Saída de VAD inválida"));
    }
    Ok(probability[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speech(dir: &Path) -> LocalSpeech {
        LocalSpeech::new(Paths::new(dir.to_path_buf(), dir.to_path_buf()))
    }
    fn b64_f32(values: &[f32]) -> String {
        STANDARD.encode(
            values
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        )
    }

    fn run_forward(fail: bool) -> (Vec<Value>, std::result::Result<u32, AppError>) {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut events = Vec::new();
            let result = forward_progress(
                |tx| {
                    for downloaded in 1..=3u64 {
                        tx.send(models::Progress {
                            artifact: "a".into(),
                            downloaded,
                            total: 3,
                        })
                        .unwrap();
                    }
                    if fail {
                        Err(audio_error("falha"))
                    } else {
                        Ok(7u32)
                    }
                },
                &mut |v| events.push(v),
            );
            let _ = done_tx.send((events, result));
        });
        done_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("forward_progress did not terminate")
    }

    #[test]
    fn forward_progress_delivers_events_and_success() {
        let (events, result) = run_forward(false);
        assert_eq!(result.unwrap(), 7);
        let got: Vec<u64> = events
            .iter()
            .map(|e| e["downloaded"].as_u64().unwrap())
            .collect();
        assert_eq!(got, [1, 2, 3]);
    }

    #[test]
    fn forward_progress_delivers_events_and_error() {
        let (events, result) = run_forward(true);
        assert!(result.is_err());
        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["downloaded"], 1);
        assert_eq!(events[2]["downloaded"], 3);
    }

    #[test]
    fn vad_output_validation() {
        let state = vec![0.0f32; VAD_STATE];
        assert_eq!(validate_vad_output(&[0.5], &state).unwrap(), 0.5);
        assert_eq!(validate_vad_output(&[0.0], &state).unwrap(), 0.0);
        assert_eq!(validate_vad_output(&[1.0], &state).unwrap(), 1.0);
        assert!(validate_vad_output(&[1.5], &state).is_err());
        assert!(validate_vad_output(&[-0.1], &state).is_err());
        assert!(validate_vad_output(&[f32::NAN], &state).is_err());
        assert!(validate_vad_output(&[0.5, 0.5], &state).is_err());
        assert!(validate_vad_output(&[0.5], &state[1..]).is_err());
        let mut bad = state.clone();
        bad[3] = f32::INFINITY;
        assert!(validate_vad_output(&[0.5], &bad).is_err());
        bad[3] = f32::NAN;
        assert!(validate_vad_output(&[0.5], &bad).is_err());
    }

    #[test]
    fn pcm_decoding_enforces_bounds() {
        assert_eq!(
            decode_pcm_f32le(&b64_f32(&[0.5, -0.5]), 4).unwrap(),
            [0.5, -0.5]
        );
        assert!(decode_pcm_f32le("", 4).is_err());
        assert!(decode_pcm_f32le("!!!", 4).is_err());
        assert!(decode_pcm_f32le(&STANDARD.encode([0u8; 3]), 4).is_err());
        assert!(decode_pcm_f32le(&b64_f32(&[0.; 5]), 4).is_err());
        assert!(decode_pcm_f32le(&b64_f32(&[f32::NAN]), 4).is_err());
        assert!(decode_pcm_f32le(&b64_f32(&[f32::INFINITY]), 4).is_err());
    }

    #[test]
    fn synthesis_validation_checks_voice_text_and_speed() {
        assert!(validate_synthesis("Olá", "pf_dora", 1.0).is_ok());
        assert!(validate_synthesis("Olá", "pf_dora", 0.85).is_ok());
        assert!(validate_synthesis("Olá", "pf_dora", 1.15).is_ok());
        assert!(validate_synthesis("Olá", "pt_BR-cadu-medium", 1.0).is_err());
        assert!(validate_synthesis("  ", "pf_dora", 1.0).is_err());
        assert!(validate_synthesis("Olá", "pf_dora", 0.84).is_err());
        assert!(validate_synthesis("Olá", "pf_dora", 1.16).is_err());
        assert!(validate_synthesis("Olá", "pf_dora", f32::NAN).is_err());
        assert!(validate_synthesis(&"á".repeat(2000), "pf_dora", 1.0).is_ok());
        assert!(validate_synthesis(&"á".repeat(2001), "pf_dora", 1.0).is_err());
    }

    #[test]
    fn remap_replaces_affricates() {
        let input: Vec<i32> = "tʃa dʒi x".chars().map(|c| c as i32).collect();
        let out: String = remap_codepoints(&input)
            .iter()
            .map(|&c| char::from_u32(c as u32).unwrap())
            .collect();
        assert_eq!(out, "ʧa ʤi x");
    }

    #[test]
    fn cancel_bumps_generation_and_invalidates_captured() {
        let dir = tempfile::tempdir().unwrap();
        let s = speech(dir.path());
        let captured = s.generation.load(Ordering::SeqCst);
        assert!(s.check(captured).is_ok());
        s.cancel();
        assert_eq!(s.check(captured).unwrap_err().code, "CANCELLED");
        assert!(s.check(s.generation.load(Ordering::SeqCst)).is_ok());
    }

    #[test]
    fn cancel_does_not_wait_for_model_mutex() {
        let dir = tempfile::tempdir().unwrap();
        let s = speech(dir.path());
        let _asr = s.asr.lock().unwrap();
        let _tts = s.tts.lock().unwrap();
        s.cancel();
    }

    #[test]
    fn request_rejects_malformed_and_unsupported_commands() {
        let dir = tempfile::tempdir().unwrap();
        let s = speech(dir.path());
        let call = |v: Value| s.request(v, |_| {});
        assert!(call(json!({})).is_err());
        assert!(call(json!({"command": 3})).is_err());
        assert!(call(json!({"command": "nope"})).is_err());
        assert!(call(json!({"command": "install"})).is_err());
        assert!(call(json!({"command": "vad"})).is_err());
        assert!(call(json!({"command": "transcribe", "pcm_f32le": ""})).is_err());
        let short = b64_f32(&[0.; 10]);
        assert!(call(json!({"command": "vad", "pcm_f32le": short})).is_err());
        assert!(call(json!({"command": "synthesize", "text": "a", "voice": "pf_dora"})).is_err());
        assert!(
            call(json!({"command": "synthesize", "text": "a", "voice": "pt_BR-cadu-medium", "speed": 1.0}))
                .is_err()
        );
    }

    #[test]
    fn missing_models_report_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let s = speech(dir.path());
        let err = s.vad(&[0.; VAD_FRAME]).unwrap_err();
        assert_eq!(err.code, "MODEL_NOT_INSTALLED");
        assert_eq!(
            s.transcribe(&[0.1; 160]).unwrap_err().code,
            "MODEL_NOT_INSTALLED"
        );
        assert_eq!(
            s.synthesize("Olá", "pf_dora", 1.0).err().unwrap().code,
            "MODEL_NOT_INSTALLED"
        );
        assert!(s.vad(&[0.; 10]).is_err());
        assert!(s.vad(&[f32::NAN; VAD_FRAME]).is_err());
    }

    #[test]
    fn status_with_empty_model_root() {
        let dir = tempfile::tempdir().unwrap();
        let s = speech(dir.path());
        let value = s.request(json!({"command": "status"}), |_| {}).unwrap();
        let artifacts = value["artifacts"].as_array().unwrap();
        assert!(!artifacts.is_empty());
        assert!(artifacts.iter().all(|a| a["installed"] == false));
        assert!(value["packages"].as_object().unwrap().is_empty());
    }

    #[test]
    fn vad_reset_without_model_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let s = speech(dir.path());
        s.vad_reset().unwrap();
        assert_eq!(
            s.request(json!({"command": "vad_reset"}), |_| {}).unwrap(),
            json!({"ready": true})
        );
    }

    #[test]
    fn default_asr_is_turbo_and_small_is_lazy() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(speech(dir.path()).asr_model, AsrModel::Turbo);
        assert_eq!(AsrModel::Turbo.threads(), 8);
        assert_eq!(AsrModel::Small.threads(), 4);
        let s = LocalSpeech::with_asr_model(
            Paths::new(dir.path().to_path_buf(), dir.path().to_path_buf()),
            AsrModel::Small,
        );
        assert_eq!(s.asr_model, AsrModel::Small);
        assert!(
            s.asr.lock().unwrap().is_none(),
            "construction must not load a model"
        );
        let err = s.transcribe(&[0.1; 160]).unwrap_err();
        assert_eq!(err.code, "MODEL_NOT_INSTALLED");
        assert_eq!(
            err.message,
            "Instale whisper-small em Configurações > Modelos locais"
        );
        assert!(s.asr.lock().unwrap().is_none());
    }

    #[test]
    fn missing_model_errors_name_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let s = speech(dir.path());
        let msg = |id: &str| format!("Instale {id} em Configurações > Modelos locais");
        let e = s.vad(&[0.; VAD_FRAME]).unwrap_err();
        assert_eq!(
            (e.code.as_str(), e.message),
            ("MODEL_NOT_INSTALLED", msg("silero-vad"))
        );
        let e = s.transcribe(&[0.1; 160]).unwrap_err();
        assert_eq!(
            (e.code.as_str(), e.message),
            ("MODEL_NOT_INSTALLED", msg("whisper-turbo"))
        );
        let e = s.synthesize("Olá", "pf_dora", 1.0).err().unwrap();
        assert_eq!(
            (e.code.as_str(), e.message),
            ("MODEL_NOT_INSTALLED", msg("kokoro-multi-lang-v1_0"))
        );
    }

    #[test]
    fn model_lock_waiter_observes_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let s = speech(dir.path());
        let captured = s.generation.load(Ordering::SeqCst);
        let held = s.asr.lock().unwrap();
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| s.lock_checked(&s.asr, captured).map(|_| ()));
            s.cancel();
            drop(held);
            assert_eq!(waiter.join().unwrap().unwrap_err().code, "CANCELLED");
        });
        // A waiter holding the current generation still succeeds.
        let current = s.generation.load(Ordering::SeqCst);
        assert!(s.lock_checked(&s.asr, current).is_ok());
    }

    #[test]
    fn tts_callback_predicate_follows_generation() {
        let generation = AtomicU64::new(7);
        assert!(is_current(&generation, 7));
        generation.fetch_add(1, Ordering::SeqCst);
        assert!(!is_current(&generation, 7));
        assert!(is_current(&generation, 8));
    }

    // ---- Model-backed tests (ignored). Fixtures hardlink the pinned benchmark models from
    // tools/audio-bench/data into a temporary install tree; no model is ever copied.
    use sha2::{Digest, Sha256};
    use std::io::Read;

    struct Fixture {
        _dir: tempfile::TempDir,
        paths: Paths,
        root: std::path::PathBuf,
    }
    impl Fixture {
        fn speech(&self, model: AsrModel) -> LocalSpeech {
            LocalSpeech::with_asr_model(self.paths.clone(), model)
        }
    }

    fn sha256_file(path: &Path) -> (u64, String) {
        let mut file = std::fs::File::open(path).unwrap();
        let (mut hasher, mut total, mut buf) = (Sha256::new(), 0u64, vec![0u8; 1 << 20]);
        loop {
            let n = file.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            total += n as u64;
        }
        let hex: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        (total, hex)
    }

    /// Builds an install tree for `ids`. Returns `None` (after printing why) only when benchmark
    /// source files are absent; any size/hash/manifest disagreement or link failure panics
    /// before a manifest is written.
    fn fixture(ids: &[&str]) -> Option<Fixture> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let root = root.canonicalize().unwrap();
        let bench = root.join("tools/audio-bench/data");
        let bench_manifest = match std::fs::read(bench.join("manifest.json")) {
            Ok(bytes) => serde_json::from_slice::<Value>(&bytes).unwrap(),
            Err(_) => {
                println!("SKIP: {} not found", bench.join("manifest.json").display());
                return None;
            }
        };
        let entries = bench_manifest["files"].as_array().expect("manifest files");
        // (artifact, catalog file, source path)
        let mut plan = Vec::new();
        for id in ids {
            let artifact = models::lookup(id).expect("catalog id");
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
                let src = bench.join(entry["path"].as_str().expect("path"));
                if !src.is_file() {
                    println!("SKIP: missing benchmark file {}", src.display());
                    return None;
                }
                plan.push((artifact, file, src));
            }
        }
        for (_, file, src) in &plan {
            let (size, hash) = sha256_file(src);
            assert_eq!(size, file.size, "{} size on disk", file.name);
            assert_eq!(hash, file.sha256, "{} sha256 on disk", file.name);
        }
        // Same filesystem as the benchmark data so hardlinks work.
        let dir = tempfile::Builder::new()
            .prefix("aura-fixture-")
            .tempdir_in(bench.join("out"))
            .expect("fixture dir");
        for id in ids {
            let artifact = models::lookup(id).unwrap();
            let rev_dir = dir
                .path()
                .join("models")
                .join(artifact.id)
                .join(artifact.revision);
            std::fs::create_dir_all(&rev_dir).unwrap();
            for (_, file, src) in plan.iter().filter(|(a, _, _)| a.id == artifact.id) {
                std::fs::hard_link(src, rev_dir.join(file.name)).unwrap_or_else(|e| {
                    panic!("hardlink {} failed (cross-device?): {e}", src.display())
                });
            }
            let manifest = models::InstallManifest {
                id: artifact.id.into(),
                revision: artifact.revision.into(),
                license: artifact.license.into(),
                files: artifact
                    .files
                    .iter()
                    .map(|f| models::FileRecord {
                        file: f.name.into(),
                        source: f.source_file.into(),
                        sha256: f.sha256.into(),
                        size: f.size,
                    })
                    .collect(),
            };
            let bytes = serde_json::to_vec(&manifest).unwrap();
            std::fs::write(rev_dir.join("manifest.json"), &bytes).unwrap();
            std::fs::write(
                dir.path()
                    .join("models")
                    .join(artifact.id)
                    .join("current.json"),
                &bytes,
            )
            .unwrap();
        }
        let mut paths = Paths::new(dir.path().to_path_buf(), root.clone());
        paths.python = root.join("workers/python/bin/python3");
        Some(Fixture {
            _dir: dir,
            paths,
            root,
        })
    }

    /// Minimal RIFF reader: asserts 16-kHz mono 16-bit PCM and returns normalized samples.
    fn load_wav_16k_mono(path: &Path) -> Vec<f32> {
        let b = std::fs::read(path).unwrap();
        assert!(b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WAVE");
        let (mut pos, mut format, mut data) = (12, None, None);
        while pos + 8 <= b.len() {
            let size = u32::from_le_bytes(b[pos + 4..pos + 8].try_into().unwrap()) as usize;
            let body = pos + 8..(pos + 8 + size).min(b.len());
            match &b[pos..pos + 4] {
                b"fmt " => {
                    let f = &b[body.clone()];
                    format = Some((
                        u16::from_le_bytes([f[0], f[1]]),
                        u16::from_le_bytes([f[2], f[3]]),
                        u32::from_le_bytes([f[4], f[5], f[6], f[7]]),
                        u16::from_le_bytes([f[14], f[15]]),
                    ));
                }
                b"data" => data = Some(body.clone()),
                _ => {}
            }
            pos = body.end + (size & 1);
        }
        assert_eq!(
            format,
            Some((1, 1, 16000, 16)),
            "expected 16-kHz mono PCM16"
        );
        b[data.expect("data chunk")]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.)
            .collect()
    }

    fn synthetic_frames(count: usize) -> Vec<Vec<f32>> {
        let mut seed = 12345u32;
        (0..count)
            .map(|f| {
                (0..VAD_FRAME)
                    .map(|i| {
                        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                        let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
                        let t = (f * VAD_FRAME + i) as f32 / 16000.;
                        let voiced = if (f / 8) % 2 == 0 { 0.4 } else { 0.0 };
                        voiced
                            * (2. * std::f32::consts::PI * 180. * t).sin()
                            * (1. + (t * 3.).sin())
                            + 0.02 * noise
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    #[ignore = "requires pinned benchmark models under tools/audio-bench/data"]
    fn vad_native_reset_and_python_parity() {
        let Some(fx) = fixture(&["silero-vad"]) else {
            return;
        };
        let native = fx.speech(AsrModel::Turbo);
        let frames = synthetic_frames(40);
        let mut passes: Vec<Vec<f32>> = Vec::new();
        for _ in 0..2 {
            native.vad_reset().unwrap();
            passes.push(frames.iter().map(|f| native.vad(f).unwrap()).collect());
        }
        assert!(
            passes[0]
                .iter()
                .all(|p| p.is_finite() && (0. ..=1.).contains(p))
        );
        for (i, (a, b)) in passes[0].iter().zip(&passes[1]).enumerate() {
            assert!(
                (a - b).abs() < 1e-4,
                "frame {i}: first pass {a}, after reset {b}"
            );
        }
        let min = passes[0].iter().cloned().fold(f32::MAX, f32::min);
        let max = passes[0].iter().cloned().fold(f32::MIN, f32::max);
        println!("VAD native probability range: min {min:.6} max {max:.6}");
        assert!(max - min > 1e-3, "probabilities should vary across frames");

        let python = crate::speech::SpeechWorker::new(fx.paths.clone());
        python.vad_reset().ok();
        match python.vad(&frames[0]) {
            Err(e) => println!(
                "PYTHON PARITY UNVERIFIED: bundled worker unavailable ({}: {})",
                e.code, e.message
            ),
            Ok(_) => {
                python.vad_reset().unwrap();
                native.vad_reset().unwrap();
                for (i, frame) in frames.iter().enumerate() {
                    let a = native.vad(frame).unwrap();
                    let b = python.vad(frame).unwrap();
                    assert!((a - b).abs() < 1e-4, "frame {i}: native {a} python {b}");
                }
                println!(
                    "PYTHON PARITY VERIFIED over {} frames (tolerance 1e-4)",
                    frames.len()
                );
            }
        }
    }

    fn asr_case(model: AsrModel, must_match: bool) {
        let Some(fx) = fixture(&[model.id()]) else {
            return;
        };
        let wav = fx.root.join("tools/audio-bench/data/corpus/01.wav");
        if !wav.is_file() {
            println!("SKIP: missing corpus file {}", wav.display());
            return;
        }
        let samples = load_wav_16k_mono(&wav);
        let s = fx.speech(model);
        assert!(s.asr.lock().unwrap().is_none());
        let text = s.transcribe(&samples).unwrap();
        println!("ASR {model:?}: {} input samples -> {text:?}", samples.len());
        assert!(s.asr.lock().unwrap().is_some());
        assert!(
            text.chars().filter(|c| c.is_alphabetic()).count() >= 5,
            "text: {text:?}"
        );
        if must_match {
            let lower = text.to_lowercase();
            assert!(
                lower.contains("mesa") || lower.contains("quatro"),
                "text: {text:?}"
            );
        }
        s.cancel();
        let captured = s.generation.load(Ordering::SeqCst);
        assert!(s.check(captured).is_ok());
    }

    #[test]
    #[ignore = "requires pinned whisper-turbo and corpus under tools/audio-bench/data"]
    fn asr_turbo_transcribes_corpus_speech() {
        asr_case(AsrModel::Turbo, true);
    }

    #[test]
    #[ignore = "requires pinned whisper-small and corpus under tools/audio-bench/data"]
    fn asr_small_transcribes_corpus_speech() {
        asr_case(AsrModel::Small, false);
    }

    #[test]
    #[ignore = "requires pinned kokoro-multi-lang-v1_0 under tools/audio-bench/data"]
    fn tts_smoke_and_request_shape() {
        let Some(fx) = fixture(&["kokoro-multi-lang-v1_0"]) else {
            return;
        };
        let s = fx.speech(AsrModel::Turbo);
        let pcm = s.synthesize("Olá, tudo bem?", "pf_dora", 1.0).unwrap();
        println!("TTS: {} Hz, {} samples", pcm.sample_rate, pcm.samples.len());
        assert!(pcm.samples.len() > 4000);
        let value = s
            .request(
                json!({"command":"synthesize","text":"Olá.","voice":"pf_dora","speed":1.0}),
                |_| {},
            )
            .unwrap();
        assert!(value["sample_rate"].as_u64().is_some());
        assert!(!value["pcm_s16le"].as_str().unwrap().is_empty());
    }
}
