//! Loop do agente: alterna texto e chamadas de ferramenta, transmitindo cada
//! rodada para a interface e para a fila de fala do turno.
mod approvals;
mod prompt;

use crate::{
    AppError, Result,
    config::Settings,
    google::database,
    provider::{ChatResponse, Provider, ToolCall, assistant_tools},
    runtime::{Emitter, Runtime, Speaker, clip, send_sentence},
    speech::Sentences,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::sync::Arc;
use support_core::tools::{Effect, ToolSpec};

/// Rodadas com ferramentas; a seguinte é sempre a resposta final sem ferramentas.
const MAX_ROUNDS: usize = 8;
/// Chamadas de ferramenta somadas no turno inteiro.
const MAX_TOOL_CALLS: usize = 12;

pub(crate) async fn answer(
    runtime: &Arc<Runtime>,
    text: &str,
    emitter: &Emitter,
) -> Result<(String, String)> {
    database(&runtime.db)?.expire_operations(crate::now())?;
    let pending = database(&runtime.db)?.pending_operation(emitter.session)?;
    if let Some(reply) = approvals::handle(runtime, pending.as_ref(), text, emitter).await? {
        emitter.send("assistant.delta", json!({"text":reply}));
        let played = runtime.say(&reply, emitter).await?;
        return Ok((reply, played));
    }
    emitter.state("thinking");
    let provider = Arc::new(runtime.provider(true).await?);
    let settings = Arc::new(runtime.settings()?);
    let now = jiff::Timestamp::now()
        .to_zoned(
            jiff::tz::TimeZone::get(&settings.time_zone)
                .map_err(|_| AppError::invalid("Fuso inválido"))?,
        )
        .to_string();
    let active = runtime.integrations.active(&runtime.db, &settings)?;
    let inactive = runtime.integrations.inactive(&runtime.db, &settings)?;
    let mut messages = vec![
        json!({"role":"system","content":prompt::system_prompt(&settings,&now,&active,&inactive)}),
    ];
    if let Some(op) = pending {
        messages.push(json!({"role":"assistant","content":format!("Dados da proposta anterior (não confirmada ou de resultado pendente): {}",serde_json::to_string(&op)?)}));
    }
    let recent = database(&runtime.db)?.recent_messages(emitter.session, 20)?;
    let mut budget = (settings.provider.context_tokens as usize * 2)
        .saturating_sub(8000)
        .max(2000);
    let mut history = Vec::new();
    for msg in recent
        .iter()
        .rev()
        .filter(|m| ["user", "assistant"].contains(&m.role.as_str()))
    {
        let content = clip(&msg.text, budget.min(8000));
        if content.is_empty() {
            break;
        }
        budget = budget.saturating_sub(content.len());
        history.push(json!({"role":msg.role,"content":content}));
        if budget == 0 {
            break;
        }
    }
    history.reverse();
    messages.extend(history);
    let tools = runtime.tools()?;
    let speaker = runtime.speaker(emitter, &settings);
    let outcome = rounds(
        runtime,
        &provider,
        &mut messages,
        &tools,
        &settings,
        emitter,
        &speaker,
    )
    .await;
    let spoken = speaker.finish(emitter).await;
    let answer = outcome?;
    Ok((answer, spoken?))
}

async fn rounds(
    runtime: &Arc<Runtime>,
    provider: &Arc<Provider>,
    messages: &mut Vec<Value>,
    tools: &[Value],
    settings: &Settings,
    emitter: &Emitter,
    speaker: &Speaker,
) -> Result<String> {
    let (mut answer, mut calls, mut approval) = (String::new(), 0usize, false);
    for round in 0..=MAX_ROUNDS {
        if emitter.cancel.is_cancelled() {
            return Err(AppError::cancelled());
        }
        emitter.state("thinking");
        let last = approval || round == MAX_ROUNDS;
        let offered: Vec<Value> = if last { Vec::new() } else { tools.to_vec() };
        if approval {
            messages.push(json!({"role":"system","content":"Uma ação foi proposta e aguarda a confirmação do usuário no aplicativo. Resuma a proposta (e conflitos, se houver) e peça confirmação. Não diga que foi executada."}));
        } else if last {
            messages.push(json!({"role":"system","content":"Responda agora com o que foi obtido; não há mais ferramentas nesta resposta."}));
        }
        let space = !answer.is_empty() && !answer.ends_with(char::is_whitespace);
        let response = stream_round(
            provider,
            messages.clone(),
            offered,
            emitter,
            speaker.sender(),
            space,
        )
        .await?;
        if !response.text.is_empty() {
            if space {
                answer.push(' ');
            }
            answer.push_str(&response.text);
        }
        if last || response.calls.is_empty() {
            break;
        }
        calls += response.calls.len();
        if calls > MAX_TOOL_CALLS {
            return Err(AppError::new(
                "TOOL_BUDGET",
                "A consulta excedeu o limite de ferramentas. Faça uma pergunta mais específica.",
            ));
        }
        let specs: Vec<Option<ToolSpec>> = response
            .calls
            .iter()
            .map(|c| {
                runtime
                    .integrations
                    .spec(&c.name, &runtime.db, settings)
                    .ok()
                    .map(|(_, s)| s)
            })
            .collect();
        if specs
            .iter()
            .filter(|s| s.as_ref().is_some_and(|s| s.effect.requires_approval()))
            .count()
            > 1
        {
            return Err(AppError::new(
                "ACTION_AMBIGUOUS",
                "Prepare uma ação de cada vez",
            ));
        }
        messages.push(assistant_tools(&response));
        let all_read = specs
            .iter()
            .all(|s| s.as_ref().is_some_and(|s| s.effect == Effect::Read));
        if all_read {
            let worker = Arc::clone(runtime);
            let events = emitter.clone();
            let calls_and_specs = response
                .calls
                .iter()
                .cloned()
                .zip(specs.iter().cloned())
                .collect::<Vec<_>>();
            let mut results =
                futures_util::stream::iter(calls_and_specs.into_iter().map(move |(call, spec)| {
                    let runtime = worker.clone();
                    let events = events.clone();
                    async move { run_call(runtime, events, call, spec).await }
                }))
                .buffered(2);
            while let Some((call, result)) = results.next().await {
                record(runtime, messages, &call, result, emitter, &mut approval)?;
            }
        } else {
            for (call, spec) in response.calls.into_iter().zip(specs) {
                let (call, result) =
                    run_call(Arc::clone(runtime), emitter.clone(), call, spec).await;
                record(runtime, messages, &call, result, emitter, &mut approval)?;
            }
        }
    }
    Ok(answer)
}

async fn stream_round(
    provider: &Arc<Provider>,
    messages: Vec<Value>,
    tools: Vec<Value>,
    emitter: &Emitter,
    tx: Option<tokio::sync::mpsc::Sender<String>>,
    space: bool,
) -> Result<ChatResponse> {
    let event = emitter.clone();
    let cancel = emitter.cancel.clone();
    let provider = provider.clone();
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let mut sentences = Sentences::default();
        let mut first = space;
        let response = handle.block_on(provider.chat(&messages, &tools, None, &cancel, |delta| {
            if delta.is_empty() {
                return;
            }
            let text = if first {
                first = false;
                format!(" {delta}")
            } else {
                delta.to_string()
            };
            event.send("assistant.delta", json!({"text":text}));
            for sentence in sentences.push(&text) {
                if let Some(tx) = &tx {
                    send_sentence(tx, &cancel, sentence);
                }
            }
        }));
        if response.is_ok() {
            if let Some(sentence) = sentences.finish() {
                if let Some(tx) = &tx {
                    send_sentence(tx, &cancel, sentence);
                }
            }
        }
        drop(tx);
        response
    })
    .await
    .map_err(|_| AppError::new("PROVIDER_ERROR", "Geração interrompida"))?
}

async fn run_call(
    runtime: Arc<Runtime>,
    emitter: Emitter,
    call: ToolCall,
    spec: Option<ToolSpec>,
) -> (ToolCall, Result<Value>) {
    let label = spec
        .as_ref()
        .map(|s| s.label.clone())
        .unwrap_or_else(|| call.name.clone());
    let effect = spec.as_ref().map(|s| json!(s.effect));
    emitter.send(
        "tool.started",
        json!({"name":&call.name,"label":&label,"effect":effect}),
    );
    let result = tokio::select! {biased;_=emitter.cancel.cancelled()=>Err(AppError::cancelled()),result=runtime.execute_tool(&call,&emitter)=>result};
    emitter.send(
        "tool.completed",
        json!({"name":&call.name,"label":&label,"ok":result.is_ok()}),
    );
    (call, result)
}

fn record(
    runtime: &Runtime,
    messages: &mut Vec<Value>,
    call: &ToolCall,
    result: Result<Value>,
    emitter: &Emitter,
    approval: &mut bool,
) -> Result<()> {
    if emitter.cancel.is_cancelled() {
        return Err(AppError::cancelled());
    }
    let value = result.unwrap_or_else(|error| json!({"error":error}));
    database(&runtime.db)?.append(emitter.session, "tool", &clip(&value.to_string(), 12000))?;
    if let Some(sources) = value.get("sources") {
        emitter.send("sources", sources.clone());
    }
    if value["approval_required"] == true {
        *approval = true;
    }
    messages.push(json!({"role":"tool","tool_call_id":call.id,"name":call.name,"content":clip(&value.to_string(),6000)}));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{Paths, ProviderKind},
        integrations::{Integration, IntegrationManifest, ToolContext},
        provider::ProbeResult,
        runtime::{AppEvent, EventSink},
        speech,
    };
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use support_core::{store::Operation, tools::ActionField};
    use tokio_util::sync::CancellationToken;

    struct FakeWrite {
        prepared: AtomicUsize,
        executed: AtomicUsize,
    }

    impl Default for FakeWrite {
        fn default() -> Self {
            Self {
                prepared: AtomicUsize::new(0),
                executed: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl Integration for FakeWrite {
        fn manifest(&self) -> IntegrationManifest {
            IntegrationManifest {
                id: "fakewrite".into(),
                name: "Fake write".into(),
                description: "Teste de escrita".into(),
                documentation: vec![],
                builtin: true,
            }
        }
        fn tools(&self) -> Vec<ToolSpec> {
            vec![ToolSpec::new(
                "fakewrite",
                "fake_write",
                "Propondo escrita",
                "Escrita de teste",
                Effect::ExternalWrite,
                json!({"type":"object","properties":{},"additionalProperties":false}),
            )]
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
            assert_eq!(name, "fake_write");
            self.prepared.fetch_add(1, Ordering::SeqCst);
            Ok(Operation {
                id: "op".into(),
                session_id: Some(1),
                integration: "fakewrite".into(),
                account_id: "acc".into(),
                tool: "fake_write".into(),
                effect: Effect::ExternalWrite,
                summary: "Teste".into(),
                fields: vec![ActionField::new("Título", "Teste")],
                payload: json!({}),
                status: "awaiting_confirmation".into(),
                expires_at: i64::MAX,
                result: None,
            })
        }
    }

    fn test_paths(dir: &std::path::Path) -> Paths {
        Paths {
            data: dir.join("data"),
            resources: dir.join("resources"),
            python: dir.join("nonexistent-python"),
        }
    }

    fn emitter(session: i64, cancel: CancellationToken, sink: EventSink) -> Emitter {
        let dir = std::env::temp_dir();
        Emitter {
            backend: Arc::new(speech::backend(test_paths(&dir), "turbo")),
            current: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            session,
            turn: "test".into(),
            generation: 1,
            sequence: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            sink,
            cancel,
            generated: Arc::new(Mutex::new(String::new())),
            played: Arc::new(Mutex::new(String::new())),
        }
    }

    fn store_probe(runtime: &Runtime, settings: &Settings) {
        let probe = ProbeResult {
            config: settings.provider.clone(),
            verified_at: 7,
            fingerprint: settings.provider.model.clone(),
            streaming: true,
            tools: true,
            portuguese: true,
            cancellation: true,
        };
        database(&runtime.db)
            .unwrap()
            .set_json("provider_probe", &serde_json::to_value(probe).unwrap())
            .unwrap();
    }

    async fn ready_runtime(
        dir: &std::path::Path,
        base_url: &str,
        extra: Option<Arc<dyn Integration>>,
    ) -> (Arc<Runtime>, i64) {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut runtime = Runtime::open(Paths::new(dir.into(), root)).unwrap();
        if let Some(integration) = extra {
            Arc::get_mut(&mut runtime)
                .unwrap()
                .register_integration(integration)
                .unwrap();
        }
        let mut settings = Settings::default();
        settings.provider.kind = ProviderKind::CompatibleApi;
        settings.provider.base_url = base_url.into();
        settings.provider.model = "modelo".into();
        settings.speak_text = false;
        runtime
            .save_settings(settings.clone(), None, None, None)
            .await
            .unwrap();
        store_probe(&runtime, &settings);
        let session = runtime.session_create("t").unwrap().id;
        database(&runtime.db)
            .unwrap()
            .append(session, "user", "pergunta")
            .unwrap();
        (runtime, session)
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    fn content_length(headers: &str) -> usize {
        headers
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("content-length:"))
            .and_then(|line| {
                line.split_once(':')
                    .map(|(_, value)| value.trim().to_string())
            })
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    }

    fn spawn_provider(
        responder: impl Fn(usize, &Value) -> String + Send + Sync + 'static,
    ) -> (u16, Arc<Mutex<Vec<Value>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let bodies = Arc::clone(&recorded);
        let responder = Arc::new(responder);
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let bodies = Arc::clone(&bodies);
                let responder = Arc::clone(&responder);
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buffer = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let end = loop {
                        let read = match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => read,
                        };
                        buffer.extend_from_slice(&chunk[..read]);
                        if let Some(end) = find(&buffer, b"\r\n\r\n") {
                            break end + 4;
                        }
                    };
                    let headers = String::from_utf8_lossy(&buffer[..end - 4]).into_owned();
                    let length = content_length(&headers);
                    while buffer.len() < end + length {
                        let read = match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => read,
                        };
                        buffer.extend_from_slice(&chunk[..read]);
                    }
                    let Ok(body) = serde_json::from_slice::<Value>(&buffer[end..end + length])
                    else {
                        return;
                    };
                    let index = {
                        let mut records = bodies.lock().unwrap();
                        records.push(body.clone());
                        records.len() - 1
                    };
                    let payload = responder(index, &body);
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        (port, recorded)
    }

    fn sse_text(text: &str) -> String {
        let delta = json!({"choices":[{"delta":{"content":text},"finish_reason":null}]});
        let stop = json!({"choices":[{"delta":{},"finish_reason":"stop"}]});
        format!("data: {delta}\n\ndata: {stop}\n\ndata: [DONE]\n\n")
    }

    fn sse_calls(calls: &[(&str, &str, &str)]) -> String {
        let tool_calls = calls
            .iter()
            .enumerate()
            .map(|(index, (id, name, arguments))| {
                json!({"index":index,"id":id,"function":{"name":name,"arguments":arguments}})
            })
            .collect::<Vec<_>>();
        let chunk = json!({"choices":[{"delta":{"tool_calls":tool_calls},"finish_reason":null}]});
        let done = json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]});
        format!("data: {chunk}\n\ndata: {done}\n\ndata: [DONE]\n\n")
    }

    fn many_calls(count: usize) -> String {
        let calls = (0..count)
            .map(|index| {
                (
                    format!("c{index}"),
                    "memory_search".to_string(),
                    "{\"query\":\"x\"}".to_string(),
                )
            })
            .collect::<Vec<_>>();
        sse_calls(
            &calls
                .iter()
                .map(|(id, name, arguments)| (id.as_str(), name.as_str(), arguments.as_str()))
                .collect::<Vec<_>>(),
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_round_loop_executes_tool_and_answers() {
        let (port, bodies) = spawn_provider(|index, _| {
            if index == 0 {
                sse_calls(&[("c1", "memory_search", r#"{"query":"x"}"#)])
            } else {
                sse_text("Resposta final.")
            }
        });
        let directory = tempfile::tempdir().unwrap();
        let (runtime, session) = ready_runtime(
            directory.path(),
            &format!("http://127.0.0.1:{port}/v1"),
            None,
        )
        .await;
        let events = Arc::new(Mutex::new(Vec::<AppEvent>::new()));
        let output = Arc::clone(&events);
        let event = emitter(
            session,
            CancellationToken::new(),
            Arc::new(move |event| output.lock().unwrap().push(event)),
        );
        let (answer, played) = crate::agent::answer(&runtime, "pergunta", &event)
            .await
            .unwrap();
        assert_eq!(answer, "Resposta final.");
        assert_eq!(played, "");
        let events = events.lock().unwrap();
        let started = events
            .iter()
            .find(|e| e.kind == "tool.started")
            .expect("tool.started");
        assert_eq!(started.payload["label"], "Buscando na memória");
        assert_eq!(started.payload["effect"], "read");
        let completed = events
            .iter()
            .find(|e| e.kind == "tool.completed")
            .expect("tool.completed");
        assert_eq!(completed.payload["ok"], true);
        drop(events);
        let stored = database(&runtime.db).unwrap().messages(session).unwrap();
        assert!(stored.iter().any(|m| m.role == "tool"));
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        assert!(bodies[0]["tools"].is_array());
        assert!(
            bodies[1]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["role"] == "tool" && m["name"] == "memory_search")
        );
        let legacy = concat!("PRON", "TO");
        assert!(!bodies.iter().any(|b| b.to_string().contains(legacy)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn approval_makes_next_round_final_without_tools() {
        let (port, bodies) = spawn_provider(|index, _| {
            if index == 0 {
                sse_calls(&[("c1", "fake_write", "{}")])
            } else {
                sse_text("Confirma?")
            }
        });
        let directory = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeWrite::default());
        let (runtime, session) = ready_runtime(
            directory.path(),
            &format!("http://127.0.0.1:{port}/v1"),
            Some(fake.clone() as Arc<dyn Integration>),
        )
        .await;
        let events = Arc::new(Mutex::new(Vec::<AppEvent>::new()));
        let output = Arc::clone(&events);
        let event = emitter(
            session,
            CancellationToken::new(),
            Arc::new(move |event| output.lock().unwrap().push(event)),
        );
        let (answer, _) = crate::agent::answer(&runtime, "pergunta", &event)
            .await
            .unwrap();
        assert_eq!(answer, "Confirma?");
        assert!(
            events
                .lock()
                .unwrap()
                .iter()
                .any(|e| e.kind == "action.changed")
        );
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        assert!(bodies[1].get("tools").is_none());
        let messages = bodies[1]["messages"].as_array().unwrap();
        let last = messages.last().unwrap();
        assert_eq!(last["role"], "system");
        assert!(
            last["content"]
                .as_str()
                .unwrap()
                .contains("aguarda a confirmação")
        );
        assert_eq!(fake.prepared.load(Ordering::SeqCst), 1);
        assert_eq!(fake.executed.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn round_limit_forces_final_answer_without_tools() {
        let (port, bodies) = spawn_provider(|_, body| {
            if body.get("tools").is_some() {
                sse_calls(&[("c", "memory_search", r#"{"query":"x"}"#)])
            } else {
                sse_text("Fim.")
            }
        });
        let directory = tempfile::tempdir().unwrap();
        let (runtime, session) = ready_runtime(
            directory.path(),
            &format!("http://127.0.0.1:{port}/v1"),
            None,
        )
        .await;
        let event = emitter(session, CancellationToken::new(), Arc::new(|_| {}));
        let (answer, _) = crate::agent::answer(&runtime, "pergunta", &event)
            .await
            .unwrap();
        assert_eq!(answer, "Fim.");
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), MAX_ROUNDS + 1);
        for body in &bodies[..MAX_ROUNDS] {
            assert!(body["tools"].is_array());
        }
        assert!(bodies[MAX_ROUNDS].get("tools").is_none());
        let last = bodies[MAX_ROUNDS]["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap();
        assert_eq!(last["role"], "system");
        assert!(
            last["content"]
                .as_str()
                .unwrap()
                .contains("não há mais ferramentas")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn call_budget_returns_tool_budget_error() {
        let (port, bodies) = spawn_provider(|index, _| match index {
            0 | 1 => many_calls(6),
            _ => many_calls(1),
        });
        let directory = tempfile::tempdir().unwrap();
        let (runtime, session) = ready_runtime(
            directory.path(),
            &format!("http://127.0.0.1:{port}/v1"),
            None,
        )
        .await;
        let event = emitter(session, CancellationToken::new(), Arc::new(|_| {}));
        let error = crate::agent::answer(&runtime, "pergunta", &event)
            .await
            .unwrap_err();
        assert_eq!(error.code, "TOOL_BUDGET");
        assert_eq!(bodies.lock().unwrap().len(), 3);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_approval_calls_in_one_round_are_ambiguous() {
        let (port, _) = spawn_provider(|_, _| {
            sse_calls(&[("c1", "fake_write", "{}"), ("c2", "fake_write", "{}")])
        });
        let directory = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeWrite::default());
        let (runtime, session) = ready_runtime(
            directory.path(),
            &format!("http://127.0.0.1:{port}/v1"),
            Some(fake.clone() as Arc<dyn Integration>),
        )
        .await;
        let event = emitter(session, CancellationToken::new(), Arc::new(|_| {}));
        let error = crate::agent::answer(&runtime, "pergunta", &event)
            .await
            .unwrap_err();
        assert_eq!(error.code, "ACTION_AMBIGUOUS");
        assert_eq!(fake.prepared.load(Ordering::SeqCst), 0);
    }
}
