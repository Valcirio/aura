use super::{Integration, IntegrationManifest, ToolContext, not_allowed};
use crate::{Result, google::database, knowledge, runtime::clip};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use support_core::tools::{Effect, ToolSpec};

fn manifest(id: &str, name: &str, description: &str) -> IntegrationManifest {
    IntegrationManifest {
        id: id.into(),
        name: name.into(),
        description: description.into(),
        documentation: vec![],
        builtin: true,
    }
}

pub struct MemoryIntegration;
#[async_trait]
impl Integration for MemoryIntegration {
    fn manifest(&self) -> IntegrationManifest {
        manifest(
            "memory",
            "Memória",
            "Busca em conversas anteriores e recupera mensagens de uma sessão.",
        )
    }
    fn tools(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec::new(
                "memory",
                "memory_search",
                "Buscando na memória",
                "Busca trechos de conversas anteriores. Para buscar apenas por data, use query vazia e since/until RFC3339; o limite superior é exclusivo.",
                Effect::Read,
                json!({"type":"object","properties":{"query":{"type":"string"},"since":{"type":"string"},"until":{"type":"string"}},"required":["query"],"additionalProperties":false}),
            ),
            ToolSpec::new(
                "memory",
                "conversation_get",
                "Abrindo conversa",
                "Abre mensagens de uma sessão recuperada, paginadas por ID.",
                Effect::Read,
                json!({"type":"object","properties":{"session_id":{"type":"integer"},"after":{"type":"integer"}},"required":["session_id"],"additionalProperties":false}),
            ),
        ]
    }
    fn state(&self, _tool: &str) -> &'static str {
        "memory"
    }
    async fn execute(&self, name: &str, args: Value, ctx: &ToolContext<'_>) -> Result<Value> {
        match name {
            "memory_search" => {
                let q: MemoryQuery = serde_json::from_value(args)?;
                if !q.query.trim().is_empty() || (q.since.is_none() && q.until.is_none()) {
                    validate_query(&q.query)?;
                }
                validate_date_filter(q.since.as_deref())?;
                validate_date_filter(q.until.as_deref())?;
                let messages = database(ctx.db)?.search_history(
                    &q.query,
                    None,
                    Some(ctx.session_id),
                    q.since.as_deref(),
                    q.until.as_deref(),
                )?;
                Ok(
                    json!({"sources":messages.into_iter().map(|m|json!({"source_id":format!("memory:{}:{}",m.session_id,m.id),"session_id":m.session_id,"message_id":m.id,"text":clip(&m.text,1000),"created_at":m.created_at,"kind":"memory"})).collect::<Vec<_>>()}),
                )
            }
            "conversation_get" => {
                let q: ConversationQuery = serde_json::from_value(args)?;
                Ok(serde_json::to_value(database(ctx.db)?.messages_page(
                    q.session_id,
                    q.after,
                    20,
                )?)?)
            }
            _ => Err(not_allowed()),
        }
    }
}

pub struct KnowledgeIntegration;
#[async_trait]
impl Integration for KnowledgeIntegration {
    fn manifest(&self) -> IntegrationManifest {
        manifest(
            "knowledge",
            "Documentação",
            "Documentação oficial indexada das integrações.",
        )
    }
    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec::new(
            "knowledge",
            "knowledge_search",
            "Consultando documentação",
            "Busca na documentação oficial indexada das integrações. Use `integration` para limitar a uma delas.",
            Effect::Read,
            json!({"type":"object","properties":{"query":{"type":"string"},"integration":{"type":"string"}},"required":["query"],"additionalProperties":false}),
        )]
    }
    fn state(&self, _tool: &str) -> &'static str {
        "knowledge"
    }
    async fn execute(&self, name: &str, args: Value, ctx: &ToolContext<'_>) -> Result<Value> {
        if name != "knowledge_search" {
            return Err(not_allowed());
        }
        let q: KnowledgeQuery = serde_json::from_value(args)?;
        validate_query(&q.query)?;
        let documents = database(ctx.db)?.knowledge_search(q.integration.as_deref(), &q.query)?;
        Ok(
            json!({"sources":documents.into_iter().map(|d|json!({"source_id":format!("doc:{}",d.id),"title":d.title,"url":d.url,"text":d.text,"updated_at":d.updated_at,"kind":"documentation"})).collect::<Vec<_>>()}),
        )
    }
}

pub struct WebIntegration;
#[async_trait]
impl Integration for WebIntegration {
    fn manifest(&self) -> IntegrationManifest {
        manifest(
            "web",
            "Pesquisa web",
            "Pesquisa pública complementar à documentação oficial.",
        )
    }
    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec::new(
            "web",
            "web_search",
            "Pesquisando na web",
            "Pesquisa pública complementar. Nunca inclua dados pessoais ou conteúdo privado da agenda.",
            Effect::Read,
            json!({"type":"object","properties":{"query":{"type":"string"},"freshness":{"type":"string","enum":["pd","pw","pm","py"]}},"required":["query"],"additionalProperties":false}),
        )]
    }
    fn state(&self, _tool: &str) -> &'static str {
        "searching"
    }
    async fn execute(&self, name: &str, args: Value, ctx: &ToolContext<'_>) -> Result<Value> {
        if name != "web_search" {
            return Err(not_allowed());
        }
        let query: knowledge::SearchQuery = serde_json::from_value(args)?;
        knowledge::web_search(ctx.http, ctx.vault.get("brave").await?, query).await
    }
}

fn validate_query(query: &str) -> Result<()> {
    if query.trim().is_empty() || query.len() > 512 {
        Err(crate::AppError::invalid(
            "Informe uma busca de até 512 bytes",
        ))
    } else {
        Ok(())
    }
}
fn validate_date_filter(value: Option<&str>) -> Result<()> {
    if let Some(value) = value {
        value
            .parse::<jiff::Timestamp>()
            .map_err(|_| crate::AppError::invalid("Filtro temporal inválido"))?;
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KnowledgeQuery {
    query: String,
    #[serde(default)]
    integration: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryQuery {
    query: String,
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationQuery {
    session_id: i64,
    #[serde(default)]
    after: Option<i64>,
}
