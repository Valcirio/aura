//! Dispatcher compartilhado pelo IPC Tauri e pelo harness de integração, sem acesso a shell/SQL arbitrários.
use crate::{
    AppError, Result, Runtime,
    config::Settings,
    google::database,
    runtime::{AppEvent, EventSink, NoticeKind},
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
struct IntegrationArg {
    #[serde(alias = "service_id")]
    integration: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetEnabled {
    #[serde(alias = "service_id")]
    integration: String,
    enabled: bool,
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
struct KnowledgeSearch {
    query: String,
    #[serde(default)]
    integration: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Notice {
    kind: NoticeKind,
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
        "provider_check" => {
            let _: Empty = decode(args)?;
            encode(runtime.provider_check().await?)
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
        "integration_list" | "service_list" => {
            let _: Empty = decode(args)?;
            let settings = runtime.settings()?;
            let list = runtime
                .integrations
                .list()
                .iter()
                .map(|i| {
                    let mut value = serde_json::to_value(i.manifest())?;
                    value["connected"] = json!(i.connected(&runtime.db)?);
                    value["enabled"] =
                        json!(!settings.integrations_disabled.contains(&i.manifest().id));
                    value["connectable"] =
                        json!(i.manifest().id == support_core::calendar::GOOGLE_CALENDAR);
                    value["tools"] = json!(
                        i.tools()
                            .iter()
                            .map(|t| json!({"name":t.name,"label":t.label,"effect":t.effect}))
                            .collect::<Vec<_>>()
                    );
                    Ok(value)
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Value::Array(list))
        }
        "integration_status" | "service_status" => {
            let _: Empty = decode(args)?;
            let google = runtime.google.status()?;
            Ok(
                json!({"google_calendar":google,"documents":database(&runtime.db)?.document_count("google_calendar")?}),
            )
        }
        "integration_connect" | "service_connect" => {
            let v: IntegrationArg = decode(args)?;
            runtime.integrations.get(&v.integration)?;
            if v.integration != support_core::calendar::GOOGLE_CALENDAR {
                return Err(AppError::new(
                    "SERVICE_NOT_CONNECTABLE",
                    "Integração sem conexão externa",
                ));
            }
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
        "integration_disconnect" | "service_disconnect" => {
            let v: IntegrationArg = decode(args)?;
            runtime.integrations.get(&v.integration)?;
            if v.integration != support_core::calendar::GOOGLE_CALENDAR {
                return Err(AppError::new(
                    "SERVICE_NOT_CONNECTABLE",
                    "Integração sem conexão externa",
                ));
            }
            runtime.cancel();
            let _guard = runtime.service_serial.lock().await;
            Ok(json!({"disconnected":true,"revoked":runtime.google.disconnect().await?}))
        }
        "integration_set_enabled" => {
            let v: SetEnabled = decode(args)?;
            runtime.integrations.get(&v.integration)?;
            let mut settings = runtime.settings()?;
            settings
                .integrations_disabled
                .retain(|id| id != &v.integration);
            if !v.enabled {
                settings.integrations_disabled.push(v.integration.clone());
            }
            let disabled = settings.integrations_disabled.clone();
            runtime.save_settings(settings, None, None, None).await?;
            Ok(
                json!({"integration":v.integration,"enabled":v.enabled,"integrations_disabled":disabled}),
            )
        }
        "knowledge_refresh" => {
            let v: IntegrationArg = decode(args)?;
            encode(
                crate::knowledge::refresh(
                    &runtime.http,
                    &runtime.db,
                    &runtime.integrations,
                    &v.integration,
                )
                .await?,
            )
        }
        "knowledge_search" => {
            let v: KnowledgeSearch = decode(args)?;
            if v.query.len() > 512 {
                return Err(AppError::invalid("Consulta muito longa"));
            }
            encode(database(&runtime.db)?.knowledge_search(v.integration.as_deref(), &v.query)?)
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
        "speech_prepare" => {
            let _: Empty = decode(args)?;
            runtime.speech_prepare().await
        }
        "speech_status" => {
            let _: Empty = decode(args)?;
            runtime.speech_status()
        }
        "alert_notice" => {
            let v: Notice = decode(args)?;
            runtime.alert_notice(v.kind).await?;
            Ok(json!({"played":true}))
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
    #[tokio::test]
    async fn provider_check_command_takes_no_arguments_and_reports_failures() {
        let directory = tempfile::tempdir().unwrap();
        let paths = crate::config::Paths::new(
            directory.path().into(),
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        );
        let runtime = Runtime::open(paths).unwrap();
        assert!(
            dispatch(
                runtime.clone(),
                "provider_check",
                json!({"extra":true}),
                None
            )
            .await
            .is_err()
        );
        let mut settings = Settings::default();
        settings.provider.kind = crate::config::ProviderKind::CompatibleApi;
        settings.provider.base_url = "http://127.0.0.1:9/v1".into();
        runtime
            .save_settings(settings, None, None, None)
            .await
            .unwrap();
        // Closed local port: a prompt, actionable failure, never a panic or hang.
        let error = dispatch(runtime, "provider_check", json!({}), None)
            .await
            .unwrap_err();
        assert!(!error.message.is_empty());
    }
    #[tokio::test]
    async fn speech_commands_take_no_arguments_and_return_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let fake = Arc::new(crate::runtime::speech_fixture::FakeSpeech::default());
        let runtime = crate::runtime::speech_fixture::runtime_with(directory.path(), &fake);
        for command in ["speech_status", "speech_prepare"] {
            assert!(
                dispatch(runtime.clone(), command, json!({"extra":true}), None)
                    .await
                    .is_err()
            );
        }
        let idle = dispatch(runtime.clone(), "speech_status", json!({}), None)
            .await
            .unwrap();
        assert_eq!(idle["phase"], "idle");
        assert!(fake.installed().is_empty());
        let started = dispatch(runtime.clone(), "speech_prepare", json!({}), None)
            .await
            .unwrap();
        assert_eq!(started["active"], true);
        for _ in 0..400 {
            let status = dispatch(runtime.clone(), "speech_status", json!({}), None)
                .await
                .unwrap();
            if status["ready"] == true {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("speech never became ready");
    }
    #[tokio::test]
    async fn alert_notice_accepts_only_a_known_kind() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let directory = tempfile::tempdir().unwrap();
        let fake = Arc::new(crate::runtime::speech_fixture::FakeSpeech::default());
        let mut runtime = crate::runtime::speech_fixture::runtime_with(directory.path(), &fake);
        let plays = Arc::new(AtomicUsize::new(0));
        let counter = plays.clone();
        Arc::get_mut(&mut runtime).unwrap().playback = Arc::new(
            crate::runtime::Playback::with_player(Arc::new(move |pcm, _, _, _| {
                assert_eq!(pcm.sample_rate, 24000);
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })),
        );
        for args in [
            json!({}),
            json!({"kind":"unknown"}),
            json!({"kind":"provider","text":"x"}),
            json!({"kind":"provider","path":"/etc/passwd"}),
            json!({"kind":"provider","pcm":[0.0]}),
            json!({"kind":"../provider"}),
            json!({"kind":null}),
            json!("provider"),
        ] {
            assert!(
                dispatch(runtime.clone(), "alert_notice", args, None)
                    .await
                    .is_err()
            );
        }
        assert_eq!(plays.load(Ordering::SeqCst), 0);
        for kind in ["provider", "models", "capture", "synthesis"] {
            let played = dispatch(runtime.clone(), "alert_notice", json!({"kind":kind}), None)
                .await
                .unwrap();
            assert_eq!(played["played"], true);
        }
        assert_eq!(plays.load(Ordering::SeqCst), 4);
    }
    #[tokio::test]
    async fn integration_list_reports_integrations_and_rejects_builtin_connect() {
        let directory = tempfile::tempdir().unwrap();
        let paths = crate::config::Paths::new(
            directory.path().into(),
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        );
        let runtime = Runtime::open(paths).unwrap();
        let list = dispatch(runtime.clone(), "integration_list", json!({}), None)
            .await
            .unwrap();
        let ids = list
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["memory", "knowledge", "web", "google_calendar"]);
        for entry in list.as_array().unwrap() {
            assert_eq!(
                entry["connectable"],
                entry["id"] == "google_calendar",
                "connectable only for google_calendar: {entry}"
            );
        }
        let google = list
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == "google_calendar")
            .unwrap();
        assert_eq!(google["connected"], false);
        let memory = list
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == "memory")
            .unwrap();
        let tools = memory["tools"].as_array().unwrap();
        assert!(!tools.is_empty());
        for tool in tools {
            assert!(!tool["label"].as_str().unwrap_or_default().is_empty());
            assert!(!tool["effect"].as_str().unwrap_or_default().is_empty());
        }
        let error = dispatch(
            runtime.clone(),
            "integration_connect",
            json!({"integration":"memory"}),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "SERVICE_NOT_CONNECTABLE");
        let error = dispatch(runtime, "calendar_prepare_event", json!({}), None)
            .await
            .unwrap_err();
        assert_eq!(error.code, "COMMAND_NOT_ALLOWED");
    }
    #[tokio::test]
    async fn integration_set_enabled_toggles_tools_and_reports_disabled_list() {
        let directory = tempfile::tempdir().unwrap();
        let paths = crate::config::Paths::new(
            directory.path().into(),
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        );
        let runtime = Runtime::open(paths).unwrap();
        let web_tool = dispatch(runtime.clone(), "integration_list", json!({}), None)
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == "web")
            .unwrap()["tools"][0]["name"]
            .as_str()
            .unwrap()
            .to_string();
        let disabled = dispatch(
            runtime.clone(),
            "integration_set_enabled",
            json!({"integration":"web","enabled":false}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(disabled["integrations_disabled"], json!(["web"]));
        assert!(
            !runtime
                .tools()
                .unwrap()
                .iter()
                .any(|v| v["function"]["name"] == web_tool.as_str())
        );
        let list = dispatch(runtime.clone(), "integration_list", json!({}), None)
            .await
            .unwrap();
        assert_eq!(
            list.as_array()
                .unwrap()
                .iter()
                .find(|v| v["id"] == "web")
                .unwrap()["enabled"],
            false
        );
        let enabled = dispatch(
            runtime.clone(),
            "integration_set_enabled",
            json!({"integration":"web","enabled":true}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(enabled["integrations_disabled"], json!([]));
        assert!(
            runtime
                .tools()
                .unwrap()
                .iter()
                .any(|v| v["function"]["name"] == web_tool.as_str())
        );
        assert!(
            dispatch(
                runtime,
                "integration_set_enabled",
                json!({"integration":"nope","enabled":false}),
                None
            )
            .await
            .is_err()
        );
    }
    #[tokio::test]
    async fn legacy_service_commands_keep_working() {
        let directory = tempfile::tempdir().unwrap();
        let paths = crate::config::Paths::new(
            directory.path().into(),
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        );
        let runtime = Runtime::open(paths).unwrap();
        let list = dispatch(runtime.clone(), "service_list", json!({}), None)
            .await
            .unwrap();
        assert_eq!(list.as_array().unwrap().len(), 4);
        let error = dispatch(
            runtime,
            "service_connect",
            json!({"service_id":"memory"}),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "SERVICE_NOT_CONNECTABLE");
    }
    #[tokio::test]
    async fn integration_status_matches_service_status_shape() {
        let directory = tempfile::tempdir().unwrap();
        let paths = crate::config::Paths::new(
            directory.path().into(),
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        );
        let runtime = Runtime::open(paths).unwrap();
        let status = dispatch(runtime.clone(), "integration_status", json!({}), None)
            .await
            .unwrap();
        assert!(status.get("google_calendar").is_some());
        assert!(status.get("documents").is_some());
        let legacy = dispatch(runtime, "service_status", json!({}), None)
            .await
            .unwrap();
        assert_eq!(status, legacy);
    }
}
