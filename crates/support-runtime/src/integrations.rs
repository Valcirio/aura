//! Contrato de execução de serviços. Um novo produto registra metadados e este executor;
//! não precisa modificar captura, modelos, memória ou o coordenador de conversação.
use crate::{
    AppError, Result,
    google::{self, CalendarApi, Database, GoogleClient, database},
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use support_core::{
    calendar::{EventDraft, EventInput},
    store::Operation,
};
use tokio_util::sync::CancellationToken;

pub struct ServiceContext<'a> {
    pub db: &'a Database,
    pub session_id: i64,
    pub cancel: &'a CancellationToken,
}
pub struct ServiceResult {
    pub value: Value,
    pub action: Option<Operation>,
}
impl ServiceResult {
    pub fn data(value: Value) -> Self {
        Self {
            value,
            action: None,
        }
    }
}
#[async_trait]
pub trait ServiceExecutor: Send + Sync {
    async fn execute(
        &self,
        name: &str,
        args: Value,
        context: ServiceContext<'_>,
    ) -> Result<ServiceResult>;
}
pub struct CalendarExecutor {
    pub client: Arc<GoogleClient>,
}
#[async_trait]
impl ServiceExecutor for CalendarExecutor {
    async fn execute(
        &self,
        name: &str,
        args: Value,
        context: ServiceContext<'_>,
    ) -> Result<ServiceResult> {
        if context.cancel.is_cancelled() {
            return Err(AppError::cancelled());
        }
        let value = match name {
            "calendar_list" => {
                let _: Empty = serde_json::from_value(args)?;
                self.client.calendars().await?
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
                    .await?
            }
            "calendar_free_busy" => {
                let q: BusyQuery = serde_json::from_value(args)?;
                self.client
                    .free_busy(&q.calendar_ids, &q.start, &q.end)
                    .await?
            }
            "calendar_prepare_event" => {
                let event = EventDraft::validate(serde_json::from_value::<EventInput>(args)?)?;
                let account = database(context.db)?
                    .connection_info()?
                    .ok_or_else(|| AppError::new("AUTH_REQUIRED", "Conecte sua conta Google"))?
                    .account_id;
                if database(context.db)?
                    .pending_operation(context.session_id)?
                    .is_some_and(|op| ["executing", "unknown"].contains(&op.status.as_str()))
                {
                    return Err(AppError::new(
                        "ACTION_UNRESOLVED",
                        "Verifique a criação anterior antes de preparar outra",
                    ));
                }
                google::ensure_writable(self.client.as_ref(), &event.calendar_id).await?;
                let busy = google::busy_for(self.client.as_ref(), &event).await?;
                if context.cancel.is_cancelled() {
                    return Err(AppError::cancelled());
                }
                let op = database(context.db)?.prepare_operation(
                    context.session_id,
                    &account,
                    &event,
                    crate::now(),
                )?;
                let op = database(context.db)?.operation_context(&op.id, &busy)?;
                return Ok(ServiceResult {
                    value: json!({"created":false,"confirmation_required":true,"proposal":op}),
                    action: Some(op),
                });
            }
            _ => {
                return Err(AppError::new(
                    "TOOL_NOT_ALLOWED",
                    "Ferramenta não exposta por este serviço",
                ));
            }
        };
        Ok(ServiceResult::data(value))
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
