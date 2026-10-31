//! Harness NDJSON: cada linha contém {id,command,args}; eventos e respostas saem em JSON.
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::sync::Arc;
#[tokio::main]
async fn main() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let data = std::env::var_os("AURA_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join(".local-data"));
    let runtime =
        match support_runtime::Runtime::open(support_runtime::config::Paths::new(data, root)) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        };
    let sink: support_runtime::runtime::EventSink = Arc::new(|event| {
        println!("{}", json!({"event":event}));
        let _ = std::io::stdout().flush();
    });
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else {
            break;
        };
        if line.len() > 100_000 {
            println!(
                "{}",
                json!({"error":{"code":"INVALID_INPUT","message":"Requisição muito longa"}})
            );
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                println!(
                    "{}",
                    json!({"error":{"code":"INVALID_INPUT","message":"JSON inválido"}})
                );
                continue;
            }
        };
        let response = support_runtime::commands::dispatch(
            runtime.clone(),
            request["command"].as_str().unwrap_or(""),
            request.get("args").cloned().unwrap_or_else(|| json!({})),
            Some(sink.clone()),
        )
        .await;
        match response {
            Ok(value) => println!("{}", json!({"id":request["id"],"result":value})),
            Err(error) => println!("{}", json!({"id":request["id"],"error":error})),
        };
        let _ = std::io::stdout().flush();
    }
    runtime.cancel();
}
