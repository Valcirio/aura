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
    let current = recent
        .iter()
        .rposition(|m| m.role == "user")
        .map(|index| (recent[index].id, recent[index].text.as_str()))
        .ok_or_else(|| {
            AppError::new(
                "INTERNAL",
                "Histórico inconsistente: mensagem atual ausente da conversa",
            )
        })?;
    if current.1 != text {
        return Err(AppError::new(
            "INTERNAL",
            "Histórico inconsistente: mensagem atual não corresponde ao pedido",
        ));
    }
    let mut budget = (settings.provider.context_tokens as usize * 2)
        .saturating_sub(8000)
        .max(2000);
    let mut history = Vec::new();
    let mut reached_current = false;
    for msg in recent
        .iter()
        .rev()
        .filter(|m| ["user", "assistant"].contains(&m.role.as_str()))
    {
        if msg.id == current.0 {
            history.push(json!({"role":"user","content":text}));
            reached_current = true;
            continue;
        }
        if !reached_current {
            continue;
        }
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
        emitter.trace("provider_round_start", Some(messages.len()), None, None);
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
    let request_messages = messages.len();
    let (response, deltas) = tokio::task::spawn_blocking(move || {
        let mut sentences = Sentences::default();
        let mut first = space;
        let mut deltas = 0usize;
        let response = handle.block_on(provider.chat(&messages, &tools, None, &cancel, |delta| {
            if delta.is_empty() {
                return;
            }
            deltas += 1;
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
        (response, deltas)
    })
    .await
    .map_err(|_| AppError::new("PROVIDER_ERROR", "Geração interrompida"))?;
    emitter.trace(
        "provider_round_end",
        Some(request_messages),
        Some(deltas),
        response.as_ref().err().map(|error| error.code.as_str()),
    );
    response
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

    struct HugeTool;

    #[async_trait::async_trait]
    impl Integration for HugeTool {
        fn manifest(&self) -> IntegrationManifest {
            IntegrationManifest {
                id: "huge".into(),
                name: "Ferramenta sintética".into(),
                description: "Ocupa o orçamento de contexto".into(),
                documentation: vec![],
                builtin: true,
            }
        }
        fn tools(&self) -> Vec<ToolSpec> {
            vec![ToolSpec {
                name: "huge_lookup".into(),
                label: "Consultando".into(),
                description: "descrição sintética ".repeat(4000),
                parameters: json!({"type":"object","properties":{},"additionalProperties":false}),
                effect: Effect::Read,
                integration: "huge".into(),
            }]
        }
        async fn execute(
            &self,
            _name: &str,
            _args: Value,
            _ctx: &ToolContext<'_>,
        ) -> Result<Value> {
            Ok(json!({}))
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

    async fn open_runtime(
        dir: &std::path::Path,
        base_url: &str,
        extra: Option<Arc<dyn Integration>>,
        context_tokens: u32,
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
        settings.provider.context_tokens = context_tokens;
        settings.speak_text = false;
        runtime
            .save_settings(settings.clone(), None, None, None)
            .await
            .unwrap();
        store_probe(&runtime, &settings);
        let session = runtime.session_create("t").unwrap().id;
        (runtime, session)
    }

    async fn ready_runtime(
        dir: &std::path::Path,
        base_url: &str,
        extra: Option<Arc<dyn Integration>>,
    ) -> (Arc<Runtime>, i64) {
        let (runtime, session) = open_runtime(dir, base_url, extra, 8192).await;
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

    fn sse_deltas(deltas: &[&str]) -> String {
        let mut payload = String::new();
        for text in deltas {
            let delta = json!({"choices":[{"delta":{"content":text},"finish_reason":null}]});
            payload.push_str(&format!("data: {delta}\n\n"));
        }
        let stop = json!({"choices":[{"delta":{},"finish_reason":"stop"}]});
        payload.push_str(&format!("data: {stop}\n\ndata: [DONE]\n\n"));
        payload
    }

    fn event_sink() -> (
        EventSink,
        Arc<Mutex<Vec<AppEvent>>>,
        tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    ) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let output = Arc::clone(&events);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let sink: EventSink = Arc::new(move |event| {
            let _ = tx.send(event.clone());
            output.lock().unwrap().push(event);
        });
        (sink, events, rx)
    }

    async fn wait_for_run(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>) {
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
                .await
                .expect("tempo esgotado aguardando run.completed")
                .expect("canal de eventos encerrado");
            if event.kind == "run.completed" {
                return;
            }
        }
    }

    fn event_text(events: &[&AppEvent], kind: &str) -> String {
        events
            .iter()
            .filter(|event| event.kind == kind)
            .filter_map(|event| event.payload["text"].as_str())
            .collect()
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_text_turns_stream_persist_and_replay_only_own_content() {
        let first = "Explique o mesmo trecho repetido.";
        let second = "Repita o mesmo trecho repetido, por favor.";
        let first_deltas = [
            "Primeira resposta. ",
            "Mesmo trecho ",
            "repetido. ",
            "Mesmo trecho repetido.",
        ];
        let second_deltas = ["Segunda resposta. ", "Mesmo trecho ", "repetido."];
        let first_answer = first_deltas.concat();
        let second_answer = second_deltas.concat();
        let (port, bodies) = spawn_provider(move |index, _| match index {
            0 => sse_calls(&[("c1", "memory_search", r#"{"query":"mesmo trecho"}"#)]),
            1 => sse_deltas(&first_deltas),
            _ => sse_deltas(&second_deltas),
        });
        let directory = tempfile::tempdir().unwrap();
        let (runtime, session) = open_runtime(
            directory.path(),
            &format!("http://127.0.0.1:{port}/v1"),
            None,
            8192,
        )
        .await;
        let (sink, events, mut rx) = event_sink();
        let first_ticket = runtime
            .start(session, Some(first.to_string()), sink.clone())
            .await
            .unwrap();
        wait_for_run(&mut rx).await;
        let second_ticket = runtime
            .start(session, Some(second.to_string()), sink)
            .await
            .unwrap();
        wait_for_run(&mut rx).await;

        assert_eq!(first_ticket.session_id, session);
        assert_eq!(second_ticket.session_id, session);
        assert_ne!(first_ticket.turn_id, second_ticket.turn_id);
        assert_ne!(first_ticket.generation, second_ticket.generation);
        assert!(second_ticket.generation > first_ticket.generation);

        let events = events.lock().unwrap();
        let first_events: Vec<&AppEvent> = events
            .iter()
            .filter(|event| event.turn_id == first_ticket.turn_id)
            .collect();
        let second_events: Vec<&AppEvent> = events
            .iter()
            .filter(|event| event.turn_id == second_ticket.turn_id)
            .collect();
        for turn in [&first_events, &second_events] {
            assert!(!turn.is_empty());
            assert_eq!(
                turn.iter().map(|event| event.sequence).collect::<Vec<_>>(),
                (0..turn.len() as u64).collect::<Vec<_>>()
            );
            assert_eq!(turn.first().unwrap().kind, "run.started");
            assert_eq!(turn.last().unwrap().kind, "run.completed");
        }
        assert_eq!(event_text(&first_events, "assistant.delta"), first_answer);
        assert_eq!(event_text(&second_events, "assistant.delta"), second_answer);
        assert!(!event_text(&second_events, "assistant.delta").contains(&first_answer));
        let first_at = |kind: &str| {
            first_events
                .iter()
                .position(|event| event.kind == kind)
                .unwrap_or_else(|| panic!("evento {kind} ausente no primeiro turno"))
        };
        assert!(first_at("transcript.final") < first_at("tool.started"));
        assert!(first_at("tool.started") < first_at("tool.completed"));
        assert!(first_at("tool.completed") < first_at("assistant.delta"));
        let last_delta = first_events
            .iter()
            .rposition(|event| event.kind == "assistant.delta")
            .unwrap();
        assert!(last_delta < first_at("turn.completed"));
        assert!(first_at("turn.completed") < first_at("run.completed"));
        let completed = first_events
            .iter()
            .find(|event| event.kind == "turn.completed")
            .unwrap();
        assert_eq!(completed.payload["text"], first_answer);
        let completed = second_events
            .iter()
            .find(|event| event.kind == "turn.completed")
            .unwrap();
        assert_eq!(completed.payload["text"], second_answer);
        drop(events);

        let stored = database(&runtime.db).unwrap().messages(session).unwrap();
        assert_eq!(
            stored.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
            ["user", "tool", "assistant", "user", "assistant"]
        );
        assert_eq!(stored[0].text, first);
        assert_eq!(stored[2].text, first_answer);
        assert_eq!(stored[3].text, second);
        assert_eq!(stored[4].text, second_answer);

        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 3);
        let initial = bodies[0]["messages"].as_array().unwrap();
        let users: Vec<_> = initial
            .iter()
            .filter(|message| message["role"] == "user")
            .collect();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0]["content"], first);
        assert_eq!(initial.last().unwrap()["content"], first);
        let after_tool = bodies[1]["messages"].as_array().unwrap();
        let users: Vec<_> = after_tool
            .iter()
            .filter(|message| message["role"] == "user")
            .collect();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0]["content"], first);
        let assistant = after_tool
            .iter()
            .find(|message| message["tool_calls"].is_array())
            .expect("assistant com tool_calls");
        assert_eq!(assistant["tool_calls"][0]["id"], "c1");
        let tool = after_tool
            .iter()
            .find(|message| message["role"] == "tool")
            .expect("resultado de ferramenta");
        assert_eq!(tool["tool_call_id"], "c1");
        assert_eq!(tool["name"], "memory_search");
        let second_initial = bodies[2]["messages"].as_array().unwrap();
        let users: Vec<_> = second_initial
            .iter()
            .filter(|message| message["role"] == "user")
            .collect();
        assert_eq!(users.len(), 2);
        assert_eq!(users[0]["content"], first);
        assert_eq!(users[1]["content"], second);
        let assistants: Vec<_> = second_initial
            .iter()
            .filter(|message| message["role"] == "assistant")
            .collect();
        assert_eq!(assistants.len(), 1);
        assert_eq!(assistants[0]["content"], first_answer);
        assert!(
            !second_initial
                .iter()
                .any(|message| message["content"] == second_answer)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn long_multibyte_current_input_reaches_provider_intact_under_pressure() {
        let (port, bodies) = spawn_provider(|_, _| sse_deltas(&["Resposta curta."]));
        let directory = tempfile::tempdir().unwrap();
        let (runtime, session) = open_runtime(
            directory.path(),
            &format!("http://127.0.0.1:{port}/v1"),
            None,
            16384,
        )
        .await;
        {
            let db = database(&runtime.db).unwrap();
            for index in 0..8 {
                let role = if index % 2 == 0 { "user" } else { "assistant" };
                db.append(
                    session,
                    role,
                    &format!("historico-{index} {}", "á".repeat(6200)),
                )
                .unwrap();
            }
        }
        let current = format!("{}{}", "a".repeat(10500), "á".repeat(500));
        assert!(current.len() > 8000 && current.len() <= 12000);
        let (sink, events, mut rx) = event_sink();
        runtime
            .start(session, Some(current.clone()), sink)
            .await
            .unwrap();
        wait_for_run(&mut rx).await;
        assert!(
            !events
                .lock()
                .unwrap()
                .iter()
                .any(|event| event.kind == "error")
        );
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1);
        let messages = bodies[0]["messages"].as_array().unwrap();
        let latest_user = messages
            .iter()
            .rev()
            .find(|message| message["role"] == "user")
            .expect("mensagem atual");
        assert_eq!(latest_user["content"], current.as_str());
        assert_eq!(
            messages
                .iter()
                .filter(
                    |message| message["role"] == "user" && message["content"] == current.as_str()
                )
                .count(),
            1
        );
        let mut retained = Vec::new();
        for message in messages {
            let Some(content) = message["content"].as_str() else {
                continue;
            };
            let Some(rest) = content.strip_prefix("historico-") else {
                continue;
            };
            if let Some(index) = rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<usize>().ok())
            {
                retained.push(index);
            }
        }
        assert!(
            !retained.is_empty(),
            "o histórico recente deveria permanecer: {retained:?}"
        );
        assert!(
            retained.len() < 8,
            "a pressão de contexto deveria remover histórico antigo: {retained:?}"
        );
        assert_eq!(retained.last(), Some(&7));
        assert_eq!(retained, (retained[0]..=7).collect::<Vec<_>>());
        assert!(
            retained[0] > 0,
            "o histórico mais antigo deveria sair primeiro"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn oversized_current_input_fails_with_context_limit_before_http() {
        let (port, bodies) = spawn_provider(|_, _| sse_deltas(&["não deveria ser enviado"]));
        let directory = tempfile::tempdir().unwrap();
        let (runtime, session) = open_runtime(
            directory.path(),
            &format!("http://127.0.0.1:{port}/v1"),
            Some(Arc::new(HugeTool) as Arc<dyn Integration>),
            8192,
        )
        .await;
        let current = "a".repeat(12000);
        let (sink, events, mut rx) = event_sink();
        let ticket = runtime.start(session, Some(current), sink).await.unwrap();
        wait_for_run(&mut rx).await;
        assert!(bodies.lock().unwrap().is_empty());
        let events = events.lock().unwrap();
        let error = events
            .iter()
            .find(|event| event.kind == "error")
            .expect("erro do turno");
        assert_eq!(error.payload["code"], "CONTEXT_LIMIT");
        assert_eq!(error.turn_id, ticket.turn_id);
        assert!(!events.iter().any(|event| event.kind == "assistant.delta"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sentence_queue_matches_round_deltas_and_tool_boundaries() {
        let final_deltas = [
            "Uma frase longa o suficiente para passar do limite de quarenta e cinco. ",
            "Trecho igual. ",
            "Trecho igual.",
            " Fim.",
        ];
        let (port, bodies) = spawn_provider(move |index, _| match index {
            0 => sse_calls(&[("c1", "memory_search", r#"{"query":"trecho"}"#)]),
            _ => sse_deltas(&final_deltas),
        });
        let provider = Arc::new(Provider {
            client: reqwest::Client::new(),
            config: crate::config::ProviderConfig {
                kind: ProviderKind::CompatibleApi,
                base_url: format!("http://127.0.0.1:{port}/v1"),
                model: "modelo".into(),
                context_tokens: 8192,
            },
            key: None,
        });
        let events = Arc::new(Mutex::new(Vec::<AppEvent>::new()));
        let output = Arc::clone(&events);
        let event = emitter(
            1,
            CancellationToken::new(),
            Arc::new(move |event| output.lock().unwrap().push(event)),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let tool_round = stream_round(
            &provider,
            vec![json!({"role":"user","content":"pedido"})],
            vec![],
            &event,
            Some(tx),
            false,
        )
        .await
        .unwrap();
        assert_eq!(tool_round.calls.len(), 1);
        assert!(tool_round.text.is_empty());
        let mut tool_queue = Vec::new();
        while let Some(sentence) = rx.recv().await {
            tool_queue.push(sentence);
        }
        assert!(
            tool_queue.is_empty(),
            "rodada de ferramenta não enfileira texto: {tool_queue:?}"
        );
        let messages = vec![
            json!({"role":"user","content":"pedido"}),
            assistant_tools(&tool_round),
            json!({"role":"tool","tool_call_id":"c1","name":"memory_search","content":"{\"sources\":[]}"}),
        ];
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let final_round = stream_round(&provider, messages, vec![], &event, Some(tx), false)
            .await
            .unwrap();
        assert!(final_round.calls.is_empty());
        assert_eq!(final_round.text, final_deltas.concat());
        let mut queue = Vec::new();
        while let Some(sentence) = rx.recv().await {
            queue.push(sentence);
        }
        assert!(queue.len() > 1);
        assert_eq!(queue.concat(), final_deltas.concat());
        let events = events.lock().unwrap();
        let deltas = event_text(&events.iter().collect::<Vec<_>>(), "assistant.delta");
        assert_eq!(deltas, final_deltas.concat());
        assert_eq!(bodies.lock().unwrap().len(), 2);
    }
}
