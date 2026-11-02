use crate::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Ollama,
    CompatibleApi,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    pub base_url: String,
    pub model: String,
    pub context_tokens: u32,
}
impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            kind: ProviderKind::Ollama,
            base_url: "http://127.0.0.1:11434".into(),
            model: "qwen3:4b".into(),
            context_tokens: 8192,
        }
    }
}
impl ProviderConfig {
    pub fn validate(&self) -> Result<()> {
        let u = url::Url::parse(&self.base_url)
            .map_err(|_| AppError::invalid("Endereço de provedor inválido"))?;
        let local = matches!(
            u.host_str(),
            Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
        );
        if !(u.scheme() == "https" || (u.scheme() == "http" && local))
            || !u.username().is_empty()
            || u.password().is_some()
            || u.query().is_some()
            || u.fragment().is_some()
        {
            return Err(AppError::invalid(
                "Use HTTPS para APIs remotas ou HTTP em loopback, sem credenciais na URL",
            ));
        }
        if self.model.trim().is_empty()
            || self.model.len() > 200
            || self.context_tokens < 8192
            || self.context_tokens > 131072
        {
            return Err(AppError::invalid(
                "Escolha um modelo e contexto de 8192 a 131072 tokens",
            ));
        }
        Ok(())
    }
    pub fn endpoint(&self, suffix: &str) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            suffix.trim_start_matches('/')
        )
    }
    pub fn secret_key(&self) -> String {
        use sha2::{Digest, Sha256};
        format!("provider:{:x}", Sha256::digest(self.base_url.as_bytes()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub service_id: String,
    pub provider: ProviderConfig,
    pub voice: String,
    pub asr_model: String,
    pub speed: f32,
    pub time_zone: String,
    pub calendar_id: String,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub speak_text: bool,
    pub google_client_id: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            service_id: "google_calendar".into(),
            provider: ProviderConfig::default(),
            voice: "pf_dora".into(),
            asr_model: "turbo".into(),
            speed: 1.,
            time_zone: jiff::tz::TimeZone::system()
                .iana_name()
                .unwrap_or("America/Bahia")
                .into(),
            calendar_id: "primary".into(),
            input_device: None,
            output_device: None,
            speak_text: true,
            google_client_id: String::new(),
        }
    }
}
impl Settings {
    /// Maps legacy Piper voice identifiers found in persisted settings to the native voice.
    pub fn migrate_loaded(&mut self) {
        if ["pt_BR-cadu-medium", "pt_BR-faber-medium"].contains(&self.voice.as_str()) {
            self.voice = "pf_dora".into();
        }
    }
    pub fn validate(&self) -> Result<()> {
        self.provider.validate()?;
        jiff::tz::TimeZone::get(&self.time_zone)
            .map_err(|_| AppError::invalid("Fuso desconhecido"))?;
        if self.voice != "pf_dora"
            || !["turbo", "small"].contains(&self.asr_model.as_str())
            || !(0.85..=1.15).contains(&self.speed)
            || !self.speed.is_finite()
        {
            return Err(AppError::invalid(
                "Voz, modelo de transcrição ou velocidade inválida",
            ));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct Paths {
    pub data: PathBuf,
    pub resources: PathBuf,
    pub python: PathBuf,
}
impl Paths {
    pub fn new(data: PathBuf, resources: PathBuf) -> Self {
        let python = std::env::var_os("AURA_PYTHON")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let bundled = resources.join("workers/python/bin/python3");
                if bundled.exists() {
                    bundled
                } else {
                    let dev = resources.join(".venv/bin/python");
                    if dev.exists() {
                        dev
                    } else {
                        PathBuf::from("python3")
                    }
                }
            });
        Self {
            data,
            resources,
            python,
        }
    }
    pub fn models(&self) -> PathBuf {
        self.data.join("models")
    }
    /// Local directory with `<artifact id>/<file>` copies that may seed model
    /// installation (always verified against the catalog before use). Never a URL.
    pub fn model_source_root(&self) -> Option<PathBuf> {
        if let Some(root) = std::env::var_os("AURA_MODEL_SOURCE_ROOT") {
            return Some(PathBuf::from(root));
        }
        let bench = self.resources.join("tools/audio-bench/data/models");
        bench.is_dir().then_some(bench)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn never_sends_credentials_to_insecure_remote_provider() {
        let mut c = ProviderConfig::default();
        c.base_url = "http://example.com/v1".into();
        assert!(c.validate().is_err());
        c.base_url = "https://user:secret@example.com/v1".into();
        assert!(c.validate().is_err());
        c.base_url = "https://example.com/v1".into();
        assert!(c.validate().is_ok());
    }
    #[test]
    fn missing_fields_use_defaults() {
        let s: Settings = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(s.voice, "pf_dora");
        assert_eq!(s.asr_model, "turbo");
        let s: Settings =
            serde_json::from_value(serde_json::json!({"voice":"pt_BR-faber-medium"})).unwrap();
        assert_eq!(s.asr_model, "turbo");
    }
    #[test]
    fn legacy_voices_migrate_on_load_only() {
        for legacy in ["pt_BR-cadu-medium", "pt_BR-faber-medium"] {
            let mut s = Settings {
                voice: legacy.into(),
                ..Settings::default()
            };
            assert!(s.validate().is_err());
            s.migrate_loaded();
            assert_eq!(s.voice, "pf_dora");
            assert!(s.validate().is_ok());
        }
        let mut s = Settings {
            voice: "other".into(),
            ..Settings::default()
        };
        s.migrate_loaded();
        assert_eq!(s.voice, "other");
    }
    #[test]
    fn rejects_unknown_voice_and_asr() {
        let s = Settings {
            voice: "x".into(),
            ..Settings::default()
        };
        assert_eq!(s.validate().unwrap_err().code, "INVALID_INPUT");
        let s = Settings {
            asr_model: "large".into(),
            ..Settings::default()
        };
        assert_eq!(s.validate().unwrap_err().code, "INVALID_INPUT");
        for m in ["turbo", "small"] {
            let s = Settings {
                asr_model: m.into(),
                ..Settings::default()
            };
            assert!(s.validate().is_ok());
        }
    }
}
