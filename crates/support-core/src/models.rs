use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    pub chat: bool,
    pub streaming: bool,
    pub tools: bool,
    pub portuguese: bool,
    pub context_tokens: u32,
    pub verified: bool,
}

impl Capabilities {
    /// O catálogo do provedor não substitui um probe real de tool calling.
    pub fn incompatibilities(&self) -> Vec<&'static str> {
        let mut reasons = Vec::new();
        if !self.chat {
            reasons.push("Modelo sem suporte a conversas");
        }
        if !self.streaming {
            reasons.push("Streaming indisponível");
        }
        if !self.tools {
            reasons.push("Chamadas de ferramentas indisponíveis");
        }
        if !self.portuguese {
            reasons.push("Português não validado");
        }
        if self.context_tokens < 8192 {
            reasons.push("Contexto mínimo do produto: 8192 tokens");
        }
        if !self.verified {
            reasons.push("Teste de compatibilidade pendente");
        }
        reasons
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn textual_model_can_support_audio_pipeline() {
        let c = Capabilities {
            chat: true,
            streaming: true,
            tools: true,
            portuguese: true,
            context_tokens: 8192,
            verified: true,
        };
        assert!(c.incompatibilities().is_empty());
    }
    #[test]
    fn advertised_tools_are_insufficient_without_probe() {
        let c = Capabilities {
            chat: true,
            streaming: true,
            tools: true,
            portuguese: true,
            context_tokens: 32768,
            verified: false,
        };
        assert_eq!(
            c.incompatibilities(),
            vec!["Teste de compatibilidade pendente"]
        );
    }
}
