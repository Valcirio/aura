use crate::{
    AppError, Result,
    config::{ProviderConfig, ProviderKind},
    http,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use support_core::tools::{Effect, ToolSpec};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}
#[derive(Debug, Clone, Default)]
pub struct ChatResponse {
    pub text: String,
    pub calls: Vec<ToolCall>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    pub config: ProviderConfig,
    pub verified_at: i64,
    pub fingerprint: String,
    pub streaming: bool,
    pub tools: bool,
    pub portuguese: bool,
    pub cancellation: bool,
}

#[derive(Default)]
struct Collector {
    text: String,
    calls: BTreeMap<usize, (String, String, String)>,
    done: bool,
}
impl Collector {
    fn accept(
        &mut self,
        value: Value,
        ollama: bool,
        on_delta: &mut impl FnMut(&str),
    ) -> Result<()> {
        if value.get("error").is_some() {
            return Err(AppError::new(
                "PROVIDER_ERROR",
                "O modelo recusou a geração",
            ));
        }
        let delta = if ollama {
            &value["message"]
        } else {
            &value["choices"][0]["delta"]
        };
        if let Some(text) = delta["content"].as_str() {
            self.text.push_str(text);
            on_delta(text);
        }
        if self.text.len() > 64_000 {
            return Err(AppError::new(
                "RESPONSE_TOO_LARGE",
                "Resposta do modelo muito longa",
            ));
        }
        if let Some(calls) = delta["tool_calls"].as_array() {
            for (ordinal, call) in calls.iter().enumerate() {
                let index = call["index"]
                    .as_u64()
                    .or_else(|| call["function"]["index"].as_u64())
                    .unwrap_or(ordinal as u64) as usize;
                if index > 5 {
                    return Err(AppError::new(
                        "TOOL_BUDGET",
                        "Número de ferramentas excedido",
                    ));
                }
                let entry = self.calls.entry(index).or_default();
                if let Some(id) = call["id"].as_str() {
                    entry.0.push_str(id);
                }
                if let Some(name) = call["function"]["name"].as_str() {
                    entry.1.push_str(name);
                }
                match &call["function"]["arguments"] {
                    Value::String(fragment) => entry.2.push_str(fragment),
                    Value::Object(_) => {
                        entry.2 = serde_json::to_string(&call["function"]["arguments"])?
                    }
                    Value::Null => {}
                    _ => return Err(AppError::invalid("Argumentos de ferramenta inválidos")),
                }
                if entry.1.len() > 100 || entry.2.len() > 16384 {
                    return Err(AppError::new(
                        "TOOL_ARGUMENTS",
                        "Chamada de ferramenta muito longa",
                    ));
                }
            }
        }
        self.done |= if ollama {
            value["done"].as_bool() == Some(true)
        } else {
            !value["choices"][0]["finish_reason"].is_null()
        };
        Ok(())
    }
    fn finish(self) -> Result<ChatResponse> {
        if !self.done {
            return Err(AppError::new(
                "STREAM_TRUNCATED",
                "O provedor encerrou a resposta antes de concluir",
            )
            .retryable());
        }
        let calls = self
            .calls
            .into_iter()
            .map(|(i, (id, name, args))| {
                if name.is_empty() {
                    return Err(AppError::invalid("Ferramenta sem nome"));
                }
                Ok(ToolCall {
                    id: if id.is_empty() {
                        format!("call-{i}")
                    } else {
                        id
                    },
                    name,
                    arguments: serde_json::from_str(&args)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ChatResponse {
            text: self.text,
            calls,
        })
    }
}

#[derive(Default)]
struct Lines {
    buffer: Vec<u8>,
}
impl Lines {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>> {
        self.buffer.extend_from_slice(chunk);
        if self.buffer.len() > 1_000_000 {
            return Err(AppError::new(
                "STREAM_FRAME_TOO_LARGE",
                "Bloco do provedor muito grande",
            ));
        }
        let mut lines = Vec::new();
        while let Some(end) = self.buffer.iter().position(|c| *c == b'\n') {
            let mut line = self.buffer.drain(..=end).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            lines.push(line);
        }
        Ok(lines)
    }
}

fn native_messages(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .map(|message| {
            let mut m = message.clone();
            if let Some(calls) = m.get_mut("tool_calls").and_then(Value::as_array_mut) {
                for call in calls {
                    if let Some(args) = call["function"]["arguments"].as_str() {
                        call["function"]["arguments"] =
                            serde_json::from_str(args).unwrap_or(Value::Null);
                    }
                }
            }
            if m["role"] == "tool" {
                if let Some(name) = m.get("name").cloned() {
                    m["tool_name"] = name;
                }
            }
            m
        })
        .collect()
}

pub struct Provider {
    pub client: reqwest::Client,
    pub config: ProviderConfig,
    pub key: Option<String>,
}
impl Provider {
    pub async fn models(&self) -> Result<Vec<String>> {
        let suffix = if self.config.kind == ProviderKind::Ollama {
            "api/tags"
        } else {
            "models"
        };
        let mut request = self.client.get(self.config.endpoint(suffix));
        if let Some(key) = &self.key {
            request = request.bearer_auth(key);
        }
        let value = http::json(request.send().await.map_err(http::network_error)?).await?;
        let items = if self.config.kind == ProviderKind::Ollama {
            value["models"].as_array()
        } else {
            value["data"].as_array()
        };
        Ok(items
            .into_iter()
            .flatten()
            .filter_map(|v| v["name"].as_str().or_else(|| v["id"].as_str()))
            .map(String::from)
            .collect())
    }
    pub async fn fingerprint(&self) -> Result<String> {
        if self.config.kind == ProviderKind::CompatibleApi {
            return Ok(self.config.model.clone());
        }
        let show = http::json(
            self.client
                .post(self.config.endpoint("api/show"))
                .json(&json!({"model":self.config.model}))
                .send()
                .await
                .map_err(http::network_error)?,
        )
        .await?;
        if let Some(capabilities) = show["capabilities"].as_array() {
            if !capabilities.iter().any(|c| c == "tools") {
                return Err(AppError::new(
                    "MODEL_INCOMPATIBLE",
                    "Esse modelo não anuncia suporte a ferramentas",
                ));
            }
        }
        if let Some(info) = show["model_info"].as_object() {
            if let Some(context) = info
                .iter()
                .find(|(key, _)| key.ends_with(".context_length"))
                .and_then(|(_, v)| v.as_u64())
            {
                if context < u64::from(self.config.context_tokens) {
                    return Err(AppError::new(
                        "MODEL_INCOMPATIBLE",
                        "Contexto configurado excede o suportado pelo modelo",
                    ));
                }
            }
        }
        use sha2::{Digest, Sha256};
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(
                &json!({"template":show["template"],"details":show["details"],"model_info":show["model_info"],"modelfile":show["modelfile"]})
            )?)
        ))
    }
    pub async fn chat(
        &self,
        messages: &[Value],
        tools: &[Value],
        forced_tool: Option<&str>,
        cancel: &CancellationToken,
        mut on_delta: impl FnMut(&str),
    ) -> Result<ChatResponse> {
        self.config.validate()?;
        let messages = fit_context(messages, tools, self.config.context_tokens)?;
        let ollama = self.config.kind == ProviderKind::Ollama;
        let mut body = if ollama {
            json!({"model":self.config.model,"messages":native_messages(&messages),"stream":true,"think":false,"options":{"num_ctx":self.config.context_tokens,"num_predict":2048}})
        } else {
            json!({"model":self.config.model,"messages":messages,"stream":true,"max_tokens":2048})
        };
        if !tools.is_empty() {
            body["tools"] = json!(tools);
            if !ollama {
                body["tool_choice"] = forced_tool.map_or_else(
                    || json!("auto"),
                    |name| json!({"type":"function","function":{"name":name}}),
                );
            }
        }
        let mut request = self
            .client
            .post(self.config.endpoint(if ollama {
                "api/chat"
            } else {
                "chat/completions"
            }))
            .json(&body);
        if let Some(key) = &self.key {
            request = request.bearer_auth(key);
        }
        let response = tokio::select! {biased;_=cancel.cancelled()=>return Err(AppError::cancelled()),r=request.send()=>r.map_err(http::network_error)?};
        if !response.status().is_success() {
            return Err(http::status_error(response.status().as_u16()));
        }
        let mut stream = response.bytes_stream();
        let mut lines = Lines::default();
        let mut collector = Collector::default();
        loop {
            let chunk = tokio::select! {biased;_=cancel.cancelled()=>return Err(AppError::cancelled()),r=stream.next()=>r};
            let Some(chunk) = chunk else {
                break;
            };
            for line in lines.push(&chunk.map_err(http::network_error)?)? {
                if line.is_empty() || line.starts_with(b":") {
                    continue;
                }
                let line = if ollama {
                    line.as_slice()
                } else if let Some(data) = line.strip_prefix(b"data:") {
                    data.trim_ascii()
                } else {
                    continue;
                };
                if line == b"[DONE]" {
                    collector.done = true;
                    continue;
                }
                collector.accept(serde_json::from_slice(line)?, ollama, &mut on_delta)?;
            }
        }
        if !lines.buffer.is_empty() {
            let line = lines.buffer.trim_ascii();
            let line = if ollama {
                Some(line)
            } else {
                line.strip_prefix(b"data:").map(<[u8]>::trim_ascii)
            };
            if let Some(line) = line {
                if line == b"[DONE]" {
                    collector.done = true;
                } else if !line.is_empty() {
                    collector.accept(serde_json::from_slice(line)?, ollama, &mut on_delta)?;
                }
            }
        }
        if cancel.is_cancelled() {
            return Err(AppError::cancelled());
        }
        collector.finish()
    }
    pub async fn probe(&self) -> Result<ProbeResult> {
        let fingerprint = self.fingerprint().await?;
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let definition = ToolSpec::new(
            "builtin",
            "compatibility_echo",
            "Teste",
            "Ferramenta de teste sem efeito externo.",
            Effect::Read,
            json!({"type":"object","properties":{"nonce":{"type":"string"}},"required":["nonce"],"additionalProperties":false}),
        )
        .definition();
        let mut messages = vec![
            json!({"role":"user","content":format!("Chame obrigatoriamente compatibility_echo com nonce={nonce}. Não responda em texto antes da ferramenta.")}),
        ];
        let cancel = CancellationToken::new();
        let response = self
            .chat(
                &messages,
                &[definition],
                Some("compatibility_echo"),
                &cancel,
                |_| {},
            )
            .await?;
        if response.calls.len() != 1
            || response.calls[0].name != "compatibility_echo"
            || response.calls[0].arguments != json!({"nonce":nonce})
        {
            return Err(AppError::new(
                "MODEL_INCOMPATIBLE",
                "O modelo não executou corretamente o teste de ferramentas",
            ));
        }
        messages.push(assistant_tools(&response));
        messages.push(json!({"role":"tool","tool_call_id":response.calls[0].id,"name":"compatibility_echo","content":json!({"nonce":nonce}).to_string()}));
        messages.push(json!({"role":"user","content":"Responda exatamente em português: pronto para ajudar."}));
        let mut deltas = 0;
        let final_response = self
            .chat(&messages, &[], None, &cancel, |_| deltas += 1)
            .await?;
        if deltas == 0
            || !final_response
                .text
                .to_lowercase()
                .contains("pronto para ajudar")
        {
            return Err(AppError::new(
                "MODEL_INCOMPATIBLE",
                "Resposta em português ou streaming não foram validados",
            ));
        }
        let cancel_test = CancellationToken::new();
        let trigger = cancel_test.clone();
        let interrupted=self.chat(&[json!({"role":"user","content":"Conte de um a cem em português, escrevendo os números por extenso."})],&[],None,&cancel_test,move |_|trigger.cancel()).await;
        if !matches!(interrupted,Err(ref e) if e.code=="CANCELLED") {
            return Err(AppError::new(
                "MODEL_INCOMPATIBLE",
                "Não foi possível validar cancelamento do streaming",
            ));
        }
        Ok(ProbeResult {
            config: self.config.clone(),
            verified_at: crate::now(),
            fingerprint,
            streaming: true,
            tools: true,
            portuguese: true,
            cancellation: true,
        })
    }
}

pub fn assistant_tools(response: &ChatResponse) -> Value {
    json!({"role":"assistant","content":response.text,"tool_calls":response.calls.iter().map(|c|json!({"id":c.id,"type":"function","function":{"name":c.name,"arguments":c.arguments.to_string()}})).collect::<Vec<_>>()})
}

fn fit_context(input: &[Value], tools: &[Value], context: u32) -> Result<Vec<Value>> {
    // Orçamento conservador por caracteres. Reserva a saída e as definições de ferramentas.
    let budget = ((context as usize).saturating_sub(2048) * 5 / 2)
        .saturating_sub(serde_json::to_string(tools)?.len());
    let mut messages = input.to_vec();
    let size = |messages: &[Value]| {
        serde_json::to_string(messages)
            .map(|s| s.chars().count())
            .unwrap_or(usize::MAX)
    };
    while size(&messages) > budget {
        let latest_user = messages
            .iter()
            .rposition(|m| m["role"] == "user")
            .unwrap_or(0);
        if let Some(index) = messages
            .iter()
            .enumerate()
            .find(|(index, m)| {
                *index > 0
                    && *index < latest_user
                    && m["tool_calls"].is_null()
                    && ["user", "assistant"].contains(&m["role"].as_str().unwrap_or(""))
            })
            .map(|(i, _)| i)
        {
            messages.remove(index);
            continue;
        }
        let mut shortened = false;
        for message in &mut messages {
            if message["role"] == "tool" {
                if let Some(content) = message["content"].as_str() {
                    if content.chars().count() > 128 {
                        let count = content.chars().count() / 2;
                        let short = content.chars().take(count).collect::<String>();
                        message["content"] = json!(format!(
                            "{short} [resultado reduzido por limite de contexto]"
                        ));
                        shortened = true;
                    }
                }
            }
        }
        if !shortened {
            return Err(AppError::new(
                "CONTEXT_LIMIT",
                "O pedido e os dados excedem o contexto configurado. Reduza o intervalo ou use um contexto maior.",
            ));
        }
    }
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stream_preserves_utf8_across_arbitrary_network_boundaries() {
        let source = "data: {\"choices\":[{\"delta\":{\"content\":\"Olá, conexão!\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";
        let mut lines = Lines::default();
        let mut collector = Collector::default();
        for chunk in source.as_bytes().chunks(1) {
            for line in lines.push(chunk).unwrap() {
                if let Some(data) = line.strip_prefix(b"data:") {
                    let data = data.trim_ascii();
                    if data == b"[DONE]" {
                        collector.done = true;
                    } else {
                        collector
                            .accept(serde_json::from_slice(data).unwrap(), false, &mut |_| {})
                            .unwrap();
                    }
                }
            }
        }
        assert_eq!(collector.finish().unwrap().text, "Olá, conexão!");
    }
    #[test]
    fn assembles_fragmented_tool_arguments_and_rejects_truncation() {
        let mut c = Collector::default();
        for args in ["{\"calendar_", "id\":\"primary\"}"] {
            c.accept(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":args}}]}}]}),false,&mut |_|{}).unwrap();
        }
        c.accept(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"calendar_list"}}]},"finish_reason":"tool_calls"}]}),false,&mut |_|{}).unwrap();
        assert_eq!(
            c.finish().unwrap().calls[0].arguments,
            json!({"calendar_id":"primary"})
        );
        assert!(Collector::default().finish().is_err());
    }
    #[test]
    fn context_budget_keeps_system_latest_user_and_tool_pairs() {
        let messages = vec![
            json!({"role":"system","content":"Política"}),
            json!({"role":"user","content":"x".repeat(30000)}),
            json!({"role":"assistant","content":"histórico"}),
            json!({"role":"user","content":"Pedido atual"}),
            json!({"role":"assistant","tool_calls":[{"id":"call-a","function":{"name":"memory_search","arguments":"{}"}}]}),
            json!({"role":"tool","tool_call_id":"call-a","content":"evidência".repeat(5000)}),
        ];
        let fitted = fit_context(&messages, &[], 8192).unwrap();
        assert_eq!(fitted[0]["content"], "Política");
        assert!(fitted.iter().any(|m| m["content"] == "Pedido atual"));
        assert!(fitted.iter().any(|m| m["tool_calls"][0]["id"] == "call-a"));
        assert!(fitted.iter().any(|m| m["tool_call_id"] == "call-a"));
        assert!(serde_json::to_string(&fitted).unwrap().chars().count() < 16000);
    }
}
