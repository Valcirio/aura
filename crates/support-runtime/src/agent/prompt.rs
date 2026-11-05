//! Prompt de sistema do agente, montado a partir das integrações disponíveis.
use crate::{
    config::Settings,
    integrations::{Integration, IntegrationManifest},
};
use std::sync::Arc;

pub(crate) fn system_prompt(
    settings: &Settings,
    now: &str,
    active: &[Arc<dyn Integration>],
    inactive: &[IntegrationManifest],
) -> String {
    let names = active
        .iter()
        .map(|i| i.manifest().name)
        .collect::<Vec<_>>()
        .join(", ");
    let policy = active
        .iter()
        .filter(|i| !i.manifest().builtin)
        .filter_map(|i| i.policy(settings))
        .collect::<Vec<_>>()
        .join(" ");
    let mut prompt = String::from(
        "Você é Aura, uma assistente pessoal generalista em português brasileiro. Resolva tarefas variadas combinando as ferramentas das integrações disponíveis no mesmo turno.",
    );
    prompt.push_str(" Use frases curtas e naturais, sem Markdown, sem código e sem URLs faladas; as fontes aparecem separadamente na interface.");
    prompt.push_str(" Use memory_search para conversas anteriores, knowledge_search para a documentação das integrações e web_search para informações públicas complementares. Nunca envie dados pessoais ou privados para a busca web. Cite as fontes.");
    prompt.push_str(" Nunca afirme que uma ação aconteceu sem um resultado confirmado.");
    prompt.push_str(" Ferramentas que exigem aprovação apenas PROPÕEM a ação: o aplicativo pede a confirmação do usuário. Proponha uma ação por turno.");
    prompt.push_str(" Pergunte quando faltar alguma informação necessária.");
    prompt.push_str(" Se uma integração necessária estiver desconectada ou desativada, peça para habilitá-la ou conectá-la em Integrações.");
    prompt.push_str(&format!(" Agora: {now} ({}).", settings.time_zone));
    prompt.push_str(&format!(" Integrações disponíveis: {names}."));
    if !policy.is_empty() {
        prompt.push(' ');
        prompt.push_str(&policy);
    }
    if !inactive.is_empty() {
        let names = inactive
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        prompt.push_str(&format!(
            " Integrações desativadas ou desconectadas: {names}."
        ));
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AppError, Result,
        config::Paths,
        integrations::{IntegrationManifest, ToolContext},
        runtime::Runtime,
    };
    use serde_json::{Value, json};
    use support_core::tools::{Effect, ToolSpec};

    struct Stub;

    #[async_trait::async_trait]
    impl Integration for Stub {
        fn manifest(&self) -> IntegrationManifest {
            IntegrationManifest {
                id: "stub".into(),
                name: "Stub".into(),
                description: "Integração de teste".into(),
                documentation: vec![],
                builtin: false,
            }
        }
        fn tools(&self) -> Vec<ToolSpec> {
            vec![ToolSpec::new(
                "stub",
                "stub_read",
                "Consultando stub",
                "Leitura de teste",
                Effect::Read,
                json!({"type":"object","properties":{},"additionalProperties":false}),
            )]
        }
        fn policy(&self, _settings: &Settings) -> Option<String> {
            Some("POLICY-X".into())
        }
        async fn execute(
            &self,
            _name: &str,
            _args: Value,
            _ctx: &ToolContext<'_>,
        ) -> Result<Value> {
            Err(AppError::invalid("Ferramenta incorreta"))
        }
    }

    fn settings() -> Settings {
        Settings::default()
    }

    #[test]
    fn active_non_builtin_policy_is_included() {
        let active: Vec<Arc<dyn Integration>> = vec![Arc::new(Stub)];
        let prompt = system_prompt(&settings(), "agora", &active, &[]);
        assert!(prompt.contains("POLICY-X"));
        assert!(prompt.contains("Stub"));
        assert!(!prompt.contains("Integrações desativadas ou desconectadas"));
    }

    #[test]
    fn inactive_non_builtin_is_listed_without_policy() {
        let stub = Stub;
        let manifest = stub.manifest();
        let active: Vec<Arc<dyn Integration>> = Vec::new();
        let prompt = system_prompt(&settings(), "agora", &active, &[manifest]);
        assert!(!prompt.contains("POLICY-X"));
        let line = prompt
            .split("Integrações desativadas ou desconectadas: ")
            .nth(1)
            .unwrap();
        assert!(line.contains(&stub.manifest().name));
    }

    #[test]
    fn disconnected_calendar_is_listed_as_inactive() {
        let directory = tempfile::tempdir().unwrap();
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let runtime = Runtime::open(Paths::new(directory.path().into(), root)).unwrap();
        let settings = settings();
        let inactive = runtime
            .integrations
            .inactive(&runtime.db, &settings)
            .unwrap();
        assert!(inactive.iter().any(|m| m.name == "Google Calendar"));
        let active = runtime.integrations.active(&runtime.db, &settings).unwrap();
        let prompt = system_prompt(&settings, "agora", &active, &inactive);
        assert!(prompt.contains("Google Calendar"));
        assert!(!prompt.contains("Agenda padrão"));
    }

    #[test]
    fn prompt_has_time_and_no_legacy_phrase() {
        let prompt = system_prompt(&settings(), "agora", &[], &[]);
        assert!(!prompt.contains("Serviço ativo"));
        assert!(prompt.contains("Agora: "));
    }
}
