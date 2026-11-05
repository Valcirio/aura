//! Contrato genérico de integrações: cada integração habilitada e conectada expõe
//! suas ferramentas simultaneamente, sem alterar captura, modelos ou memória.
mod builtin;
mod calendar;
pub use builtin::{KnowledgeIntegration, MemoryIntegration, WebIntegration};
pub use calendar::CalendarIntegration;

use crate::{AppError, Result, config::Settings, google::Database, secrets::SecretStore};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;
use support_core::{store::Operation, tools::ToolSpec};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize)]
pub struct IntegrationManifest {
    pub id: String,
    pub name: String,
    pub description: String,
    pub documentation: Vec<String>,
    pub builtin: bool,
}

pub struct ToolContext<'a> {
    pub db: &'a Database,
    pub http: &'a reqwest::Client,
    pub vault: &'a SecretStore,
    pub settings: &'a Settings,
    pub session_id: i64,
    pub cancel: &'a CancellationToken,
}

#[derive(Debug)]
pub struct ToolOutcome {
    pub value: Value,
    pub action: Option<Operation>,
}

#[async_trait]
pub trait Integration: Send + Sync {
    fn manifest(&self) -> IntegrationManifest;
    fn tools(&self) -> Vec<ToolSpec>;
    fn allows_document(&self, _url: &str) -> bool {
        false
    }
    fn policy(&self, _settings: &Settings) -> Option<String> {
        None
    }
    fn state(&self, _tool: &str) -> &'static str {
        "consulting"
    }
    fn connected(&self, _db: &Database) -> Result<bool> {
        Ok(true)
    }
    async fn execute(&self, name: &str, args: Value, ctx: &ToolContext<'_>) -> Result<Value>;
    async fn prepare(
        &self,
        _name: &str,
        _args: Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<Operation> {
        Err(not_allowed())
    }
    async fn commit(&self, _op: &Operation, _ctx: &ToolContext<'_>) -> Result<Operation> {
        Err(not_allowed())
    }
    async fn reconcile(&self, _op: &Operation, _ctx: &ToolContext<'_>) -> Result<Operation> {
        Err(not_allowed())
    }
    async fn retry(&self, _op: &Operation, _ctx: &ToolContext<'_>) -> Result<Operation> {
        Err(not_allowed())
    }
    fn describe_result(&self, op: &Operation) -> String {
        default_result(op)
    }
}

fn not_allowed() -> AppError {
    AppError::new("TOOL_NOT_ALLOWED", "Ferramenta não permitida")
}

pub(super) fn default_result(op: &Operation) -> String {
    if op.status == "completed" {
        format!("Ação concluída: {}.", op.summary)
    } else {
        "Não consegui confirmar o resultado da ação. Use Verificar estado antes de tentar novamente."
            .into()
    }
}

#[derive(Default)]
pub struct IntegrationRegistry {
    items: Vec<Arc<dyn Integration>>,
}

impl IntegrationRegistry {
    pub fn register(&mut self, integration: Arc<dyn Integration>) -> Result<()> {
        let manifest = integration.manifest();
        let duplicate = manifest.id.is_empty()
            || self.items.iter().any(|i| i.manifest().id == manifest.id)
            || self.items.iter().flat_map(|i| i.tools()).any(|existing| {
                integration
                    .tools()
                    .iter()
                    .any(|tool| tool.name == existing.name)
            });
        let mut names = std::collections::HashSet::new();
        let repeated = integration
            .tools()
            .iter()
            .any(|tool| !names.insert(tool.name.clone()));
        if duplicate || repeated {
            return Err(AppError::invalid(
                "Integração já registrada, identificador inválido ou ferramenta duplicada",
            ));
        }
        self.items.push(integration);
        Ok(())
    }
    pub fn get(&self, id: &str) -> Result<Arc<dyn Integration>> {
        self.items
            .iter()
            .find(|i| i.manifest().id == id)
            .cloned()
            .ok_or_else(|| AppError::new("SERVICE_NOT_FOUND", "Integração não encontrada"))
    }
    pub fn list(&self) -> Vec<Arc<dyn Integration>> {
        self.items.clone()
    }
    pub fn active(&self, db: &Database, settings: &Settings) -> Result<Vec<Arc<dyn Integration>>> {
        let mut active = Vec::new();
        for integration in &self.items {
            if settings
                .integrations_disabled
                .contains(&integration.manifest().id)
            {
                continue;
            }
            if integration.connected(db)? {
                active.push(integration.clone());
            }
        }
        Ok(active)
    }
    pub fn inactive(&self, db: &Database, settings: &Settings) -> Result<Vec<IntegrationManifest>> {
        let mut inactive = Vec::new();
        for integration in &self.items {
            let manifest = integration.manifest();
            if manifest.builtin {
                continue;
            }
            if settings.integrations_disabled.contains(&manifest.id)
                || !integration.connected(db)?
            {
                inactive.push(manifest);
            }
        }
        Ok(inactive)
    }
    pub fn available(&self, db: &Database, settings: &Settings) -> Result<Vec<ToolSpec>> {
        Ok(self
            .active(db, settings)?
            .iter()
            .flat_map(|i| i.tools())
            .collect())
    }
    pub fn spec(
        &self,
        name: &str,
        db: &Database,
        settings: &Settings,
    ) -> Result<(Arc<dyn Integration>, ToolSpec)> {
        for integration in self.active(db, settings)? {
            if let Some(spec) = integration.tools().into_iter().find(|t| t.name == name) {
                return Ok((integration, spec));
            }
        }
        Err(not_allowed())
    }
    pub async fn dispatch(
        &self,
        name: &str,
        args: Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolOutcome> {
        if ctx.cancel.is_cancelled() {
            return Err(AppError::cancelled());
        }
        let (integration, spec) = self.spec(name, ctx.db, ctx.settings)?;
        spec.validate(&args)?;
        if spec.effect.requires_approval() {
            let op = integration.prepare(name, args, ctx).await?;
            return Ok(ToolOutcome {
                value: json!({"approval_required":true,"executed":false,"action":op}),
                action: Some(op),
            });
        }
        let value = integration.execute(name, args, ctx).await?;
        if ctx.cancel.is_cancelled() {
            return Err(AppError::cancelled());
        }
        Ok(ToolOutcome {
            value,
            action: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::google::Database;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use support_core::{
        memory::Memory,
        store::Operation,
        tools::{ActionField, Effect, ToolSpec},
    };

    struct Fake {
        id: &'static str,
        tool: &'static str,
        effect: Effect,
        connected: bool,
        builtin: bool,
        executed: AtomicUsize,
        prepared: AtomicUsize,
    }
    impl Fake {
        fn new(id: &'static str, tool: &'static str, effect: Effect) -> Self {
            Self {
                id,
                tool,
                effect,
                connected: true,
                builtin: true,
                executed: AtomicUsize::new(0),
                prepared: AtomicUsize::new(0),
            }
        }
    }
    #[async_trait]
    impl Integration for Fake {
        fn manifest(&self) -> IntegrationManifest {
            IntegrationManifest {
                id: self.id.into(),
                name: self.id.into(),
                description: "fake".into(),
                documentation: vec![],
                builtin: self.builtin,
            }
        }
        fn tools(&self) -> Vec<ToolSpec> {
            vec![ToolSpec::new(
                self.id,
                self.tool,
                "Teste",
                "Teste",
                self.effect,
                json!({"type":"object","properties":{"x":{"type":"string"}},"required":["x"],"additionalProperties":false}),
            )]
        }
        fn connected(&self, _db: &Database) -> Result<bool> {
            Ok(self.connected)
        }
        async fn execute(
            &self,
            _name: &str,
            _args: Value,
            _ctx: &ToolContext<'_>,
        ) -> Result<Value> {
            self.executed.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"ok":true}))
        }
        async fn prepare(
            &self,
            name: &str,
            _args: Value,
            _ctx: &ToolContext<'_>,
        ) -> Result<Operation> {
            assert_eq!(name, self.tool);
            self.prepared.fetch_add(1, Ordering::SeqCst);
            Ok(Operation {
                id: "op".into(),
                session_id: Some(1),
                integration: self.id.into(),
                account_id: "acc".into(),
                tool: self.tool.into(),
                effect: self.effect,
                summary: "Teste".into(),
                fields: vec![ActionField::new("Título", "Teste")],
                payload: json!({}),
                status: "awaiting_confirmation".into(),
                expires_at: 1,
                result: None,
            })
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        db: Database,
        http: reqwest::Client,
        vault: SecretStore,
        settings: Settings,
        cancel: CancellationToken,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let db = Arc::new(Mutex::new(Memory::open(dir.path().join("m.db")).unwrap()));
            Self {
                _dir: dir,
                db,
                http: crate::http::client().unwrap(),
                vault: SecretStore::default(),
                settings: Settings::default(),
                cancel: CancellationToken::new(),
            }
        }
        fn context(&self) -> ToolContext<'_> {
            ToolContext {
                db: &self.db,
                http: &self.http,
                vault: &self.vault,
                settings: &self.settings,
                session_id: 1,
                cancel: &self.cancel,
            }
        }
    }

    #[test]
    fn rejects_invalid_and_duplicate_registrations() {
        let mut registry = IntegrationRegistry::default();
        assert!(
            registry
                .register(Arc::new(Fake::new("", "a", Effect::Read)))
                .is_err()
        );
        registry
            .register(Arc::new(Fake::new("one", "a", Effect::Read)))
            .unwrap();
        assert!(
            registry
                .register(Arc::new(Fake::new("one", "b", Effect::Read)))
                .is_err()
        );
        assert!(
            registry
                .register(Arc::new(Fake::new("two", "a", Effect::Read)))
                .is_err()
        );
    }

    #[tokio::test]
    async fn unknown_tool_and_invalid_args_are_rejected() {
        let fixture = Fixture::new();
        let mut registry = IntegrationRegistry::default();
        let fake = Arc::new(Fake::new("fake", "fake_read", Effect::Read));
        registry.register(fake.clone()).unwrap();
        let ctx = fixture.context();
        let unknown = registry
            .dispatch("missing", json!({"x":"1"}), &ctx)
            .await
            .unwrap_err();
        assert_eq!(unknown.code, "TOOL_NOT_ALLOWED");
        let invalid = registry
            .dispatch("fake_read", json!({}), &ctx)
            .await
            .unwrap_err();
        assert_eq!(invalid.code, "TOOL_ARGS_INVALID");
        assert_eq!(fake.executed.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn approval_tool_runs_prepare_only() {
        let fixture = Fixture::new();
        let mut registry = IntegrationRegistry::default();
        let fake = Arc::new(Fake::new("fake", "fake_write", Effect::ExternalWrite));
        registry.register(fake.clone()).unwrap();
        let ctx = fixture.context();
        let outcome = registry
            .dispatch("fake_write", json!({"x":"1"}), &ctx)
            .await
            .unwrap();
        assert_eq!(outcome.value["approval_required"], true);
        assert_eq!(outcome.value["executed"], false);
        assert!(outcome.action.is_some());
        assert_eq!(fake.prepared.load(Ordering::SeqCst), 1);
        assert_eq!(fake.executed.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn disabled_and_disconnected_integrations_are_absent() {
        let fixture = Fixture::new();
        let mut registry = IntegrationRegistry::default();
        let fake = Arc::new(Fake::new("fake", "fake_read", Effect::Read));
        registry.register(fake.clone()).unwrap();
        let mut settings = Settings::default();
        settings.integrations_disabled = vec!["fake".into()];
        assert!(
            registry
                .available(&fixture.db, &settings)
                .unwrap()
                .is_empty()
        );
        let ctx = ToolContext {
            db: &fixture.db,
            http: &fixture.http,
            vault: &fixture.vault,
            settings: &settings,
            session_id: 1,
            cancel: &fixture.cancel,
        };
        assert_eq!(
            registry
                .dispatch("fake_read", json!({"x":"1"}), &ctx)
                .await
                .unwrap_err()
                .code,
            "TOOL_NOT_ALLOWED"
        );
        let disconnected = Fake {
            connected: false,
            ..Fake::new("fake2", "fake2_read", Effect::Read)
        };
        let mut registry = IntegrationRegistry::default();
        registry.register(Arc::new(disconnected)).unwrap();
        assert!(
            registry
                .available(&fixture.db, &fixture.settings)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn inactive_lists_disconnected_and_disabled_non_builtin_integrations() {
        let fixture = Fixture::new();
        let mut registry = IntegrationRegistry::default();
        registry
            .register(Arc::new(Fake::new("builtin", "builtin_read", Effect::Read)))
            .unwrap();
        registry
            .register(Arc::new(Fake {
                builtin: false,
                ..Fake::new("ativo", "ativo_read", Effect::Read)
            }))
            .unwrap();
        registry
            .register(Arc::new(Fake {
                builtin: false,
                connected: false,
                ..Fake::new("desconectada", "desconectada_read", Effect::Read)
            }))
            .unwrap();
        registry
            .register(Arc::new(Fake {
                builtin: false,
                ..Fake::new("desativada", "desativada_read", Effect::Read)
            }))
            .unwrap();
        let mut settings = Settings::default();
        settings.integrations_disabled = vec!["desativada".into()];
        let inactive = registry.inactive(&fixture.db, &settings).unwrap();
        let ids = inactive.iter().map(|m| m.id.as_str()).collect::<Vec<_>>();
        assert!(ids.contains(&"desconectada"));
        assert!(ids.contains(&"desativada"));
        assert!(!ids.contains(&"builtin"));
        assert!(!ids.contains(&"ativo"));
    }
}
