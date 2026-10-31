//! Dispatcher compartilhado pelo IPC Tauri e pelo harness de integração, sem acesso a shell/SQL arbitrários.
use crate::{
    AppError, Result, Runtime,
    config::Settings,
    google::database,
    runtime::{AppEvent, EventSink},
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::sync::Arc;

fn decode<T: DeserializeOwned>(value: Value) -> Result<T> {
    Ok(serde_json::from_value(value)?)
}
fn encode<T: serde::Serialize>(value: T) -> Result<Value> {
    Ok(serde_json::to_value(value)?)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Save {
    settings: Settings,
    #[serde(default)]
    provider_key: Option<String>,
    #[serde(default)]
    brave_key: Option<String>,
    #[serde(default)]
    google_secret: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewSession {
    title: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    #[serde(default)]
    before: Option<i64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Session {
    session_id: i64,
    #[serde(default)]
    after: Option<i64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Search {
    query: String,
    #[serde(default)]
    session_id: Option<i64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    session_id: i64,
    #[serde(default)]
    text: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Action {
    id: String,
    session_id: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenSource {
    url: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Service {
    service_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Events {
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
struct Busy {
    calendar_ids: Vec<String>,
    start: String,
    end: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepare {
    session_id: i64,
    event: support_core::calendar::EventInput,
}

pub async fn dispatch(
    runtime: Arc<Runtime>,
    command: &str,
    args: Value,
    sink: Option<EventSink>,
) -> Result<Value> {
    use crate::google::CalendarApi;
    match command {
        "settings_get" => {
            let _: Empty = decode(args)?;
            encode(runtime.settings()?)
        }
        "source_open" => {
            let v: OpenSource = decode(args)?;
            let url = url::Url::parse(&v.url).map_err(|_| AppError::invalid("Link inválido"))?;
            if !["http", "https"].contains(&url.scheme())
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(AppError::invalid(
                    "Somente links HTTP/HTTPS sem credenciais podem ser abertos",
                ));
            }
            tokio::task::spawn_blocking(move || webbrowser::open(url.as_str()))
                .await
                .map_err(|_| AppError::new("OPEN_SOURCE", "Não foi possível abrir a fonte"))?
                .map_err(|_| AppError::new("OPEN_SOURCE", "Não foi possível abrir o navegador"))?;
            Ok(json!({"opened":true}))
        }
        "settings_save" => {
            let v: Save = decode(args)?;
            runtime
                .save_settings(v.settings, v.provider_key, v.brave_key, v.google_secret)
                .await
        }
        "provider_models" => {
            let _: Empty = decode(args)?;
            encode(runtime.provider_models().await?)
        }
        "provider_probe" => {
            let _: Empty = decode(args)?;
            encode(runtime.provider_probe().await?)
        }
        "provider_status" => {
            let _: Empty = decode(args)?;
            runtime.probe_status()
        }
        "session_create" => {
            let v: NewSession = decode(args)?;
            encode(runtime.session_create(&v.title)?)
        }
        "session_list" => {
            let v: Page = decode(args)?;
            encode(runtime.session_list(v.before)?)
        }
        "session_messages" => {
            let v: Session = decode(args)?;
            encode(runtime.session_messages(v.session_id, v.after)?)
        }
        "session_delete" => {
            let v: Session = decode(args)?;
            encode(runtime.session_delete(v.session_id)?)
        }
        "memory_search" => {
            let v: Search = decode(args)?;
            encode(runtime.memory_search(&v.query, v.session_id)?)
        }
        "conversation_start" => {
            let v: Start = decode(args)?;
            let sink = sink.ok_or_else(|| AppError::invalid("Canal de eventos obrigatório"))?;
            encode(runtime.start(v.session_id, v.text, sink).await?)
        }
        "conversation_cancel" => {
            let _: Empty = decode(args)?;
            runtime.cancel();
            Ok(json!({"cancelled":true}))
        }
        "conversation_finish_input" => {
            let _: Empty = decode(args)?;
            runtime.finish_input();
            Ok(json!({"finished":true}))
        }
        "service_list" => {
            let _: Empty = decode(args)?;
            encode(runtime.registry.list())
        }
        "service_status" => {
            let _: Empty = decode(args)?;
            let google = runtime.google.status()?;
            Ok(
                json!({"google_calendar":google,"documents":database(&runtime.db)?.document_count("google_calendar")?}),
            )
        }
        "service_connect" => {
            let v: Service = decode(args)?;
            runtime.registry.get(&v.service_id)?;
            let settings = runtime.settings()?;
            let secret = runtime
                .vault
                .get("google_client_secret")
                .await?
                .unwrap_or_default();
            let ticket = runtime
                .google
                .begin(settings.google_client_id, secret)
                .await?;
            let url = ticket.authorization_url.clone();
            let opened = tokio::task::spawn_blocking(move || webbrowser::open(&url).is_ok())
                .await
                .unwrap_or(false);
            Ok(json!({"authorization_url":ticket.authorization_url,"browser_opened":opened}))
        }
        "service_disconnect" => {
            let v: Service = decode(args)?;
            runtime.registry.get(&v.service_id)?;
            runtime.cancel();
            let _guard = runtime.service_serial.lock().await;
            Ok(json!({"disconnected":true,"revoked":runtime.google.disconnect().await?}))
        }
        "knowledge_refresh" => {
            let v: Service = decode(args)?;
            encode(
                crate::knowledge::refresh(
                    &runtime.http,
                    &runtime.db,
                    &runtime.registry,
                    &v.service_id,
                )
                .await?,
            )
        }
        "knowledge_search" => {
            let v: Search = decode(args)?;
            if v.query.len() > 512 {
                return Err(AppError::invalid("Consulta muito longa"));
            }
            encode(database(&runtime.db)?.knowledge_search("google_calendar", &v.query)?)
        }
        "calendar_list" => {
            let _: Empty = decode(args)?;
            runtime.google.calendars().await
        }
        "calendar_events" => {
            let v: Events = decode(args)?;
            runtime
                .google
                .events(
                    &v.calendar_id,
                    &v.start,
                    &v.end,
                    &v.query,
                    v.page_token.as_deref(),
                )
                .await
        }
        "calendar_free_busy" => {
            let v: Busy = decode(args)?;
            runtime
                .google
                .free_busy(&v.calendar_ids, &v.start, &v.end)
                .await
        }
        "calendar_prepare_event" => {
            let v: Prepare = decode(args)?;
            let draft = support_core::calendar::EventDraft::validate(v.event)?;
            let account = database(&runtime.db)?
                .connection_info()?
                .ok_or_else(|| AppError::new("AUTH_REQUIRED", "Conecte sua conta Google"))?
                .account_id;
            crate::google::ensure_writable(runtime.google.as_ref(), &draft.calendar_id).await?;
            let busy = crate::google::busy_for(runtime.google.as_ref(), &draft).await?;
            let op = database(&runtime.db)?.prepare_operation(
                v.session_id,
                &account,
                &draft,
                crate::now(),
            )?;
            encode(database(&runtime.db)?.operation_context(&op.id, &busy)?)
        }
        "action_get" => {
            let v: Session = decode(args)?;
            let db = database(&runtime.db)?;
            db.expire_operations(crate::now())?;
            encode(db.latest_operation(v.session_id)?)
        }
        "action_confirm" => {
            let v: Action = decode(args)?;
            encode(runtime.confirm(&v.id, v.session_id).await?)
        }
        "action_retry" => {
            let v: Action = decode(args)?;
            encode(runtime.retry(&v.id, v.session_id).await?)
        }
        "action_cancel" => {
            let v: Action = decode(args)?;
            database(&runtime.db)?.cancel_operation(&v.id, v.session_id)?;
            encode(database(&runtime.db)?.operation(&v.id)?)
        }
        "action_reconcile" => {
            let v: Id = decode(args)?;
            encode(runtime.reconcile(&v.id).await?)
        }
        "audio_devices" => {
            let _: Empty = decode(args)?;
            tokio::task::spawn_blocking(|| encode(crate::audio::devices()?))
                .await
                .map_err(|_| AppError::new("AUDIO_ERROR", "Não foi possível listar dispositivos"))?
        }
        "model_status" | "voice_list" => {
            let _: Empty = decode(args)?;
            runtime.model_status().await
        }
        "voice_preview" => {
            let v: Id = decode(args)?;
            runtime.preview(v.id).await?;
            Ok(json!({"played":true}))
        }
        "model_install" => {
            let v: Id = decode(args)?;
            let progress = Arc::new(move |payload: Value| {
                if let Some(sink) = &sink {
                    sink(AppEvent {
                        session_id: 0,
                        turn_id: "download".into(),
                        generation: 0,
                        sequence: 0,
                        kind: "model.progress".into(),
                        payload,
                    });
                }
            });
            runtime.model_install(v.id, progress).await
        }
        _ => Err(AppError::new(
            "COMMAND_NOT_ALLOWED",
            "Comando não disponível",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn commands_reject_unknown_actions_and_preserve_persistent_history() {
        let directory = tempfile::tempdir().unwrap();
        let paths = crate::config::Paths::new(
            directory.path().into(),
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        );
        let runtime = Runtime::open(paths.clone()).unwrap();
        let session = dispatch(
            runtime.clone(),
            "session_create",
            json!({"title":"Suporte"}),
            None,
        )
        .await
        .unwrap();
        assert!(
            dispatch(runtime.clone(), "calendar_delete", json!({}), None)
                .await
                .is_err()
        );
        assert!(
            dispatch(
                runtime.clone(),
                "settings_get",
                json!({"secret":true}),
                None
            )
            .await
            .is_err()
        );
        database(&runtime.db)
            .unwrap()
            .append(session["id"].as_i64().unwrap(), "user", "Reunião anterior")
            .unwrap();
        drop(runtime);
        let reopened = Runtime::open(paths).unwrap();
        let messages = dispatch(reopened, "memory_search", json!({"query":"reuniao"}), None)
            .await
            .unwrap();
        assert_eq!(messages.as_array().unwrap().len(), 1);
    }
}
