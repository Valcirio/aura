use super::{Integration, IntegrationManifest, ToolContext, default_result};
use crate::{
    AppError, Result,
    google::{self, CalendarApi, Database, GoogleClient, database},
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use support_core::{
    calendar::{CREATE_EVENT_TOOL, EventDraft, EventInput, GOOGLE_CALENDAR},
    store::Operation,
    tools::{Effect, ToolSpec},
};

const ID: &str = GOOGLE_CALENDAR;

pub struct CalendarIntegration {
    pub client: Arc<GoogleClient>,
}

#[async_trait]
impl Integration for CalendarIntegration {
    fn manifest(&self) -> IntegrationManifest {
        IntegrationManifest {
            id: ID.into(),
            name: "Google Calendar".into(),
            description:
                "Entenda o serviço, consulte sua agenda e crie eventos com confirmação.".into(),
            documentation: vec![
                "https://developers.google.com/workspace/calendar?hl=pt-br".into(),
                "https://developers.google.com/workspace/calendar/api/guides/overview?hl=pt-br"
                    .into(),
                "https://developers.google.com/workspace/calendar/api/guides/create-events?hl=pt-br"
                    .into(),
                "https://developers.google.com/workspace/calendar/api/v3/reference/events/list?hl=pt-br"
                    .into(),
                "https://developers.google.com/workspace/calendar/api/v3/reference/freebusy/query?hl=pt-br"
                    .into(),
            ],
            builtin: false,
        }
    }
    fn allows_document(&self, url: &str) -> bool {
        url == "https://developers.google.com/workspace/calendar"
            || url.starts_with("https://developers.google.com/workspace/calendar?")
            || url.starts_with("https://developers.google.com/workspace/calendar/")
    }
    fn tools(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec::new(
                ID,
                "calendar_list",
                "Consultando agendas",
                "Lista as agendas da conta conectada e permissões de escrita.",
                Effect::Read,
                serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            ToolSpec::new(
                ID,
                "calendar_events",
                "Consultando eventos",
                "Consulta eventos no intervalo RFC3339; expande recorrências. Use page_token quando houver próxima página.",
                Effect::Read,
                serde_json::json!({"type":"object","properties":{"calendar_id":{"type":"string"},"start":{"type":"string"},"end":{"type":"string"},"query":{"type":"string"},"page_token":{"type":"string"}},"required":["calendar_id","start","end"],"additionalProperties":false}),
            ),
            ToolSpec::new(
                ID,
                "calendar_free_busy",
                "Verificando disponibilidade",
                "Consulta disponibilidade real; não conclua que a agenda está livre se houver erro por calendário.",
                Effect::Read,
                serde_json::json!({"type":"object","properties":{"calendar_ids":{"type":"array","items":{"type":"string"},"maxItems":10},"start":{"type":"string"},"end":{"type":"string"}},"required":["calendar_ids","start","end"],"additionalProperties":false}),
            ),
            ToolSpec::new(
                ID,
                CREATE_EVENT_TOOL,
                "Preparando evento",
                "Propõe a criação de um evento, SEM criá-lo: o aplicativo pede a confirmação do usuário antes de criar. Exija título, agenda, início e fim/duração; esclareça ambiguidades. Datas de dia inteiro usam fim exclusivo.",
                Effect::ExternalWrite,
                serde_json::json!({"type":"object","properties":{"calendar_id":{"type":"string"},"summary":{"type":"string"},"start":{"type":"string"},"end":{"type":"string"},"time_zone":{"type":"string"},"all_day":{"type":"boolean"},"description":{"type":"string"}},"required":["calendar_id","summary","start","end","time_zone"],"additionalProperties":false}),
            ),
        ]
    }
    fn policy(&self, settings: &crate::config::Settings) -> Option<String> {
        Some(format!(
            "Agenda padrão: {}. calendar_create_event NÃO cria nada até o usuário confirmar no aplicativo: apresente título, agenda, data absoluta, início, fim, fuso e conflitos, e peça confirmação. NUNCA diga que criou evento sem resultado confirmado. O aplicativo não executa edição, exclusão, criação de recorrência, convites ou Meet no MVP; pode explicar essas funções usando documentação. Pergunte quando faltarem título, data, hora ou duração.",
            settings.calendar_id
        ))
    }
    fn connected(&self, db: &Database) -> Result<bool> {
        Ok(database(db)?.connection_info(GOOGLE_CALENDAR)?.is_some())
    }
    async fn execute(&self, name: &str, args: Value, ctx: &ToolContext<'_>) -> Result<Value> {
        if ctx.cancel.is_cancelled() {
            return Err(AppError::cancelled());
        }
        match name {
            "calendar_list" => {
                let _: Empty = serde_json::from_value(args)?;
                self.client.calendars().await
            }
            "calendar_events" => {
                let q: EventsQuery = serde_json::from_value(args)?;
                self.client
                    .events(
                        &q.calendar_id,
                        &q.start,
                        &q.end,
                        &q.query,
                        q.page_token.as_deref(),
                    )
                    .await
            }
            "calendar_free_busy" => {
                let q: BusyQuery = serde_json::from_value(args)?;
                self.client
                    .free_busy(&q.calendar_ids, &q.start, &q.end)
                    .await
            }
            _ => Err(AppError::new(
                "TOOL_NOT_ALLOWED",
                "Ferramenta não exposta por esta integração",
            )),
        }
    }
    async fn prepare(&self, name: &str, args: Value, ctx: &ToolContext<'_>) -> Result<Operation> {
        if name != CREATE_EVENT_TOOL {
            return Err(AppError::new(
                "TOOL_NOT_ALLOWED",
                "Ferramenta não permitida",
            ));
        }
        let event = EventDraft::validate(serde_json::from_value::<EventInput>(args)?)?;
        let account = database(ctx.db)?
            .connection_info(GOOGLE_CALENDAR)?
            .ok_or_else(|| AppError::new("AUTH_REQUIRED", "Conecte sua conta Google"))?
            .account_id;
        if database(ctx.db)?
            .pending_operation(ctx.session_id)?
            .is_some_and(|op| ["executing", "unknown"].contains(&op.status.as_str()))
        {
            return Err(AppError::new(
                "ACTION_UNRESOLVED",
                "Verifique a criação anterior antes de preparar outra",
            ));
        }
        google::ensure_writable(self.client.as_ref(), &event.calendar_id).await?;
        let busy = google::busy_for(self.client.as_ref(), &event).await?;
        if ctx.cancel.is_cancelled() {
            return Err(AppError::cancelled());
        }
        let op = database(ctx.db)?.prepare_calendar_event(
            ctx.session_id,
            &account,
            &event,
            crate::now(),
        )?;
        database(ctx.db)?.operation_context(&op.id, &busy)
    }
    async fn commit(&self, op: &Operation, ctx: &ToolContext<'_>) -> Result<Operation> {
        let account = database(ctx.db)?
            .connection_info(GOOGLE_CALENDAR)?
            .ok_or_else(|| {
                AppError::new("AUTH_REQUIRED", "Conecte a conta original para confirmar")
            })?
            .account_id;
        google::confirm_operation(
            ctx.db,
            self.client.as_ref(),
            &op.id,
            ctx.session_id,
            &account,
        )
        .await
    }
    async fn reconcile(&self, op: &Operation, ctx: &ToolContext<'_>) -> Result<Operation> {
        let account = database(ctx.db)?
            .connection_info(GOOGLE_CALENDAR)?
            .ok_or_else(|| {
                AppError::new("AUTH_REQUIRED", "Conecte a conta original para verificar")
            })?
            .account_id;
        google::reconcile_operation(ctx.db, self.client.as_ref(), &op.id, &account).await
    }
    async fn retry(&self, op: &Operation, ctx: &ToolContext<'_>) -> Result<Operation> {
        let account = database(ctx.db)?
            .connection_info(GOOGLE_CALENDAR)?
            .ok_or_else(|| AppError::new("AUTH_REQUIRED", "Conecte a conta original"))?
            .account_id;
        google::retry_operation(
            ctx.db,
            self.client.as_ref(),
            &op.id,
            ctx.session_id,
            &account,
        )
        .await
    }
    fn describe_result(&self, op: &Operation) -> String {
        if op.status == "completed" {
            if let Ok(draft) = op.payload_as::<EventDraft>() {
                return format!(
                    "Compromisso criado: {}, de {} a {}, no fuso {}.",
                    draft.summary, draft.start, draft.end, draft.time_zone
                );
            }
        }
        default_result(op)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventsQuery {
    calendar_id: String,
    start: String,
    end: String,
    #[serde(default)]
    query: String,
    #[serde(default)]
    page_token: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BusyQuery {
    calendar_ids: Vec<String>,
    start: String,
    end: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::SecretStore;
    use std::sync::{Arc, Mutex};
    use support_core::{memory::Memory, tools::Effect};

    fn integration() -> CalendarIntegration {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(Mutex::new(
            Memory::open(directory.path().join("m.db")).unwrap(),
        ));
        CalendarIntegration {
            client: Arc::new(GoogleClient::new(
                crate::http::client().unwrap(),
                Arc::new(SecretStore::default()),
                db,
            )),
        }
    }

    #[test]
    fn model_cannot_call_commit_or_delete() {
        let tools = integration().tools();
        assert!(
            !tools
                .iter()
                .any(|t| { ["action_confirm", "calendar_delete"].contains(&t.name.as_str()) })
        );
        assert_eq!(tools.len(), 4);
        assert!(!tools.iter().any(|t| t.name == "calendar_prepare_event"));
        for tool in &tools {
            if tool.name == CREATE_EVENT_TOOL {
                assert_eq!(tool.effect, Effect::ExternalWrite);
            } else {
                assert_eq!(tool.effect, Effect::Read);
            }
        }
    }

    #[test]
    fn documentation_urls_are_scoped_to_service() {
        let integration = integration();
        assert!(integration.allows_document(
            "https://developers.google.com/workspace/calendar/api/guides/overview?hl=pt-br"
        ));
        assert!(
            !integration
                .allows_document("https://developers.google.com.evil.test/workspace/calendar/")
        );
        assert!(
            !integration.allows_document("https://developers.google.com/workspace/calendar-other")
        );
    }
}
