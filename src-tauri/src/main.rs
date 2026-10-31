#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use serde_json::{Value, json};
use std::sync::Arc;
use support_runtime::{Result, Runtime, config::Paths, runtime::EventSink};
use tauri::{Manager, State, ipc::Channel};

#[tauri::command]
async fn app_command(
    command: String,
    args: Value,
    runtime: State<'_, Arc<Runtime>>,
) -> Result<Value> {
    support_runtime::commands::dispatch(runtime.inner().clone(), &command, args, None).await
}
#[tauri::command]
async fn app_stream(
    command: String,
    args: Value,
    on_event: Channel<Value>,
    runtime: State<'_, Arc<Runtime>>,
) -> Result<Value> {
    let sink: EventSink = Arc::new(move |event: support_runtime::runtime::AppEvent| {
        if let Ok(value) = serde_json::to_value(event) {
            let _ = on_event.send(value);
        }
    });
    support_runtime::commands::dispatch(runtime.inner().clone(), &command, args, Some(sink)).await
}
#[tauri::command]
fn session_create(
    title: String,
    runtime: State<'_, Arc<Runtime>>,
) -> Result<support_core::memory::Session> {
    runtime.session_create(&title)
}
#[tauri::command]
fn session_list(runtime: State<'_, Arc<Runtime>>) -> Result<Vec<support_core::memory::Session>> {
    runtime.session_list(None)
}
#[tauri::command]
fn session_messages(
    session_id: i64,
    runtime: State<'_, Arc<Runtime>>,
) -> Result<Vec<support_core::memory::Message>> {
    runtime.session_messages(session_id, None)
}
#[tauri::command]
fn session_delete(session_id: i64, runtime: State<'_, Arc<Runtime>>) -> Result<bool> {
    runtime.session_delete(session_id)
}
#[tauri::command]
fn memory_search(
    query: String,
    session_id: Option<i64>,
    limit: Option<u32>,
    runtime: State<'_, Arc<Runtime>>,
) -> Result<Vec<support_core::memory::Message>> {
    let _ = limit;
    runtime.memory_search(&query, session_id)
}
#[tauri::command]
fn model_validate(capabilities: support_core::models::Capabilities) -> Value {
    json!(capabilities.incompatibilities())
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let resources = if cfg!(debug_assertions) {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
            } else {
                app.path().resource_dir()?
            };
            let runtime = Runtime::open(Paths::new(app.path().app_data_dir()?, resources))?;
            app.manage(runtime);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_command,
            app_stream,
            session_create,
            session_list,
            session_messages,
            session_delete,
            memory_search,
            model_validate
        ])
        .build(tauri::generate_context!())
        .expect("Falha ao iniciar Aura")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                app.state::<Arc<Runtime>>().cancel();
            }
        });
}
