use leptos::prelude::*;
use serde_json::{Value, json};
use wasm_bindgen::{closure::Closure, prelude::*};

#[wasm_bindgen(module = "/public/bridge.js")]
extern "C" {
    #[wasm_bindgen(js_name=isDesktop)]
    fn desktop() -> bool;
    #[wasm_bindgen(catch,js_name=request)]
    async fn request_js(command: &str, args: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch,js_name=requestStream)]
    async fn stream_js(
        command: &str,
        args: &str,
        callback: &JsValue,
        id: u32,
    ) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name=disposeStream)]
    fn dispose_stream_js(id: u32);
    #[wasm_bindgen(js_name=applyOrbLevel)]
    fn orb_level(level: f64);
}

static NEXT_STREAM: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
/// Unique, JS-safe local identifier for a bridge stream.
fn next_stream_id() -> u32 {
    NEXT_STREAM.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

async fn request(command: &str, args: Value) -> Result<Value, String> {
    let value = request_js(command, &args.to_string())
        .await
        .map_err(js_error)?;
    serde_json::from_str(&value.as_string().unwrap_or_default())
        .map_err(|_| "Resposta do aplicativo inválida".into())
}
fn js_error(value: JsValue) -> String {
    let raw = value
        .as_string()
        .unwrap_or_else(|| "Não foi possível acessar o aplicativo".into());
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|v| v["message"].as_str().map(String::from))
        .unwrap_or(raw)
}
fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").to_string()
}
fn array(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

#[derive(Clone)]
struct ChatLine {
    id: String,
    role: String,
    text: String,
}
#[derive(Clone, Copy)]
struct Ui {
    page: RwSignal<&'static str>,
    settings: RwSignal<Value>,
    sessions: RwSignal<Vec<Value>>,
    messages: RwSignal<Vec<ChatLine>>,
    sources: RwSignal<Vec<Value>>,
    session: RwSignal<i64>,
    state: RwSignal<String>,
    running: RwSignal<bool>,
    generation: RwSignal<u64>,
    epoch: RwSignal<u64>,
    active_stream: RwSignal<Option<u32>>,
    message_cursor: RwSignal<Option<i64>>,
    more_messages: RwSignal<bool>,
    notice: RwSignal<String>,
    busy: RwSignal<bool>,
    input: RwSignal<String>,
    memory_query: RwSignal<String>,
    memory_results: RwSignal<Vec<Value>>,
    service: RwSignal<Value>,
    integrations: RwSignal<Vec<Value>>,
    tools: RwSignal<Vec<(String, String, String)>>,
    calendars: RwSignal<Vec<Value>>,
    models: RwSignal<Vec<String>>,
    provider_state: RwSignal<&'static str>,
    provider_error: RwSignal<String>,
    check_generation: RwSignal<u64>,
    audio_error: RwSignal<String>,
    artifacts: RwSignal<Value>,
    devices: RwSignal<Vec<Value>>,
    operation: RwSignal<Value>,
    provider_key: RwSignal<String>,
    brave_key: RwSignal<String>,
    google_secret: RwSignal<String>,
    download: RwSignal<String>,
    speech: RwSignal<Value>,
    speech_poll: RwSignal<u64>,
    speech_asr: RwSignal<String>,
    devices_known: RwSignal<bool>,
    voice_attempted: RwSignal<bool>,
    notices: RwSignal<NoticeGate>,
    persisted_input: RwSignal<String>,
    saving: RwSignal<bool>,
    input_level: RwSignal<f64>,
    input_feedback: RwSignal<String>,
}
impl Ui {
    fn new() -> Self {
        Self {
            page: RwSignal::new("conversation"),
            settings: RwSignal::new(json!({})),
            sessions: RwSignal::new(Vec::new()),
            messages: RwSignal::new(Vec::new()),
            sources: RwSignal::new(Vec::new()),
            session: RwSignal::new(0),
            state: RwSignal::new("idle".into()),
            running: RwSignal::new(false),
            generation: RwSignal::new(0),
            epoch: RwSignal::new(0),
            active_stream: RwSignal::new(None),
            message_cursor: RwSignal::new(None),
            more_messages: RwSignal::new(false),
            notice: RwSignal::new(String::new()),
            busy: RwSignal::new(false),
            input: RwSignal::new(String::new()),
            memory_query: RwSignal::new(String::new()),
            memory_results: RwSignal::new(Vec::new()),
            service: RwSignal::new(json!({})),
            integrations: RwSignal::new(Vec::new()),
            tools: RwSignal::new(Vec::new()),
            calendars: RwSignal::new(Vec::new()),
            models: RwSignal::new(Vec::new()),
            provider_state: RwSignal::new("unavailable"),
            provider_error: RwSignal::new(String::new()),
            check_generation: RwSignal::new(0),
            audio_error: RwSignal::new(String::new()),
            artifacts: RwSignal::new(Value::Null),
            devices: RwSignal::new(Vec::new()),
            operation: RwSignal::new(Value::Null),
            provider_key: RwSignal::new(String::new()),
            brave_key: RwSignal::new(String::new()),
            google_secret: RwSignal::new(String::new()),
            download: RwSignal::new(String::new()),
            speech: RwSignal::new(json!({"phase":"idle","ready":false,"active":false})),
            speech_poll: RwSignal::new(0),
            speech_asr: RwSignal::new(String::new()),
            devices_known: RwSignal::new(false),
            voice_attempted: RwSignal::new(false),
            notices: RwSignal::new(NoticeGate::default()),
            persisted_input: RwSignal::new(String::new()),
            saving: RwSignal::new(false),
            input_level: RwSignal::new(0.),
            input_feedback: RwSignal::new(String::new()),
        }
    }
    fn task(self, future: impl std::future::Future<Output = Result<(), String>> + 'static) {
        self.busy.set(true);
        self.notice.set(String::new());
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(error) = future.await {
                self.notice.set(error);
            }
            self.busy.set(false);
        });
    }
    fn set(self, key: &str, value: Value) {
        self.settings.update(|v| v[key] = value);
    }
    fn provider_set(self, key: &str, value: Value) {
        self.settings.update(|v| v["provider"][key] = value);
        self.invalidate_provider();
    }
    /// Provider fields or credential input changed: any check in flight is now stale.
    fn invalidate_provider(self) {
        self.check_generation.update(|n| *n += 1);
        self.provider_state.set("unavailable");
        self.provider_error.set(PROVIDER_CHANGED.into());
    }
    /// Applies a check response only if no newer check or invalidation happened meanwhile.
    fn apply_check(self, generation: u64, result: Result<Value, String>) {
        match check_outcome(self.check_generation.get_untracked(), generation, result) {
            Some(Readiness::Ready) => {
                self.provider_state.set("ready");
                self.provider_error.set(String::new());
            }
            Some(Readiness::Unavailable(error)) => {
                self.provider_state.set("unavailable");
                self.provider_error.set(error);
            }
            None => {}
        }
    }
    /// Starts a provider check in its own future: independent of the global busy state.
    fn start_check(self) {
        self.check_generation.update(|n| *n += 1);
        let generation = self.check_generation.get_untracked();
        self.provider_state.set("checking");
        self.provider_error.set(String::new());
        wasm_bindgen_futures::spawn_local(async move {
            let result = request("provider_check", json!({})).await;
            self.apply_check(generation, result);
        });
    }
    /// Applies a speech snapshot unless a newer preparation has been started meanwhile.
    fn apply_speech(self, token: u64, snapshot: Value) {
        if self.speech_poll.get_untracked() == token {
            self.speech.set(snapshot);
        }
    }
    /// Starts (or retries) native speech preparation and polls its cached status.
    /// Independent of provider checking, busy state and text input.
    fn start_speech(self) {
        self.speech_poll.update(|n| *n += 1);
        let token = self.speech_poll.get_untracked();
        self.speech_asr
            .set(text(&self.settings.get_untracked(), "asr_model"));
        wasm_bindgen_futures::spawn_local(async move {
            match request("speech_prepare", json!({})).await {
                Ok(snapshot) => self.apply_speech(token, snapshot),
                Err(error) => {
                    self.apply_speech(token, speech_request_failure(&error));
                    return;
                }
            }
            while speech_polling(&self.speech.get_untracked()) {
                gloo_timers::future::TimeoutFuture::new(500).await;
                if self.speech_poll.get_untracked() != token {
                    return;
                }
                match request("speech_status", json!({})).await {
                    Ok(snapshot) => self.apply_speech(token, snapshot),
                    Err(error) => {
                        self.apply_speech(token, speech_request_failure(&error));
                        break;
                    }
                }
            }
            if self.speech_poll.get_untracked() == token {
                if let Ok(models) = request("model_status", json!({})).await {
                    self.artifacts.set(models);
                }
            }
        });
    }
    async fn refresh_devices(self) {
        match request("audio_devices", json!({})).await {
            Ok(value) => {
                self.devices.set(array(&value));
                self.devices_known.set(true);
                self.audio_error.set(String::new());
            }
            Err(error) => {
                self.devices_known.set(false);
                self.audio_error.set(audio_failure(&error));
            }
        }
    }
    /// First blocker for starting a voice conversation; text never depends on it.
    fn voice_blocker(self) -> Option<VoiceBlocker> {
        let known = self.devices_known.get();
        let devices = self.devices.get();
        voice_blocker(
            self.provider_state.get(),
            &self.speech.get(),
            known.then_some(devices.as_slice()),
            &text(&self.settings.get(), "input_device"),
            &self.persisted_input.get(),
            self.saving.get(),
        )
    }
    /// Plays an embedded native notice. A failure here is only displayed: it never
    /// requests another notice.
    fn play_notice(self, kind: &'static str) {
        self.notices.update(|gate| gate.in_flight = true);
        wasm_bindgen_futures::spawn_local(async move {
            let result = request("alert_notice", json!({"kind":kind})).await;
            self.notices.update(|gate| gate.in_flight = false);
            if let Err(error) = result {
                self.notice.set(notice_output_failure(&error));
            }
        });
    }
    /// A blocked voice attempt or help request: shows the blocker and, unless a notice is
    fn announce_blocker(self, blocker: VoiceBlocker) {
        self.voice_attempted.set(true);
        let mut allowed = false;
        self.notices.update(|gate| allowed = gate.allow_blocked());
        if let (Some(notice), true) = (blocker.notice(), allowed) {
            self.play_notice(notice);
        }
    }
    fn voice_start(self) {
        if let Some(blocker) = self.voice_blocker() {
            self.announce_blocker(blocker);
            return;
        }
        self.voice_attempted.set(false);
        self.input_feedback.set(String::new());
        self.task(async move { self.start(None).await });
    }
    /// Retries whatever is blocking voice, without playing a notice.
    fn retry_blocker(self, blocker: VoiceBlocker) {
        match blocker {
            VoiceBlocker::Provider => {
                if self.provider_state.get_untracked() != "checking" {
                    self.start_check();
                }
            }
            VoiceBlocker::Models => {
                if !speech_polling(&self.speech.get_untracked()) {
                    self.start_speech();
                }
            }
            VoiceBlocker::Input | VoiceBlocker::InvalidInput => {
                wasm_bindgen_futures::spawn_local(async move { self.refresh_devices().await });
            }
            VoiceBlocker::UnsavedInput | VoiceBlocker::SavingInput => {}
        }
    }
    /// Structured failures that deserve a spoken notice, at most once per failure per attempt.
    fn failure_notice(self, kind: &str, payload: &Value) {
        let Some(notice) = notice_for_event(kind, payload) else {
            return;
        };
        let mut allowed = false;
        self.notices
            .update(|gate| allowed = gate.allow_failure(notice));
        if allowed {
            self.play_notice(notice);
        }
    }
    async fn refresh_sessions(self) -> Result<(), String> {
        self.sessions
            .set(array(&request("session_list", json!({})).await?));
        Ok(())
    }
    async fn refresh_service(self) -> Result<(), String> {
        self.service
            .set(request("integration_status", json!({})).await?);
        Ok(())
    }
    async fn refresh_integrations(self) -> Result<(), String> {
        self.integrations
            .set(array(&request("integration_list", json!({})).await?));
        Ok(())
    }
    async fn refresh_models(self) -> Result<(), String> {
        match request("model_status", json!({})).await {
            Ok(value) => {
                self.artifacts.set(value);
                Ok(())
            }
            Err(error) => {
                self.artifacts.set(Value::Null);
                Err(error)
            }
        }
    }
    async fn save(self) -> Result<(), String> {
        let optional = |s: String| if s.is_empty() { Value::Null } else { json!(s) };
        let settings = self.settings.get_untracked();
        let submitted_input = text(&settings, "input_device");
        self.saving.set(true);
        let result=request("settings_save",json!({"settings":settings,"provider_key":optional(self.provider_key.get_untracked()),"brave_key":optional(self.brave_key.get_untracked()),"google_secret":optional(self.google_secret.get_untracked())})).await;
        self.saving.set(false);
        let result = result?;
        self.persisted_input.update(|persisted| {
            *persisted = persisted_after_save(persisted, &submitted_input, true)
        });
        self.provider_key.set(String::new());
        self.brave_key.set(String::new());
        self.google_secret.set(String::new());
        self.notice.set(
            if result["persistent_credentials"] == false {
                "Configurações salvas. Cofre indisponível: credenciais apenas nesta execução."
            } else {
                "Configurações salvas."
            }
            .into(),
        );
        self.start_check();
        if asr_changed(
            &self.speech_asr.get_untracked(),
            &text(&self.settings.get_untracked(), "asr_model"),
        ) {
            self.start_speech();
        }
        Ok(())
    }
    async fn new_session(self) -> Result<i64, String> {
        self.stop().await?;
        let session = request("session_create", json!({"title":"Conversa"})).await?;
        let id = session["id"].as_i64().ok_or("Sessão inválida")?;
        self.session.set(id);
        self.messages.set(Vec::new());
        self.sources.set(Vec::new());
        self.tools.set(Vec::new());
        self.operation.set(Value::Null);
        self.page.set("conversation");
        self.refresh_sessions().await?;
        Ok(id)
    }
    async fn stop(self) -> Result<(), String> {
        self.epoch.update(|n| *n += 1);
        let epoch = self.epoch.get_untracked();
        let session = self.session.get_untracked();
        self.dispose_active();
        request("conversation_cancel", json!({})).await?;
        if !epoch_current(self.epoch.get_untracked(), epoch) {
            return Ok(());
        }
        self.running.set(false);
        self.state.set("idle".into());
        self.input_level.set(0.);
        orb_level(0.);
        if session > 0 {
            if let Ok(operation) = request("action_get", json!({"session_id":session})).await {
                if epoch_current(self.epoch.get_untracked(), epoch)
                    && self.session.get_untracked() == session
                {
                    self.operation.set(operation);
                }
            }
        }
        Ok(())
    }
    /// Takes the active conversation stream, if any, and releases its bridge channel.
    fn dispose_active(self) {
        self.input_level.set(0.);
        if let Some(stream) = self.active_stream.get_untracked() {
            self.active_stream.set(None);
            dispose_stream_js(stream);
        }
    }
    /// Forgets `stream` as active only if it still is the active one.
    fn release_stream(self, stream: u32) {
        if self.active_stream.get_untracked() == Some(stream) {
            self.active_stream.set(None);
        }
    }
    /// Delivers a bridge event only while its epoch, session and stream are still current.
    fn stream_event(self, epoch: u64, session: i64, stream: u32, raw: String) {
        if !stream_current(
            self.epoch.get_untracked(),
            epoch,
            self.session.get_untracked(),
            session,
            self.active_stream.get_untracked(),
            stream,
        ) {
            return;
        }
        let terminal = is_terminal(&raw);
        self.event(raw);
        if terminal {
            self.release_stream(stream);
        }
    }
    /// Resets the UI after a failed start, only if that start still owns it.
    fn fail_start(self, epoch: u64, stream: u32) -> bool {
        let owned = owns_start(
            self.epoch.get_untracked(),
            epoch,
            self.active_stream.get_untracked(),
            stream,
        );
        if owned {
            self.active_stream.set(None);
            self.running.set(false);
            self.state.set("idle".into());
            self.input_level.set(0.);
        }
        owned
    }
    async fn open_session(self, id: i64) -> Result<(), String> {
        self.stop().await?;
        self.session.set(id);
        self.page.set("conversation");
        self.messages.set(Vec::new());
        self.sources.set(Vec::new());
        self.load_messages(None).await?;
        self.operation
            .set(request("action_get", json!({"session_id":id})).await?);
        Ok(())
    }
    async fn load_messages(self, after: Option<i64>) -> Result<(), String> {
        let values = request(
            "session_messages",
            json!({"session_id":self.session.get_untracked(),"after":after}),
        )
        .await?;
        let values = array(&values);
        self.more_messages.set(values.len() == 30);
        if let Some(last) = values.last().and_then(|v| v["id"].as_i64()) {
            self.message_cursor.set(Some(last));
        }
        for message in values {
            if message["role"] == "tool" {
                if let Ok(value) = serde_json::from_str::<Value>(&text(&message, "text")) {
                    self.add_sources(array(&value["sources"]));
                }
                continue;
            }
            let id = message["id"].to_string();
            self.messages.update(|lines| {
                lines.push(ChatLine {
                    id,
                    role: text(&message, "role"),
                    text: text(&message, "text"),
                })
            });
        }
        Ok(())
    }
    fn add_sources(self, sources: Vec<Value>) {
        self.sources.update(|all| {
            for source in sources {
                if !all
                    .iter()
                    .any(|s| s["source_id"] == source["source_id"] && s["url"] == source["url"])
                {
                    all.push(source);
                }
            }
        });
    }
    fn event(self, raw: String) {
        let Ok(event) = serde_json::from_str::<Value>(&raw) else {
            return;
        };
        if event["kind"] == "model.progress" {
            let p = &event["payload"];
            self.download.set(format!(
                "{}: {} / {} MB",
                text(p, "artifact"),
                p["downloaded"].as_u64().unwrap_or(0) / 1_000_000,
                p["total"].as_u64().unwrap_or(0) / 1_000_000
            ));
            return;
        }
        let generation = event["generation"].as_u64().unwrap_or(0);
        if event["session_id"].as_i64() != Some(self.session.get_untracked()) {
            return;
        }
        let Some(accepted) = accept_generation(self.generation.get_untracked(), generation) else {
            return;
        };
        self.generation.set(accepted);
        if event["kind"] == "run.started" {
            self.running.set(true);
            self.tools.set(Vec::new());
        }
        let payload = &event["payload"];
        let turn = text(&event, "turn_id");
        match event["kind"].as_str().unwrap_or("") {
            "state.changed" => {
                let state = text(payload, "state");
                self.state.set(state.clone());
                if state != "listening" {
                    self.input_level.set(0.);
                }
            }
            "audio.level" => {
                orb_level(payload["rms"].as_f64().unwrap_or(0.));
                if let Some(rms) = meter_rms(
                    &text(payload, "source"),
                    &self.state.get_untracked(),
                    payload["rms"].as_f64(),
                ) {
                    self.input_level.set(rms);
                }
            }
            "input.feedback" => {
                self.input_feedback.set(
                    input_feedback_message(&text(payload, "reason"))
                        .unwrap_or_default()
                        .to_string(),
                );
            }
            "transcript.final" => {
                self.input_feedback.set(String::new());
                let id = format!("{turn}:user");
                self.messages.update(|messages| {
                    messages.push(ChatLine {
                        id,
                        role: "user".into(),
                        text: text(payload, "text"),
                    })
                });
            }
            "assistant.delta" => {
                let id = format!("{turn}:assistant");
                let delta = text(payload, "text");
                self.messages.update(|messages| {
                    if let Some(line) = messages.iter_mut().find(|m| m.id == id) {
                        line.text.push_str(&delta);
                    } else {
                        messages.push(ChatLine {
                            id,
                            role: "assistant".into(),
                            text: delta,
                        });
                    }
                });
            }
            "tool.started" => self.tools.update(|t| {
                t.push((
                    text(payload, "name"),
                    text(payload, "label"),
                    "running".into(),
                ))
            }),
            "tool.completed" => {
                let name = text(payload, "name");
                let status = if payload["ok"] == true { "ok" } else { "error" };
                self.tools.update(|t| {
                    if let Some(row) = t.iter_mut().rev().find(|r| r.0 == name && r.2 == "running")
                    {
                        row.2 = status.into();
                    }
                });
            }
            "action.changed" => self.operation.set(payload.clone()),
            "sources" => self.add_sources(array(payload)),
            kind @ ("error" | "audio.error") => {
                self.notice.set(audio_event_message(kind, payload));
                self.failure_notice(kind, payload);
            }
            "turn.cancelled" => {
                self.running.set(false);
                self.state.set("idle".into());
                self.input_level.set(0.);
                orb_level(0.);
            }
            "run.completed" => {
                self.running.set(false);
                self.input_level.set(0.);
                orb_level(0.);
                if self.state.get_untracked() != "error" {
                    self.state.set(
                        if self.operation.get_untracked()["status"] == "awaiting_confirmation" {
                            "awaiting_confirmation"
                        } else {
                            "idle"
                        }
                        .into(),
                    );
                }
            }
            _ => {}
        }
    }
    async fn start(self, message: Option<String>) -> Result<(), String> {
        self.notices.update(|gate| gate.new_attempt());
        self.input_level.set(0.);
        if message.is_some() && self.provider_state.get_untracked() != "ready" {
            return Err(
                "O modelo ainda não foi validado. Aguarde a verificação ou tente novamente.".into(),
            );
        }
        let id = if self.session.get_untracked() == 0 {
            self.new_session().await?
        } else {
            self.session.get_untracked()
        };
        self.dispose_active();
        self.epoch.update(|n| *n += 1);
        let epoch = self.epoch.get_untracked();
        self.generation.set(0);
        let stream = next_stream_id();
        self.active_stream.set(Some(stream));
        self.running.set(true);
        let callback = Closure::<dyn FnMut(String)>::new(move |event| {
            self.stream_event(epoch, id, stream, event);
        })
        .into_js_value();
        let result = stream_js(
            "conversation_start",
            &json!({"session_id":id,"text":message}).to_string(),
            &callback,
            stream,
        )
        .await;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                dispose_stream_js(stream);
                if self.fail_start(epoch, stream) {
                    return Err(js_error(error));
                }
                return Ok(());
            }
        };
        if !epoch_current(self.epoch.get_untracked(), epoch) {
            dispose_stream_js(stream);
            return Ok(());
        }
        if serde_json::from_str::<Value>(&result.as_string().unwrap_or_default()).is_err() {
            dispose_stream_js(stream);
            self.fail_start(epoch, stream);
            return Err("Resposta inválida".into());
        }
        self.input.set(String::new());
        Ok(())
    }
    async fn action(self, command: &str) -> Result<(), String> {
        let op = self.operation.get_untracked();
        if command == "action_confirm" {
            self.stop().await?;
            self.state.set("executing".into());
        }
        let mut args = json!({"id":op["id"]});
        if command != "action_reconcile" {
            args["session_id"] = json!(self.session.get_untracked());
        }
        match request(command, args).await {
            Ok(value) => {
                self.operation.set(value);
                self.state.set("idle".into());
                Ok(())
            }
            Err(error) => {
                self.state.set("error".into());
                self.operation.set(
                    request(
                        "action_get",
                        json!({"session_id":self.session.get_untracked()}),
                    )
                    .await?,
                );
                Err(error)
            }
        }
    }
}

const PROVIDER_CHANGED: &str =
    "A configuração do provedor mudou. Salve as configurações para validar o modelo novamente.";
const PROVIDER_GUIDANCE: &str = "Configure e inicie o provedor selecionado e escolha um modelo compatível com português, streaming e ferramentas.";

fn epoch_current(now: u64, captured: u64) -> bool {
    now == captured
}
/// A conversation stream callback may touch the UI only while all of these still hold.
fn stream_current(
    epoch_now: u64,
    epoch: u64,
    session_now: i64,
    session: i64,
    active: Option<u32>,
    stream: u32,
) -> bool {
    epoch_current(epoch_now, epoch) && session_now == session && active == Some(stream)
}
/// A start may reset the UI after failing only if it is still the latest start.
fn owns_start(epoch_now: u64, epoch: u64, active: Option<u32>, stream: u32) -> bool {
    epoch_current(epoch_now, epoch) && active == Some(stream)
}
fn is_terminal(raw: &str) -> bool {
    serde_json::from_str::<Value>(raw)
        .map(|v| matches!(v["kind"].as_str(), Some("run.completed" | "turn.cancelled")))
        .unwrap_or(false)
}
/// The first event of a run (generation still 0) establishes the generation; later ones must match.
fn accept_generation(current: u64, event: u64) -> Option<u64> {
    if event == 0 || (current != 0 && current != event) {
        None
    } else {
        Some(event)
    }
}

#[derive(Debug, PartialEq)]
enum Readiness {
    Ready,
    Unavailable(String),
}

/// `None` when the response belongs to a superseded check and must be ignored.
fn check_outcome(
    current: u64,
    generation: u64,
    result: Result<Value, String>,
) -> Option<Readiness> {
    if current != generation {
        return None;
    }
    Some(match result {
        Ok(probe)
            if ["tools", "streaming", "portuguese", "cancellation"]
                .iter()
                .all(|flag| probe[*flag] == true) =>
        {
            Readiness::Ready
        }
        Ok(_) => Readiness::Unavailable(format!(
            "A verificação do modelo não confirmou todos os recursos necessários. {PROVIDER_GUIDANCE}"
        )),
        Err(error) => Readiness::Unavailable(format!("{error} {PROVIDER_GUIDANCE}")),
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum VoiceBlocker {
    Provider,
    Models,
    UnsavedInput,
    SavingInput,
    InvalidInput,
    Input,
}
impl VoiceBlocker {
    fn notice(self) -> Option<&'static str> {
        match self {
            Self::Provider => Some("provider"),
            Self::Models => Some("models"),
            Self::Input | Self::InvalidInput => Some("capture"),
            Self::UnsavedInput | Self::SavingInput => None,
        }
    }
    fn message(self, speech: &Value) -> String {
        match self {
            Self::Provider => "A voz precisa do provedor configurado e validado. Configure o provedor em Configurações e tente novamente. A conversa por texto não depende do áudio.".into(),
            Self::Models => {
                let hint = if speech["phase"] == "failed" || speech["phase"] == "unsupported" {
                    format!(
                        " {} {}",
                        speech["error"].as_str().unwrap_or(""),
                        speech_error_hint(speech["code"].as_str().unwrap_or(""))
                    )
                } else {
                    " Aguarde a preparação ou use Tentar novamente.".into()
                };
                format!("Os modelos de fala (reconhecimento, detecção de voz e síntese) ainda não estão prontos.{hint}")
            }
            Self::UnsavedInput => "A seleção de microfone tem alterações que ainda não foram salvas. Salve as configurações para iniciar a conversa por voz. A conversa por texto continua disponível.".into(),
            Self::SavingInput => "A seleção de microfone está sendo salva. Aguarde a confirmação para iniciar a conversa por voz. A conversa por texto continua disponível.".into(),
            Self::InvalidInput => "O microfone salvo não está disponível ou usa uma identificação anterior. Selecione novamente o dispositivo de entrada em Configurações e salve. A conversa por texto continua disponível.".into(),
            Self::Input => "Nenhum microfone disponível. Conecte um dispositivo de entrada ou escolha outro em Configurações e tente novamente.".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputSelection {
    Saved,
    Saving,
    Unsaved,
}

fn input_selection(draft: &str, persisted: &str, saving: bool) -> InputSelection {
    if saving {
        InputSelection::Saving
    } else if draft != persisted {
        InputSelection::Unsaved
    } else {
        InputSelection::Saved
    }
}

fn input_selection_message(selection: InputSelection) -> &'static str {
    match selection {
        InputSelection::Saved => "",
        InputSelection::Saving => "Salvando a seleção de microfone…",
        InputSelection::Unsaved => {
            "Seleção de microfone não salva. Salve as configurações para usar a voz."
        }
    }
}

fn persisted_after_save(previous: &str, submitted: &str, succeeded: bool) -> String {
    if succeeded {
        submitted.to_string()
    } else {
        previous.to_string()
    }
}

fn voice_blocker(
    provider_state: &str,
    speech: &Value,
    devices: Option<&[Value]>,
    draft_input: &str,
    persisted_input: &str,
    saving: bool,
) -> Option<VoiceBlocker> {
    if provider_state != "ready" {
        return Some(VoiceBlocker::Provider);
    }
    if speech["ready"] != true {
        return Some(VoiceBlocker::Models);
    }
    match input_selection(draft_input, persisted_input, saving) {
        InputSelection::Saving => return Some(VoiceBlocker::SavingInput),
        InputSelection::Unsaved => return Some(VoiceBlocker::UnsavedInput),
        InputSelection::Saved => {}
    }
    let inputs = devices?
        .iter()
        .filter(|device| device["direction"] == "input")
        .collect::<Vec<_>>();
    if inputs.is_empty() {
        return Some(VoiceBlocker::Input);
    }
    if !persisted_input.is_empty() && !inputs.iter().any(|device| device["id"] == persisted_input) {
        return Some(VoiceBlocker::InvalidInput);
    }
    None
}

/// Throttle for spoken notices: never two at once, a failure at most once per attempt.
#[derive(Debug, Default, Clone, PartialEq)]
struct NoticeGate {
    in_flight: bool,
    announced: Vec<&'static str>,
}
impl NoticeGate {
    /// A new run starts a new attempt: its failures may be announced again.
    fn new_attempt(&mut self) {
        self.announced.clear();
    }
    fn allow_blocked(&mut self) -> bool {
        !self.in_flight
    }
    fn allow_failure(&mut self, kind: &'static str) -> bool {
        if self.in_flight || self.announced.contains(&kind) {
            return false;
        }
        self.announced.push(kind);
        true
    }
}

fn notice_for_event(kind: &str, payload: &Value) -> Option<&'static str> {
    match kind {
        "audio.error" if payload["stage"] == "synthesis" && payload["partial"] != true => {
            Some("synthesis")
        }
        "error"
            if [
                "DEVICE_UNAVAILABLE",
                "MIC_OPEN_FAILED",
                "AUDIO_FORMAT",
                "AUDIO_OVERRUN",
            ]
            .contains(&payload["code"].as_str().unwrap_or("")) =>
        {
            Some("capture")
        }
        _ => None,
    }
}

fn audio_event_message(kind: &str, payload: &Value) -> String {
    let message = text(payload, "message");
    if kind == "audio.error" && payload["stage"] == "synthesis" && payload["partial"] == true {
        "Parte da resposta não pôde ser falada. A resposta completa continua visível por escrito."
            .into()
    } else if kind == "audio.error" && payload["stage"] == "playback" {
        format!(
            "{message} Verifique o dispositivo de saída em Configurações. A resposta continua visível por escrito."
        )
    } else {
        message
    }
}

fn notice_output_failure(error: &str) -> String {
    format!(
        "Não foi possível tocar o aviso sonoro: {error} Verifique o dispositivo de saída em Configurações; as orientações continuam visíveis na tela."
    )
}

fn audio_failure(error: &str) -> String {
    format!(
        "Não foi possível listar os dispositivos de áudio: {error} A conversa por texto continua disponível."
    )
}

fn saved_input_label(devices: &[Value], persisted: &str) -> String {
    if persisted.is_empty() {
        return "Padrão do sistema".into();
    }
    devices
        .iter()
        .find(|device| device["direction"] == "input" && text(device, "id") == persisted)
        .map(|device| text(device, "name"))
        .unwrap_or_else(|| "indisponível — selecione novamente".into())
}

fn input_feedback_message(reason: &str) -> Option<&'static str> {
    match reason {
        "empty_speech" => Some("Nenhuma fala detectada. Tente falar novamente."),
        "empty_transcript" => {
            Some("Não foi possível reconhecer palavras no áudio. Tente novamente.")
        }
        _ => None,
    }
}

fn meter_rms(source: &str, state: &str, value: Option<f64>) -> Option<f64> {
    if source != "input" || state != "listening" {
        return None;
    }
    Some(
        value
            .filter(|rms| rms.is_finite())
            .unwrap_or(0.)
            .clamp(0., 1.),
    )
}

/// Polling continues while a background preparation job is running.
fn speech_polling(snapshot: &Value) -> bool {
    snapshot["active"] == true
}

/// A retry is meaningful when speech is neither ready nor being prepared.
fn speech_can_retry(snapshot: &Value) -> bool {
    snapshot["ready"] != true
        && snapshot["active"] != true
        && ["failed", "unsupported"].contains(&snapshot["phase"].as_str().unwrap_or(""))
}

fn asr_changed(previous: &str, current: &str) -> bool {
    !previous.is_empty() && !current.is_empty() && previous != current
}

fn speech_request_failure(error: &str) -> Value {
    json!({"phase":"failed","ready":false,"active":false,"code":"SPEECH_REQUEST",
        "error":format!("Não foi possível consultar a preparação da fala: {error}")})
}

fn artifact_label(id: &str) -> &str {
    match id {
        "whisper-turbo" => "Whisper Turbo",
        "whisper-small" => "Whisper Small",
        "silero-vad" => "Silero VAD",
        "kokoro-multi-lang-v1_0" => "Kokoro",
        other => other,
    }
}

fn speech_error_hint(code: &str) -> &'static str {
    match code {
        "MODEL_NETWORK" => "Verifique a conexão com a internet e tente novamente.",
        "MODEL_INTEGRITY" => {
            "O arquivo baixado não passou na verificação de integridade. Tente novamente."
        }
        "MODEL_IO" => "Verifique o espaço livre em disco e as permissões da pasta de dados.",
        "MODEL_INVALID" | "MODEL_NOT_INSTALLED" => {
            "Os arquivos do modelo estão incompletos ou inválidos. Tente novamente para reinstalar."
        }
        "AUDIO_ERROR" => "Os modelos não puderam ser carregados. Tente novamente.",
        "SPEECH_UNSUPPORTED" => "Esta versão do aplicativo não inclui o áudio local.",
        _ => "Tente novamente.",
    }
}

fn speech_progress(snapshot: &Value) -> String {
    let artifact = artifact_label(snapshot["artifact"].as_str().unwrap_or("")).to_string();
    let megabytes = |key: &str| snapshot[key].as_u64().unwrap_or(0) / 1_000_000;
    let size = if snapshot["total"].as_u64().unwrap_or(0) > 0 {
        format!(" · {} / {} MB", megabytes("downloaded"), megabytes("total"))
    } else {
        String::new()
    };
    let file = snapshot["file"]
        .as_str()
        .map(|file| format!(" ({file})"))
        .unwrap_or_default();
    match snapshot["phase"].as_str().unwrap_or("") {
        "verifying" => format!("Verificando {artifact}{file}{size}"),
        "copying" => format!("Copiando {artifact} do disco local{file}{size}"),
        "downloading" => format!("Baixando {artifact}{file}{size}"),
        _ => format!("Preparando {artifact}"),
    }
}

/// Portuguese description of the speech preparation state; `None` when nothing
/// has started. Never mentions or affects text conversation.
fn speech_message(snapshot: &Value) -> Option<String> {
    let phase = snapshot["phase"].as_str().unwrap_or("idle");
    let warning = snapshot["warning"].as_str().map(|w| format!(" {w}"));
    match phase {
        "idle" => None,
        "starting" => Some("Preparando a fala local em segundo plano…".into()),
        "loading" => Some("Carregando os modelos de fala na memória…".into()),
        "failed" | "unsupported" => {
            let error = snapshot["error"].as_str().unwrap_or("Falha desconhecida.");
            let hint = speech_error_hint(snapshot["code"].as_str().unwrap_or(""));
            Some(format!(
                "Fala local indisponível: {error} {hint} A conversa por texto continua disponível."
            ))
        }
        _ if snapshot["ready"] == true && snapshot["active"] == true => Some(format!(
            "Fala local pronta. Preparando o modelo opcional em segundo plano: {}.{}",
            speech_progress(snapshot),
            warning.unwrap_or_default()
        )),
        "ready" => Some(format!("Fala local pronta.{}", warning.unwrap_or_default())),
        _ => Some(format!("{}…", speech_progress(snapshot))),
    }
}

/// Shown in the conversation area only when the user has something to know or do.
fn speech_notice_visible(snapshot: &Value) -> bool {
    speech_message(snapshot).is_some()
        && (snapshot["ready"] != true
            || snapshot["active"] == true
            || snapshot["warning"].is_string())
}

fn state_copy(state: &str) -> (&'static str, &'static str) {
    match state {
        "preparing" => ("Preparando áudio", "Um instante para abrir o microfone."),
        "listening" => ("Ouvindo", "Pode falar. Estou ouvindo."),
        "transcribing" => ("Transcrevendo", "Transformando sua fala em palavras."),
        "thinking" => ("Pensando", "Um instante para entender."),
        "knowledge" => ("Documentação", "Buscando uma orientação oficial."),
        "memory" => ("Memória", "Lembrando o que já conversamos."),
        "searching" => ("Pesquisando", "Buscando mais informações."),
        "consulting" => ("Consultando", "Verificando suas informações."),
        "awaiting_confirmation" => ("Aguardando confirmação", "Confira antes de confirmar."),
        "executing" => ("Executando", "Realizando a ação confirmada."),
        "speaking" => ("Respondendo", "Uma resposta para ouvir."),
        "error" => ("Algo precisa de atenção", "Vamos tentar de novo?"),
        _ => ("Pronta para começar", "Como posso ajudar?"),
    }
}

#[component]
pub fn App() -> impl IntoView {
    let ui = Ui::new();
    Effect::new(move || {
        if !desktop() {
            ui.notice.set("Esta é a interface Leptos. Abra o aplicativo Tauri para conectar modelos, áudio e integrações.".into());
            return;
        }
        ui.task(async move {
            let settings = request("settings_get", json!({})).await?;
            ui.persisted_input.set(text(&settings, "input_device"));
            ui.settings.set(settings);
            ui.start_speech();
            ui.start_check();
            ui.refresh_sessions().await?;
            ui.refresh_service().await?;
            ui.refresh_integrations().await?;
            ui.refresh_devices().await;
            if let Err(error) = ui.refresh_models().await {
                ui.notice.set(error);
            }
            Ok(())
        });
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                gloo_timers::future::TimeoutFuture::new(2000).await;
                if ui.service.get_untracked()["google_calendar"]["connecting"] == true {
                    let _ = ui.refresh_service().await;
                }
                if ["executing", "unknown"].contains(
                    &ui.operation.get_untracked()["status"]
                        .as_str()
                        .unwrap_or(""),
                ) {
                    if let Ok(operation) = request(
                        "action_get",
                        json!({"session_id":ui.session.get_untracked()}),
                    )
                    .await
                    {
                        ui.operation.set(operation);
                    }
                }
            }
        });
    });
    view! {
        <div class="app" data-state=move||ui.state.get()>
            <aside class="sidebar"><div class="brand"><span class="brand-icon"></span><span class="brand-text">"aura"</span></div><div class="eyebrow">"Seu espaço de conversa"</div>
                <nav class="nav" aria-label="Navegação principal"><Nav ui page="conversation" symbol="◯" label="Conversa"/><Nav ui page="memory" symbol="◷" label="Memória"/><Nav ui page="integrations" symbol="◇" label="Integrações"/><Nav ui page="settings" symbol="⚙" label="Configurações"/></nav>
                <div class="sidebar-bottom"><p><span class="local-dot"></span>"Áudio local · sua escolha de modelo"</p><div class="sidebar-note">"Uma conversa de cada vez."<br/>"Com calma. Com contexto."</div></div>
            </aside>
            <main class="workspace"><header class="topbar"><div class="breadcrumb">"Aura"<span>"/ Assistente"</span></div><div class="header-status"><span class="pill">{move||if ui.running.get(){state_copy(&ui.state.get()).0}else{match ui.provider_state.get(){"checking"=>"Verificando modelo…","ready"=>"Compatibilidade validada",_=>"Modelo indisponível"}}}</span><Show when=move||ui.provider_state.get()=="unavailable"&&!ui.running.get()><button class="text-button" on:click=move |_|ui.start_check()>"Tentar novamente"</button></Show><Show when=move||ui.running.get()||ui.notices.get().in_flight><button class="text-button" on:click=move |_|ui.task(async move{ui.stop().await})>"Parar"</button></Show></div></header>
                <div class="content"><Show when=move||!ui.notice.get().is_empty()><div class="notice" role="status">{move||ui.notice.get()}</div></Show>
                    <Show when=move||ui.provider_state.get()=="unavailable"&&!ui.provider_error.get().is_empty()><div class="notice" role="alert">{move||ui.provider_error.get()}<button class="text-button" on:click=move |_|ui.start_check()>"Tentar novamente"</button></div></Show>
                    <Show when=move||speech_notice_visible(&ui.speech.get())><div class="notice" role="status">{move||speech_message(&ui.speech.get()).unwrap_or_default()}<Show when=move||speech_can_retry(&ui.speech.get())><button class="text-button" on:click=move |_|ui.start_speech()>"Tentar novamente"</button></Show></div></Show>
                    <section class="page" hidden=move||ui.page.get()!="conversation"><Conversation ui/></section>
                    <section class="page" hidden=move||ui.page.get()!="memory"><MemoryPage ui/></section>
                    <section class="page" hidden=move||ui.page.get()!="integrations"><IntegrationsPage ui/></section>
                    <section class="page" hidden=move||ui.page.get()!="settings"><SettingsPage ui/></section>
                    <footer class="footer"><span>"Whisper + Kokoro · processamento local de áudio"</span><span>"Ações externas só acontecem após sua confirmação"</span></footer>
                </div>
            </main>
        </div>
    }
}

#[component]
fn Nav(ui: Ui, page: &'static str, symbol: &'static str, label: &'static str) -> impl IntoView {
    view! {<button class:active=move||ui.page.get()==page aria-current=move||if ui.page.get()==page{"page"}else{"false"} on:click=move |_|ui.page.set(page)><span class="nav-symbol">{symbol}</span><span class="nav-label">{label}</span></button>}
}

#[component]
fn Conversation(ui: Ui) -> impl IntoView {
    view! {
        <div class="page-heading"><div><div class="eyebrow">"Aqui para ouvir"</div><h1>"Vamos conversar."</h1><div class="subtitle">"Sua voz, suas ações, suas respostas."</div></div><button class="text-button" disabled=move||ui.busy.get() on:click=move |_|ui.task(async move{ui.new_session().await?;Ok(())})>"+ Nova conversa"</button></div>
        <div class="voice-stage"><div class="orb-scene" aria-hidden="true"><div class="orbit"></div><div class="orbit"></div><div class="orb"></div></div>
            <div class="state-label">{move||state_copy(&ui.state.get()).0}</div><h2 class="stage-title">{move||state_copy(&ui.state.get()).1}</h2>
            <p class="stage-description" role="status" aria-live="polite">{move||if ui.running.get(){"Interrompa a resposta pelo botão. Ao terminar, volto a ouvir."}else{"Peça o que precisar: consultar a agenda, pesquisar, lembrar conversas anteriores e mais."}}</p>
            <div class="stage-controls"><button class="primary" disabled=move||ui.busy.get()||ui.running.get() on:click=move |_|ui.voice_start()>"● Começar conversa"</button>
                <button class="secondary" hidden=move||ui.state.get()!="listening" on:click=move |_|ui.task(async move{request("conversation_finish_input",json!({})).await?;Ok(())})>"Concluir fala"</button>
                <button class="round" aria-label="Interromper áudio e conversa" on:click=move |_|ui.task(async move{ui.stop().await})>"■"</button></div>
            <Show when=move||ui.voice_attempted.get()&&ui.voice_blocker().is_some()><div class="notice" role="alert">{move||ui.voice_blocker().map(|blocker|blocker.message(&ui.speech.get())).unwrap_or_default()}
                <button class="text-button" on:click=move |_|if let Some(blocker)=ui.voice_blocker(){ui.retry_blocker(blocker)}>"Tentar novamente"</button>
                <button class="text-button" on:click=move |_|if let Some(blocker)=ui.voice_blocker(){ui.announce_blocker(blocker)}>"Ouvir orientação"</button></div></Show>
            <p class="fine-print">"O microfone é aberto apenas ao começar. O áudio bruto não é salvo."</p>
            <div class="input-meter"><label for="input-level">"Nível do microfone"</label>
                <meter id="input-level" min="0" max="1" tabindex="0" prop:value=move||ui.input_level.get()
                    aria-valuetext=move||format!("{:.0}% do nível máximo",ui.input_level.get()*100.)></meter>
                <span class="input-meter-value" aria-hidden="true">{move||format!("{:.0}%",ui.input_level.get()*100.)}</span></div>
            <Show when=move||!ui.input_feedback.get().is_empty()><p class="input-feedback" role="status" aria-live="polite">{move||ui.input_feedback.get()}</p></Show>
        </div>
        <div class="configuration-row"><button class="config-card" on:click=move |_|ui.page.set("settings")><span class="config-icon">"✧"</span><span class="config-copy"><small>"Modelo de suporte"</small><strong>{move||text(&ui.settings.get()["provider"],"model")}</strong></span><span>"↗"</span></button>
            <div class="config-card"><span class="config-icon">"≋"</span><div class="config-copy"><label for="active-voice">"Voz da resposta"</label><select id="active-voice" disabled=move||ui.running.get()||!settings_loaded(ui) on:change=move|ev|{ui.set("voice",json!(event_target_value(&ev)));ui.task(async move{ui.save().await});}>{move||choice_options(ui,"voice")}</select></div></div></div>
        <ActionCard ui/>
        <section class="conversation-preview"><div class="section-header"><h2>"A conversa, por escrito"</h2><span class="fine-print">{move||if ui.session.get()==0{"Nova conversa".into()}else{format!("Sessão {}",ui.session.get())}}</span></div>
            <Show when=move||ui.messages.get().is_empty()><p class="empty">"Experimente: “O que tenho amanhã na agenda?”, “Pesquise as novidades do Rust 2024” ou “Do que falamos ontem?”"</p></Show>
            <div class="transcript" aria-live="off">{move||ui.messages.get().into_iter().map(|message|view!{<div class="transcript-line"><span class="avatar">{if message.role=="user"{"V"}else{"a"}}</span><p><strong>{if message.role=="user"{"Você"}else{"Aura"}}</strong>{message.text}</p></div>}).collect_view()}</div>
            <Show when=move||!ui.tools.get().is_empty()><ul class="tool-activity" aria-live="polite">{move||ui.tools.get().into_iter().map(|(_,label,status)|view!{<li>{match status.as_str(){"ok"=>format!("✓ {label}"),"error"=>format!("✗ {label}"),_=>format!("{label}…")}}</li>}).collect_view()}</ul></Show>
            <Show when=move||ui.more_messages.get()><button class="text-button" disabled=move||ui.running.get()||ui.busy.get() on:click=move |_|ui.task(async move{ui.load_messages(ui.message_cursor.get_untracked()).await})>"Carregar mais mensagens"</button></Show>
            <form class="message-form" on:submit=move|ev|{ev.prevent_default();if ui.provider_state.get_untracked()!="ready"{return;}let text=ui.input.get_untracked();ui.task(async move{ui.start(Some(text)).await});}><label class="sr-only" for="message">"Escreva sua mensagem"</label><input id="message" placeholder="Ou escreva uma mensagem…" maxlength="12000" prop:value=move||ui.input.get() on:input=move|ev|ui.input.set(event_target_value(&ev))/><button class="primary" disabled=move||ui.busy.get()||ui.provider_state.get()!="ready"||ui.input.get().trim().is_empty()>"Enviar"</button></form>
            <Sources ui/>
        </section>
    }
}

#[component]
fn ActionCard(ui: Ui) -> impl IntoView {
    view! {<Show when=move||!ui.operation.get().is_null()>{move||{let op=ui.operation.get();let status=text(&op,"status");let pending=status=="awaiting_confirmation";let conflicts=op["result"]["busy"].as_array().is_some_and(|a|!a.is_empty());let reconcile=status=="unknown"||status=="executing";
        view!{<section class="panel action-card" aria-label="Ação pendente"><div class="eyebrow">{if pending{"Confira antes de confirmar"}else{"Resultado da ação"}}</div><h2>{text(&op,"summary")}</h2><dl>{array(&op["fields"]).into_iter().map(|f|view!{<dt>{text(&f,"label")}</dt><dd>{text(&f,"value")}</dd>}).collect_view()}</dl>
            <p class="subtitle">{match status.as_str(){"awaiting_confirmation"=>"A ação ainda não foi executada. Confirme por voz ou pelo botão.","completed"=>"Ação concluída e confirmada.","unknown"=>"O resultado ainda não foi confirmado. Verifique antes de tentar novamente.","executing"=>"A ação está em andamento.","cancelled"=>"Proposta cancelada. Nada foi executado.",_=>"Não foi possível concluir esta ação."}}</p>
            <Show when=move||pending><Show when=move||conflicts><p class="conflict-note">"Há compromissos nesse intervalo. Confirmar prosseguirá mesmo com sobreposição."</p></Show><div class="page-actions"><button class="primary" disabled=move||ui.busy.get() on:click=move |_|ui.task(async move{ui.action("action_confirm").await})>"Confirmar"</button><button class="secondary" disabled=move||ui.busy.get() on:click=move |_|ui.task(async move{ui.action("action_cancel").await})>"Cancelar"</button></div></Show>
            <Show when=move||reconcile><div class="page-actions"><button class="secondary" disabled=move||ui.busy.get() on:click=move |_|ui.task(async move{ui.action("action_reconcile").await})>"Verificar estado"</button><button class="secondary" disabled=move||ui.busy.get() on:click=move |_|ui.task(async move{ui.action("action_retry").await})>"Preparar nova tentativa"</button></div></Show>
        </section>}
    }}</Show>}
}

#[component]
fn Sources(ui: Ui) -> impl IntoView {
    view! {<Show when=move||!ui.sources.get().is_empty()><details class="sources"><summary>"Fontes e conversas consultadas"</summary>{move||ui.sources.get().into_iter().map(|source|{
        let url=text(&source,"url");let safe=url.starts_with("https://")||url.starts_with("http://");let title=source["title"].as_str().unwrap_or("Conversa anterior").to_string();
        let heading=if safe{{let destination=url.clone();view!{<a href=url target="_blank" rel="noopener noreferrer" on:click=move|ev|{ev.prevent_default();let destination=destination.clone();ui.task(async move{request("source_open",json!({"url":destination})).await?;Ok(())});}>{title}</a>}.into_any()}}else{view!{<strong>{title}</strong>}.into_any()};
        view!{<div class="result">{heading}<small>{text(&source,"text")}</small></div>}
    }).collect_view()}</details></Show>}
}

#[component]
fn MemoryPage(ui: Ui) -> impl IntoView {
    view! {<div class="page-heading"><div><div class="eyebrow">"Contexto que acompanha"</div><h1>"Sua memória."</h1><p class="subtitle">"Busque e retome conversas armazenadas neste computador."</p></div></div>
    <div class="panel"><form on:submit=move|ev|{ev.prevent_default();ui.task(async move{ui.memory_results.set(array(&request("memory_search",json!({"query":ui.memory_query.get_untracked()})).await?));Ok(())});}><label class="field">"O que você quer lembrar?"<input type="search" placeholder="Ex.: reunião, projeto, decisão" prop:value=move||ui.memory_query.get() on:input=move|ev|ui.memory_query.set(event_target_value(&ev))/></label><button class="primary" disabled=move||ui.busy.get()>"Buscar na memória"</button></form>
        {move||ui.memory_results.get().into_iter().map(|result|{let session=result["session_id"].as_i64().unwrap_or(0);view!{<div class="result"><p>{text(&result,"text")}</p><small>{text(&result,"created_at")}</small><button class="text-button" on:click=move |_|ui.task(async move{ui.open_session(session).await})>"Abrir conversa"</button></div>}}).collect_view()}</div>
    <div class="panel"><div class="section-header"><h2>"Conversas"</h2><button class="text-button" on:click=move |_|ui.task(async move{ui.refresh_sessions().await})>"Atualizar"</button></div>
        <Show when=move||ui.sessions.get().is_empty()><p class="empty">"Sua primeira conversa aparecerá aqui."</p></Show>
        {move||ui.sessions.get().into_iter().map(|session|{let id=session["id"].as_i64().unwrap_or(0);view!{<div class="result"><strong>{text(&session,"title")}</strong><small>{text(&session,"created_at")}</small><div class="page-actions"><button class="text-button" on:click=move |_|ui.task(async move{ui.open_session(id).await})>"Abrir e retomar"</button><button class="text-button" on:click=move |_|ui.task(async move{request("session_delete",json!({"session_id":id})).await?;if ui.session.get_untracked()==id{ui.session.set(0);ui.messages.set(Vec::new());ui.operation.set(Value::Null);}ui.refresh_sessions().await})>"Excluir"</button></div></div>}}).collect_view()}
        <button class="text-button" on:click=move |_|ui.task(async move{let before=ui.sessions.get_untracked().last().and_then(|s|s["id"].as_i64());let next=array(&request("session_list",json!({"before":before})).await?);ui.sessions.update(|all|all.extend(next));Ok(())})>"Carregar conversas anteriores"</button>
    </div>}
}

#[component]
fn IntegrationsPage(ui: Ui) -> impl IntoView {
    view! {
        <div class="page-heading"><div><div class="eyebrow">"Assistência que acompanha você"</div><h1>"Suas integrações."</h1><p class="subtitle">"Escolha o que o assistente pode usar. Ações externas sempre pedem sua confirmação."</p></div></div>
        <div class="panel"><div class="section-header"><h2>"Disponíveis"</h2><button class="text-button" on:click=move |_|ui.task(async move{ui.refresh_integrations().await})>"Atualizar"</button></div>
        {move||ui.integrations.get().into_iter().map(|item|{
            let id=text(&item,"id");let enabled=item["enabled"]==true;
            let badge=if item["builtin"]==true{"Embutida"}else if item["connected"]==true{"Conectada"}else{"Desconectada"};
            let tools=array(&item["tools"]).iter().map(|t|text(t,"label")).collect::<Vec<_>>().join(" · ");
            view!{<div class="result"><div class="section-header"><strong>{text(&item,"name")}</strong><span class="pill">{badge}</span></div><p>{text(&item,"description")}</p><small>{tools}</small>
              <label class="check-field"><input type="checkbox" prop:checked=enabled disabled=move||ui.busy.get() on:change=move|ev|{let on=event_target_checked(&ev);let id=id.clone();ui.task(async move{let r=request("integration_set_enabled",json!({"integration":id,"enabled":on})).await?;ui.set("integrations_disabled",r["integrations_disabled"].clone());ui.refresh_integrations().await})}/>"Permitir que o assistente use"</label></div>}
        }).collect_view()}</div>
        <div class="panel"><div class="section-header"><h2>"Google Calendar"</h2><span class="pill">{move||if ui.service.get()["google_calendar"]["connection"].is_object(){"Conectado"}else{"Não conectado"}}</span></div>
            <p class="subtitle">"Consulte eventos e disponibilidade. Toda criação de compromisso pede sua confirmação."</p>
            <Show when=move||ui.service.get()["google_calendar"]["connection"].is_object()><p>{move||text(&ui.service.get()["google_calendar"]["connection"],"label")}</p></Show>
            <Show when=move||ui.service.get()["google_calendar"]["connecting"]==true><p class="subtitle">"Conclua a autorização no navegador. A conexão será atualizada automaticamente."</p></Show>
            <Show when=move||ui.service.get()["google_calendar"]["error"].is_object()><p class="conflict-note">{move||text(&ui.service.get()["google_calendar"]["error"],"message")}</p></Show>
            <div class="page-actions"><button class="primary" disabled=move||ui.busy.get()||ui.service.get()["google_calendar"]["connection"].is_object()||ui.service.get()["google_calendar"]["connecting"]==true on:click=move |_|ui.task(async move{
                ui.save().await?;let value=request("integration_connect",json!({"integration":"google_calendar"})).await?;ui.refresh_integrations().await?;ui.refresh_service().await?;
                ui.notice.set(if value["browser_opened"]==true{"Autorize a conexão no navegador do sistema.".into()}else{format!("Abra no navegador: {}",text(&value,"authorization_url"))});Ok(())
            })>"Conectar conta Google"</button><button class="secondary" disabled=move||ui.busy.get() on:click=move |_|ui.task(async move{request("integration_disconnect",json!({"integration":"google_calendar"})).await?;ui.calendars.set(Vec::new());ui.refresh_integrations().await?;ui.refresh_service().await})>"Desconectar"</button></div>
            <button class="text-button" on:click=move |_|ui.page.set("settings")>"Configurar credenciais do aplicativo"</button>
        </div>
        <div class="panel"><h2>"Sua agenda"</h2><button class="secondary" disabled=move||ui.busy.get() on:click=move |_|ui.task(async move{let value=request("calendar_list",json!({})).await?;ui.calendars.set(array(&value["items"]));Ok(())})>"Carregar agendas"</button>
            <label class="field">"Agenda padrão"<select prop:value=move||text(&ui.settings.get(),"calendar_id") on:change=move|ev|{ui.set("calendar_id",json!(event_target_value(&ev)));ui.task(async move{ui.save().await});}><option value="primary">"Agenda principal"</option>{move||ui.calendars.get().into_iter().map(|calendar|view!{<option value=text(&calendar,"id")>{format!("{} · {}",text(&calendar,"summary"),if ["owner","writer"].contains(&calendar["accessRole"].as_str().unwrap_or("")){"permite criação"}else{"somente leitura"})}</option>}).collect_view()}</select></label>
            <button class="text-button" disabled=move||ui.busy.get()||ui.provider_state.get()!="ready" on:click=move |_|ui.task(async move{ui.page.set("conversation");ui.start(Some("Quais compromissos tenho amanhã?".into())).await})>"Consultar compromissos de amanhã →"</button>
        </div>
        <div class="panel"><h2>"Documentação do Google Calendar"</h2><p class="subtitle">"A documentação oficial orienta as respostas. A pesquisa na web pode complementar, sempre identificando as fontes."</p>
            <p class="subtitle">{move||format!("{} documentos locais",ui.service.get()["documents"].as_i64().unwrap_or(0))}</p>
            <button class="secondary" disabled=move||ui.busy.get() on:click=move |_|ui.task(async move{let report=request("knowledge_refresh",json!({"integration":"google_calendar"})).await?;ui.refresh_service().await?;ui.notice.set(format!("{} documentos atualizados; {} fontes indisponíveis. Versões anteriores foram preservadas.",report["updated"],array(&report["failures"]).len()));Ok(())})>"Atualizar documentação oficial"</button>
        </div>
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Choice {
    value: String,
    label: String,
    installed: bool,
}

fn catalog_entries(status: &Value) -> Vec<Value> {
    status["artifacts"].as_array().cloned().unwrap_or_default()
}

/// Choices from catalog entries having a nonempty string under `key`
/// (`voice` or `asr_model`). A current value absent from the catalog is
/// prepended as a temporary option; settings are never modified.
fn catalog_choices(status: &Value, key: &str, current: &str) -> Vec<Choice> {
    let mut choices: Vec<Choice> = catalog_entries(status)
        .iter()
        .filter_map(|entry| {
            let value = entry[key].as_str().filter(|v| !v.is_empty())?;
            let installed = entry["installed"] == true;
            let label = entry["label"]
                .as_str()
                .filter(|l| !l.is_empty())
                .unwrap_or(value);
            Some(Choice {
                value: value.to_string(),
                label: if installed {
                    label.to_string()
                } else {
                    format!("{label} · requer instalação")
                },
                installed,
            })
        })
        .collect();
    if !current.is_empty() && !choices.iter().any(|c| c.value == current) {
        choices.insert(
            0,
            Choice {
                value: current.to_string(),
                label: current.to_string(),
                installed: false,
            },
        );
    }
    choices
}

fn selected_warning(status: &Value, current: &str) -> Option<String> {
    catalog_entries(status)
        .iter()
        .find(|entry| entry["asr_model"].as_str() == Some(current))
        .and_then(|entry| entry["warning"].as_str())
        .filter(|w| !w.is_empty())
        .map(String::from)
}

fn preview_voice(entry: &Value) -> Option<String> {
    entry["voice"]
        .as_str()
        .filter(|v| !v.is_empty())
        .map(String::from)
}

fn settings_loaded(ui: Ui) -> bool {
    ui.settings.get()["voice"].is_string()
}

fn choice_options(ui: Ui, key: &'static str) -> impl IntoView {
    let current = text(
        &ui.settings.get(),
        if key == "voice" { "voice" } else { "asr_model" },
    );
    catalog_choices(&ui.artifacts.get(), key, &current)
        .into_iter()
        .map(|choice| {
            let selected = choice.value == current;
            view! {<option value=choice.value selected=selected>{choice.label}</option>}
        })
        .collect_view()
}

#[component]
fn SettingsPage(ui: Ui) -> impl IntoView {
    view! {
        <div class="page-heading"><div><div class="eyebrow">"Do seu jeito"</div><h1>"Uma voz. O seu modelo."</h1><p class="subtitle">"Configure uma vez e escolha como conversar."</p></div></div>
        <fieldset class="settings-fields" disabled=move||ui.busy.get()||!desktop()>
        <div class="panel"><h2>"Modelo de suporte"</h2><p class="subtitle">"Escolha um modelo com português, streaming e ferramentas. O teste libera a conversa."</p>
            <label class="field">"Provedor"<select prop:value=move||text(&ui.settings.get()["provider"],"kind") on:change=move|ev|{let kind=event_target_value(&ev);ui.provider_set("kind",json!(kind));ui.provider_set("base_url",json!(if kind=="ollama"{"http://127.0.0.1:11434"}else{"http://127.0.0.1:11434/v1"}));}><option value="ollama">"Ollama · modelo local"</option><option value="compatible_api">"API compatível com Chat Completions"</option></select></label>
            <label class="field">"Endereço do provedor"<input type="url" prop:value=move||text(&ui.settings.get()["provider"],"base_url") on:input=move|ev|ui.provider_set("base_url",json!(event_target_value(&ev)))/></label>
            <div class="form-grid"><label class="field">"Modelo"<input list="discovered-models" prop:value=move||text(&ui.settings.get()["provider"],"model") on:input=move|ev|ui.provider_set("model",json!(event_target_value(&ev)))/><datalist id="discovered-models">{move||ui.models.get().into_iter().map(|model|view!{<option value=model/>}).collect_view()}</datalist></label>
                <label class="field">"Contexto em tokens"<input type="number" min="8192" max="131072" prop:value=move||ui.settings.get()["provider"]["context_tokens"].to_string() on:input=move|ev|{if let Ok(value)=event_target_value(&ev).parse::<u32>(){ui.provider_set("context_tokens",json!(value));}}/></label></div>
            <label class="field">"Chave de API (opcional para Ollama)"<input type="password" autocomplete="off" placeholder="Deixe em branco para manter a chave atual" prop:value=move||ui.provider_key.get() on:input=move|ev|{ui.provider_key.set(event_target_value(&ev));ui.invalidate_provider();}/></label>
            <p class="subtitle" role="status">{move||match ui.provider_state.get(){"checking"=>"Verificando o modelo…".to_string(),"ready"=>"Modelo validado: português, streaming, ferramentas e cancelamento.".to_string(),_=>ui.provider_error.get()}}</p>
            <div class="page-actions"><button class="secondary" on:click=move |_|ui.task(async move{ui.save().await?;let value=request("provider_models",json!({})).await?;ui.models.set(array(&value).into_iter().filter_map(|v|v.as_str().map(String::from)).collect());ui.notice.set("Modelos encontrados. Escolha um e salve para validar.".into());Ok(())})>"Buscar modelos"</button>
                <button class="primary" on:click=move |_|ui.task(async move{ui.save().await})>"Tentar novamente"</button></div>
        </div>
        <div class="panel"><h2>"Áudio local"</h2><p class="subtitle">"Baixe transcrição, detecção de fala e uma voz. Os modelos permanecem no computador."</p>
            <div class="model-list">{move||{let entries=catalog_entries(&ui.artifacts.get());if entries.is_empty(){view!{<p class="subtitle" role="status">"Status dos modelos indisponível ou ainda carregando. Use “Verificar instalação”."</p>}.into_any()}else{entries.into_iter().map(|entry|view!{<ArtifactCard ui entry/>}).collect_view().into_any()}}}</div>
            <p class="subtitle" role="status">{move||ui.download.get()}</p>
            <p class="subtitle" role="status" aria-live="polite">{move||speech_message(&ui.speech.get()).unwrap_or_else(||"A fala local será preparada em segundo plano ao abrir o aplicativo.".into())}</p>
            <Show when=move||speech_can_retry(&ui.speech.get())><button class="text-button" on:click=move |_|ui.start_speech()>"Tentar preparar a fala novamente"</button></Show>
            <button class="text-button" on:click=move |_|ui.task(async move{ui.refresh_models().await})>"Verificar instalação"</button>
            <div class="form-grid"><label class="field">"Voz"<select disabled=move||!settings_loaded(ui) on:change=move|ev|ui.set("voice",json!(event_target_value(&ev)))>{move||choice_options(ui,"voice")}</select></label>
            <label class="field">"Modelo de transcrição"<select disabled=move||!settings_loaded(ui) on:change=move|ev|ui.set("asr_model",json!(event_target_value(&ev)))>{move||choice_options(ui,"asr_model")}</select></label>
            <Show when=move||selected_warning(&ui.artifacts.get(),&text(&ui.settings.get(),"asr_model")).is_some()><p class="conflict-note" role="status">{move||selected_warning(&ui.artifacts.get(),&text(&ui.settings.get(),"asr_model")).unwrap_or_default()}</p></Show>
            <label class="field">"Velocidade"<input type="number" min="0.85" max="1.15" step="0.05" prop:value=move||ui.settings.get()["speed"].to_string() on:input=move|ev|{if let Ok(value)=event_target_value(&ev).parse::<f64>(){ui.set("speed",json!(value));}}/></label></div>
            <label class="check-field"><input type="checkbox" prop:checked=move||ui.settings.get()["speak_text"]==true on:change=move|ev|ui.set("speak_text",json!(event_target_checked(&ev)))/>"Ler respostas em voz alta"</label>
            <div class="form-grid"><DeviceSelect ui direction="input" label="Microfone"/><DeviceSelect ui direction="output" label="Saída de áudio"/></div>
            <Show when=move||!ui.audio_error.get().is_empty()><p class="conflict-note" role="alert">{move||ui.audio_error.get()}</p></Show>
            <button class="text-button" on:click=move |_|ui.task(async move{ui.refresh_devices().await;Ok(())})>"Atualizar dispositivos"</button>
        </div>
        <div class="panel"><h2>"Google Calendar"</h2><p class="subtitle">"Use as credenciais de um cliente OAuth do tipo Desktop. A autorização abre no navegador do sistema."</p>
            <label class="field">"Client ID"<input autocomplete="off" prop:value=move||text(&ui.settings.get(),"google_client_id") on:input=move|ev|ui.set("google_client_id",json!(event_target_value(&ev)))/></label>
            <label class="field">"Client secret do cliente Desktop"<input type="password" autocomplete="off" placeholder="Deixe em branco para manter o valor configurado" prop:value=move||ui.google_secret.get() on:input=move|ev|ui.google_secret.set(event_target_value(&ev))/></label>
            <label class="field">"Fuso das suas falas e compromissos"<input placeholder="America/Bahia" prop:value=move||text(&ui.settings.get(),"time_zone") on:input=move|ev|ui.set("time_zone",json!(event_target_value(&ev)))/></label>
        </div>
        <div class="panel"><h2>"Pesquisa na internet"</h2><label class="field">"Chave Brave Search"<input type="password" autocomplete="off" placeholder="Deixe em branco para manter a chave atual" prop:value=move||ui.brave_key.get() on:input=move|ev|ui.brave_key.set(event_target_value(&ev))/></label><p class="subtitle">"Somente a consulta pública é enviada à pesquisa. O agente é instruído a preservar o conteúdo privado do usuário."</p></div>
        <div class="page-actions"><button class="primary" on:click=move |_|ui.task(async move{ui.save().await})>"Salvar configurações"</button></div>
        </fieldset>
    }
}

#[component]
fn ArtifactCard(ui: Ui, entry: Value) -> impl IntoView {
    let id = text(&entry, "id");
    let label = text(&entry, "label");
    let description = text(&entry, "description");
    let warning = text(&entry, "warning");
    let license = text(&entry, "license");
    let installed = entry["installed"] == true;
    let recommended = entry["recommended"] == true;
    let voice = preview_voice(&entry);
    view! {<div class="model-item"><strong>{label}{if recommended{" · recomendado"}else{""}}</strong><small>{description}</small>
    {(!warning.is_empty()).then(||view!{<small class="conflict-note">{warning}</small>})}
    <small>{format!("Licença {license} · {}",if installed{"Instalado localmente"}else{"Download necessário"})}</small><div class="page-actions"><button class="text-button" on:click=move |_|{let id=id.clone();ui.task(async move{
        let callback=Closure::<dyn FnMut(String)>::new(move|event|ui.event(event)).into_js_value();
        let result=stream_js("model_install",&json!({"id":id}).to_string(),&callback,next_stream_id()).await.map_err(js_error);
        ui.download.set(String::new());result?;ui.refresh_models().await?;ui.notice.set("Modelo instalado e integridade verificada.".into());
        if speech_can_retry(&ui.speech.get_untracked()){ui.start_speech();}Ok(())
    })}>{if installed{"Verificar / atualizar"}else{"Instalar"}}</button>
    {voice.map(|voice|view!{<button class="text-button" disabled=!installed on:click=move |_|{let voice=voice.clone();ui.task(async move{request("voice_preview",json!({"id":voice})).await?;Ok(())})}>"Ouvir voz"</button>})}</div></div>}
}

#[component]
fn DeviceSelect(ui: Ui, direction: &'static str, label: &'static str) -> impl IntoView {
    let key = if direction == "input" {
        "input_device"
    } else {
        "output_device"
    };
    view! {<label class="field">{label}<select prop:value=move||text(&ui.settings.get(),key) on:change=move|ev|{let value=event_target_value(&ev);ui.set(key,if value.is_empty(){Value::Null}else{json!(value)});}>
    <option value="">"Padrão do sistema"</option>{move||device_options(ui.devices.get(),direction,&text(&ui.settings.get(),key))}</select></label>
    <Show when=move||direction=="input"><p class="saved-input">{move||format!("Microfone salvo: {}",saved_input_label(&ui.devices.get(),&ui.persisted_input.get()))}</p>
        <Show when=move||input_selection(&text(&ui.settings.get(),"input_device"),&ui.persisted_input.get(),ui.saving.get())!=InputSelection::Saved>
            <p class="unsaved-input" role="status">{move||input_selection_message(input_selection(&text(&ui.settings.get(),"input_device"),&ui.persisted_input.get(),ui.saving.get()))}</p></Show></Show>}
}

fn device_options(
    devices: Vec<Value>,
    direction: &'static str,
    current: &str,
) -> impl IntoView + use<> {
    let listed = devices
        .into_iter()
        .filter(|device| device["direction"] == direction)
        .collect::<Vec<_>>();
    let missing = !current.is_empty() && !listed.iter().any(|device| text(device, "id") == current);
    let unavailable = missing.then(|| {
        view! {<option value=current.to_string() selected=true>"indisponível — selecione novamente"</option>}
    });
    view! {
        {unavailable}
        {listed.into_iter().map(|device|{let id=text(&device,"id");let selected=id==current;view!{<option value=id selected=selected>{text(&device,"name")}</option>}}).collect_view()}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> Value {
        json!({"packages":{},"artifacts":[
            {"id":"whisper-turbo","label":"Whisper Turbo","installed":true,"asr_model":"turbo","voice":null,"warning":null},
            {"id":"whisper-small","label":"Whisper Small","installed":false,"asr_model":"small","voice":null,"warning":"Aviso small"},
            {"id":"silero-vad","label":"Silero VAD","installed":true,"asr_model":null,"voice":null,"warning":null},
            {"id":"kokoro-multi-lang-v1_0","label":"Kokoro","installed":false,"asr_model":null,"voice":"pf_dora","warning":null}
        ]})
    }

    #[test]
    fn empty_status_shows_only_current_value() {
        let empty = Value::Null;
        assert!(catalog_entries(&empty).is_empty());
        assert!(catalog_choices(&empty, "voice", "").is_empty());
        let choices = catalog_choices(&empty, "voice", "pf_dora");
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].value, "pf_dora");
        assert_eq!(selected_warning(&empty, "small"), None);
    }

    #[test]
    fn absent_current_value_is_temporary_option() {
        let choices = catalog_choices(&status(), "voice", "outra");
        assert_eq!(choices[0].value, "outra");
        assert_eq!(choices.len(), 2);
    }

    #[test]
    fn installed_and_uninstalled_choices() {
        let asr = catalog_choices(&status(), "asr_model", "turbo");
        assert_eq!(asr.len(), 2);
        assert!(asr[0].installed && asr[0].label == "Whisper Turbo");
        assert!(!asr[1].installed && asr[1].label.contains("requer instalação"));
        assert_eq!(asr[1].value, "small");
    }

    #[test]
    fn voice_id_differs_from_artifact_id() {
        let voices = catalog_choices(&status(), "voice", "pf_dora");
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].value, "pf_dora");
        let entry = &catalog_entries(&status())[3];
        assert_eq!(entry["id"], "kokoro-multi-lang-v1_0");
        assert_eq!(preview_voice(entry).as_deref(), Some("pf_dora"));
        assert_eq!(preview_voice(&catalog_entries(&status())[0]), None);
    }

    #[test]
    fn small_warning_lookup() {
        assert_eq!(
            selected_warning(&status(), "small").as_deref(),
            Some("Aviso small")
        );
        assert_eq!(selected_warning(&status(), "turbo"), None);
    }

    fn complete_probe() -> Value {
        json!({"tools":true,"streaming":true,"portuguese":true,"cancellation":true})
    }

    #[test]
    fn stale_check_results_are_ignored() {
        assert_eq!(check_outcome(2, 1, Ok(complete_probe())), None);
        assert_eq!(check_outcome(2, 1, Err("falha".into())), None);
        assert_eq!(
            check_outcome(2, 2, Ok(complete_probe())),
            Some(Readiness::Ready)
        );
    }

    #[test]
    fn failures_and_incomplete_probes_are_unavailable_with_guidance() {
        let Some(Readiness::Unavailable(message)) =
            check_outcome(1, 1, Err("Provedor inacessível.".into()))
        else {
            panic!("expected unavailable");
        };
        assert!(message.contains("Provedor inacessível.") && message.contains("modelo compatível"));
        let mut partial = complete_probe();
        partial["cancellation"] = json!(false);
        assert!(matches!(
            check_outcome(1, 1, Ok(partial)),
            Some(Readiness::Unavailable(_))
        ));
        assert!(matches!(
            check_outcome(1, 1, Ok(Value::Null)),
            Some(Readiness::Unavailable(_))
        ));
    }

    #[test]
    fn invalidation_makes_in_flight_result_stale_and_unavailable() {
        let ui = Ui::new();
        let generation = ui.check_generation.get_untracked();
        ui.provider_state.set("checking");
        ui.invalidate_provider();
        assert_eq!(ui.provider_state.get_untracked(), "unavailable");
        ui.apply_check(generation, Ok(complete_probe()));
        assert_eq!(ui.provider_state.get_untracked(), "unavailable");
        assert_eq!(ui.provider_error.get_untracked(), PROVIDER_CHANGED);
        let current = ui.check_generation.get_untracked();
        ui.apply_check(current, Ok(complete_probe()));
        assert_eq!(ui.provider_state.get_untracked(), "ready");
        assert!(ui.provider_error.get_untracked().is_empty());
    }

    #[test]
    fn stale_failed_start_cannot_reset_newer_start() {
        assert!(owns_start(3, 3, Some(7), 7));
        assert!(!owns_start(4, 3, Some(8), 7));
        assert!(!owns_start(3, 3, Some(8), 7));
        assert!(!owns_start(3, 3, None, 7));
        let ui = Ui::new();
        ui.epoch.set(4);
        ui.active_stream.set(Some(8));
        ui.running.set(true);
        ui.state.set("thinking".into());
        assert!(!ui.fail_start(3, 7));
        assert!(ui.running.get_untracked());
        assert_eq!(ui.state.get_untracked(), "thinking");
        assert_eq!(ui.active_stream.get_untracked(), Some(8));
        assert!(ui.fail_start(4, 8));
        assert!(!ui.running.get_untracked());
        assert_eq!(ui.active_stream.get_untracked(), None);
    }

    #[test]
    fn stop_completion_is_stale_after_newer_start() {
        assert!(epoch_current(5, 5));
        assert!(!epoch_current(6, 5));
    }

    #[test]
    fn stream_events_require_epoch_session_and_active_stream() {
        assert!(stream_current(2, 2, 9, 9, Some(1), 1));
        assert!(!stream_current(3, 2, 9, 9, Some(1), 1));
        assert!(!stream_current(2, 2, 8, 9, Some(1), 1));
        assert!(!stream_current(2, 2, 9, 9, Some(2), 1));
        assert!(!stream_current(2, 2, 9, 9, None, 1));
        let ui = Ui::new();
        ui.epoch.set(3);
        ui.session.set(9);
        ui.active_stream.set(Some(1));
        let raw = json!({"kind":"run.started","session_id":9,"generation":4}).to_string();
        ui.stream_event(2, 9, 1, raw.clone());
        ui.stream_event(3, 8, 1, raw.clone());
        ui.stream_event(3, 9, 2, raw.clone());
        assert!(!ui.running.get_untracked());
        assert_eq!(ui.generation.get_untracked(), 0);
        ui.stream_event(3, 9, 1, raw);
        assert!(ui.running.get_untracked());
        assert_eq!(ui.generation.get_untracked(), 4);
    }

    #[test]
    fn generation_is_established_by_first_event_then_enforced() {
        assert_eq!(accept_generation(0, 4), Some(4));
        assert_eq!(accept_generation(0, 0), None);
        assert_eq!(accept_generation(4, 4), Some(4));
        assert_eq!(accept_generation(4, 5), None);
    }

    #[test]
    fn terminal_before_ticket_does_not_reactivate_ui() {
        assert!(is_terminal(r#"{"kind":"run.completed"}"#));
        assert!(is_terminal(r#"{"kind":"turn.cancelled"}"#));
        assert!(!is_terminal(r#"{"kind":"state.changed"}"#));
        let ui = Ui::new();
        ui.epoch.set(2);
        ui.active_stream.set(Some(5));
        // Terminal replayed before the ticket settles: running false, stream released.
        ui.running.set(false);
        ui.release_stream(5);
        assert_eq!(ui.active_stream.get_untracked(), None);
        // A stale release for another stream never clears a newer one.
        ui.active_stream.set(Some(6));
        ui.release_stream(5);
        assert_eq!(ui.active_stream.get_untracked(), Some(6));
        // Ticket settlement only checks the epoch and never restores running/stream.
        assert!(epoch_current(ui.epoch.get_untracked(), 2));
        ui.active_stream.set(None);
        assert!(!ui.running.get_untracked());
        assert_eq!(ui.active_stream.get_untracked(), None);
    }

    #[test]
    fn audio_error_is_visible_and_does_not_touch_provider_readiness() {
        let ui = Ui::new();
        ui.provider_state.set("ready");
        ui.audio_error.set(audio_failure("Nenhum microfone."));
        assert!(ui.audio_error.get_untracked().contains("Nenhum microfone."));
        assert!(ui.audio_error.get_untracked().contains("texto continua"));
        assert_eq!(ui.provider_state.get_untracked(), "ready");
    }

    #[test]
    fn speech_polls_only_while_a_job_is_active() {
        assert!(speech_polling(
            &json!({"phase":"downloading","active":true})
        ));
        assert!(speech_polling(
            &json!({"phase":"ready","ready":true,"active":true})
        ));
        for done in [
            json!({"phase":"idle","active":false}),
            json!({"phase":"ready","ready":true,"active":false}),
            json!({"phase":"failed","active":false}),
        ] {
            assert!(!speech_polling(&done));
        }
    }

    #[test]
    fn speech_progress_and_failures_are_portuguese_and_actionable() {
        assert_eq!(speech_message(&json!({"phase":"idle"})), None);
        let download = speech_message(&json!({"phase":"downloading","active":true,
            "artifact":"whisper-turbo","file":"turbo-encoder.int8.onnx",
            "downloaded":300_000_000u64,"total":674_000_000u64}))
        .unwrap();
        assert!(download.contains("Baixando Whisper Turbo") && download.contains("300 / 674 MB"));
        let copy =
            speech_message(&json!({"phase":"copying","active":true,"artifact":"silero-vad"}))
                .unwrap();
        assert!(copy.contains("Copiando Silero VAD"));
        let offline = speech_message(&json!({"phase":"failed","code":"MODEL_NETWORK",
            "error":"Falha de rede: offline"}))
        .unwrap();
        assert!(offline.contains("conexão") && offline.contains("texto continua"));
        let disk =
            speech_message(&json!({"phase":"failed","code":"MODEL_IO","error":"x"})).unwrap();
        assert!(disk.contains("espaço livre"));
        let integrity =
            speech_message(&json!({"phase":"failed","code":"MODEL_INTEGRITY","error":"x"}))
                .unwrap();
        assert!(integrity.contains("integridade"));
        let ready_with_optional = speech_message(&json!({"phase":"downloading","ready":true,
            "active":true,"artifact":"whisper-turbo"}))
        .unwrap();
        assert!(ready_with_optional.starts_with("Fala local pronta"));
        let warned = json!({"phase":"ready","ready":true,"active":false,"warning":"Turbo falhou."});
        assert!(speech_message(&warned).unwrap().contains("Turbo falhou."));
        assert!(speech_notice_visible(&warned));
        assert!(!speech_notice_visible(
            &json!({"phase":"ready","ready":true,"active":false})
        ));
        assert!(!speech_notice_visible(&json!({"phase":"idle"})));
    }

    #[test]
    fn retry_is_offered_only_after_failure() {
        assert!(speech_can_retry(
            &json!({"phase":"failed","ready":false,"active":false})
        ));
        assert!(speech_can_retry(
            &json!({"phase":"unsupported","active":false})
        ));
        assert!(!speech_can_retry(
            &json!({"phase":"downloading","active":true})
        ));
        assert!(!speech_can_retry(
            &json!({"phase":"ready","ready":true,"active":false})
        ));
        let failure = speech_request_failure("sem resposta");
        assert!(speech_can_retry(&failure));
        assert!(!speech_polling(&failure));
    }

    #[test]
    fn asr_change_restarts_speech_only_when_the_selection_differs() {
        assert!(asr_changed("turbo", "small"));
        assert!(!asr_changed("turbo", "turbo"));
        assert!(!asr_changed("", "small"));
    }

    #[test]
    fn speech_state_is_independent_of_provider_readiness_and_text_input() {
        let ui = Ui::new();
        ui.provider_state.set("ready");
        ui.speech.set(speech_request_failure("offline"));
        assert_eq!(ui.provider_state.get_untracked(), "ready");
        assert!(!ui.busy.get_untracked());
        ui.speech.set(json!({"phase":"downloading","active":true}));
        assert_eq!(ui.provider_state.get_untracked(), "ready");
        assert!(!ui.busy.get_untracked() && !ui.running.get_untracked());
        // A snapshot for a superseded preparation is ignored.
        ui.speech_poll.set(2);
        ui.apply_speech(1, json!({"phase":"ready","ready":true}));
        assert_eq!(ui.speech.get_untracked()["phase"], "downloading");
        ui.apply_speech(2, json!({"phase":"ready","ready":true,"active":false}));
        assert_eq!(ui.speech.get_untracked()["phase"], "ready");
    }

    fn ready_speech() -> Value {
        json!({"phase":"ready","ready":true,"active":false})
    }
    fn mic(id: &str) -> Value {
        json!({"id":id,"name":"Microfone","direction":"input","default":true})
    }

    #[test]
    fn voice_start_is_gated_in_order_and_text_stays_independent() {
        let devices = vec![mic("input:0")];
        assert_eq!(
            voice_blocker(
                "unavailable",
                &ready_speech(),
                Some(&devices),
                "",
                "",
                false
            ),
            Some(VoiceBlocker::Provider)
        );
        assert_eq!(
            voice_blocker(
                "ready",
                &json!({"phase":"downloading","active":true}),
                Some(&devices),
                "",
                "",
                false
            ),
            Some(VoiceBlocker::Models)
        );
        assert_eq!(
            voice_blocker("ready", &ready_speech(), Some(&devices), "", "", false),
            None
        );
        assert_eq!(
            voice_blocker("ready", &ready_speech(), Some(&[]), "", "", false),
            Some(VoiceBlocker::Input)
        );
        let outputs = vec![json!({"id":"output:0","direction":"output"})];
        assert_eq!(
            voice_blocker("ready", &ready_speech(), Some(&outputs), "", "", false),
            Some(VoiceBlocker::Input)
        );
        assert_eq!(
            voice_blocker(
                "ready",
                &ready_speech(),
                Some(&devices),
                "input:7",
                "input:7",
                false
            ),
            Some(VoiceBlocker::InvalidInput)
        );
        assert_eq!(
            voice_blocker(
                "ready",
                &ready_speech(),
                Some(&devices),
                "input:0",
                "input:0",
                false
            ),
            None
        );
        // Unknown device availability never blocks.
        assert_eq!(
            voice_blocker("ready", &ready_speech(), None, "input:7", "input:7", false),
            None
        );
        // Text operation only looks at the provider.
        let ui = Ui::new();
        ui.provider_state.set("ready");
        ui.speech.set(json!({"phase":"failed","ready":false}));
        assert_eq!(ui.voice_blocker(), Some(VoiceBlocker::Models));
        assert_eq!(ui.provider_state.get_untracked(), "ready");
    }

    #[test]
    fn voice_start_waits_for_the_saved_input_not_the_draft() {
        let devices = vec![mic("input:0"), mic("input:1")];
        assert_eq!(
            voice_blocker(
                "ready",
                &ready_speech(),
                Some(&devices),
                "input:1",
                "input:0",
                false
            ),
            Some(VoiceBlocker::UnsavedInput)
        );
        assert_eq!(
            voice_blocker(
                "ready",
                &ready_speech(),
                Some(&devices),
                "input:0",
                "input:0",
                true
            ),
            Some(VoiceBlocker::SavingInput)
        );
        assert_eq!(
            input_selection("input:1", "input:0", false),
            InputSelection::Unsaved
        );
        assert_eq!(
            input_selection("input:0", "input:0", true),
            InputSelection::Saving
        );
        assert_eq!(
            input_selection("input:0", "input:0", false),
            InputSelection::Saved
        );
        assert!(input_selection_message(InputSelection::Unsaved).contains("não salva"));
        assert!(input_selection_message(InputSelection::Saving).contains("Salvando"));
        assert!(input_selection_message(InputSelection::Saved).is_empty());
    }

    #[test]
    fn saved_identity_changes_only_after_a_successful_save() {
        assert_eq!(persisted_after_save("input:0", "input:1", true), "input:1");
        assert_eq!(persisted_after_save("input:0", "input:1", false), "input:0");
        assert_eq!(persisted_after_save("", "", true), "");
        let persisted = persisted_after_save("input:0", "input:1", false);
        assert_eq!(
            voice_blocker(
                "ready",
                &ready_speech(),
                Some(&[mic("input:0"), mic("input:1")]),
                "input:1",
                &persisted,
                false
            ),
            Some(VoiceBlocker::UnsavedInput)
        );
    }

    #[test]
    fn missing_and_legacy_selections_are_shown_as_unavailable_not_default() {
        let devices = vec![
            json!({"id":"v1:[\"input\",\"Fone\"]","name":"Fone","direction":"input"}),
            json!({"id":"v1:[\"output\",\"Caixa\"]","name":"Caixa","direction":"output"}),
        ];
        assert_eq!(saved_input_label(&devices, ""), "Padrão do sistema");
        assert_eq!(
            saved_input_label(&devices, "v1:[\"input\",\"Fone\"]"),
            "Fone"
        );
        assert_eq!(
            saved_input_label(&devices, "input:0"),
            "indisponível — selecione novamente"
        );
        assert_eq!(
            saved_input_label(&devices, "v1:[\"input\",\"Sumiu\"]"),
            "indisponível — selecione novamente"
        );
        assert_eq!(
            voice_blocker(
                "ready",
                &ready_speech(),
                Some(&devices),
                "input:0",
                "input:0",
                false
            ),
            Some(VoiceBlocker::InvalidInput)
        );
        let message = VoiceBlocker::InvalidInput.message(&json!({}));
        assert!(message.contains("Selecione novamente") && !message.contains("Nenhum microfone"));
    }

    #[test]
    fn input_meter_follows_only_listening_input_levels() {
        assert_eq!(meter_rms("input", "listening", Some(0.42)), Some(0.42));
        assert_eq!(meter_rms("output", "listening", Some(0.9)), None);
        assert_eq!(meter_rms("input", "preparing", Some(0.9)), None);
        assert_eq!(meter_rms("input", "speaking", Some(0.9)), None);
        assert_eq!(meter_rms("input", "listening", Some(1.7)), Some(1.));
        assert_eq!(meter_rms("input", "listening", Some(-0.3)), Some(0.));
        assert_eq!(meter_rms("input", "listening", Some(f64::NAN)), Some(0.));
        assert_eq!(
            meter_rms("input", "listening", Some(f64::INFINITY)),
            Some(0.)
        );
        assert_eq!(meter_rms("input", "listening", None), Some(0.));
    }

    #[test]
    fn input_feedback_is_text_only_and_clears_as_documented() {
        assert_eq!(
            input_feedback_message("empty_speech"),
            Some("Nenhuma fala detectada. Tente falar novamente.")
        );
        assert_eq!(
            input_feedback_message("empty_transcript"),
            Some("Não foi possível reconhecer palavras no áudio. Tente novamente.")
        );
        assert_eq!(input_feedback_message("other"), None);
        for reason in ["empty_speech", "empty_transcript"] {
            assert_eq!(
                notice_for_event("input.feedback", &json!({"reason":reason})),
                None
            );
        }
        let ui = Ui::new();
        ui.input_feedback.set("Nenhuma fala detectada.".into());
        ui.event(
            json!({"kind":"input.feedback","session_id":0,"generation":0,
            "payload":{"reason":"empty_transcript"}})
            .to_string(),
        );
        assert_eq!(ui.input_feedback.get_untracked(), "Nenhuma fala detectada.");
        ui.event(
            json!({"kind":"state.changed","session_id":0,"generation":0,
            "payload":{"state":"listening"}})
            .to_string(),
        );
        assert_eq!(ui.input_feedback.get_untracked(), "Nenhuma fala detectada.");
        ui.active_stream.set(Some(1));
        ui.stream_event(
            ui.epoch.get_untracked(),
            0,
            1,
            json!({"kind":"state.changed","session_id":0,"generation":5,
                "payload":{"state":"listening"}})
            .to_string(),
        );
        assert_eq!(ui.input_feedback.get_untracked(), "Nenhuma fala detectada.");
        ui.stream_event(
            ui.epoch.get_untracked(),
            0,
            1,
            json!({"kind":"input.feedback","session_id":0,"generation":5,
                "payload":{"reason":"empty_transcript"}})
            .to_string(),
        );
        assert_eq!(
            ui.input_feedback.get_untracked(),
            "Não foi possível reconhecer palavras no áudio. Tente novamente."
        );
        ui.stream_event(
            ui.epoch.get_untracked(),
            0,
            1,
            json!({"kind":"state.changed","session_id":0,"generation":5,
                "payload":{"state":"listening"}})
            .to_string(),
        );
        assert_eq!(
            ui.input_feedback.get_untracked(),
            "Não foi possível reconhecer palavras no áudio. Tente novamente."
        );
        ui.stream_event(
            ui.epoch.get_untracked(),
            0,
            1,
            json!({"kind":"transcript.final","session_id":0,"generation":5,
                "payload":{"text":"olá"}})
            .to_string(),
        );
        assert!(ui.input_feedback.get_untracked().is_empty());
    }

    #[test]
    fn blocker_notices_are_silent_for_local_selection_states() {
        assert_eq!(VoiceBlocker::Provider.notice(), Some("provider"));
        assert_eq!(VoiceBlocker::Models.notice(), Some("models"));
        assert_eq!(VoiceBlocker::Input.notice(), Some("capture"));
        assert_eq!(VoiceBlocker::InvalidInput.notice(), Some("capture"));
        assert_eq!(VoiceBlocker::UnsavedInput.notice(), None);
        assert_eq!(VoiceBlocker::SavingInput.notice(), None);
        let unsaved = VoiceBlocker::UnsavedInput.message(&json!({}));
        assert!(unsaved.contains("não foram salvas") && unsaved.contains("texto continua"));
        let saving = VoiceBlocker::SavingInput.message(&json!({}));
        assert!(saving.contains("sendo salva") && saving.contains("texto continua"));
    }

    #[test]
    fn blockers_have_specific_actionable_messages_and_notices() {
        let failed = json!({"phase":"failed","code":"MODEL_NETWORK","error":"Sem rede."});
        let models = VoiceBlocker::Models.message(&failed);
        assert!(models.contains("Sem rede.") && models.contains("conexão"));
        assert!(
            VoiceBlocker::Models
                .message(&json!({"phase":"loading"}))
                .contains("Aguarde")
        );
        assert!(
            VoiceBlocker::Provider
                .message(&json!({}))
                .contains("provedor")
        );
        assert!(
            VoiceBlocker::Input
                .message(&json!({}))
                .contains("microfone")
        );
        assert_eq!(VoiceBlocker::Provider.notice(), Some("provider"));
        assert_eq!(VoiceBlocker::Models.notice(), Some("models"));
        assert_eq!(VoiceBlocker::Input.notice(), Some("capture"));
    }

    #[test]
    fn blocked_attempts_announce_unless_a_notice_is_already_playing() {
        let mut gate = NoticeGate::default();
        assert!(gate.allow_blocked());
        gate.in_flight = true;
        assert!(!gate.allow_blocked());
        gate.in_flight = false;
        assert!(gate.allow_blocked());
    }

    #[test]
    fn failures_are_announced_once_per_attempt_not_per_event_or_poll() {
        let mut gate = NoticeGate::default();
        assert!(gate.allow_failure("synthesis"));
        gate.in_flight = false;
        // Repeated structured events in the same attempt stay silent.
        assert!(!gate.allow_failure("synthesis"));
        assert!(gate.allow_failure("capture"));
        gate.new_attempt();
        assert!(gate.allow_failure("synthesis"));
        // Never overlaps a notice that is still playing.
        gate.new_attempt();
        gate.in_flight = true;
        assert!(!gate.allow_failure("capture"));
    }

    #[test]
    fn only_capture_and_synthesis_failures_request_notices() {
        let synthesis = json!({"code":"AUDIO_ERROR","stage":"synthesis"});
        let playback = json!({"code":"DEVICE_UNAVAILABLE","stage":"playback"});
        assert_eq!(
            notice_for_event("audio.error", &synthesis),
            Some("synthesis")
        );
        assert_eq!(notice_for_event("audio.error", &playback), None);
        assert_eq!(notice_for_event("audio.error", &json!({"code":"X"})), None);
        for code in [
            "DEVICE_UNAVAILABLE",
            "MIC_OPEN_FAILED",
            "AUDIO_FORMAT",
            "AUDIO_OVERRUN",
        ] {
            assert_eq!(
                notice_for_event("error", &json!({"code":code})),
                Some("capture")
            );
        }
        assert_eq!(
            notice_for_event("error", &json!({"code":"PROVIDER_ERROR"})),
            None
        );
        assert_eq!(notice_for_event("state.changed", &synthesis), None);
        let shown = audio_event_message(
            "audio.error",
            &json!({"message":"Falhou.","stage":"playback"}),
        );
        assert!(shown.contains("Falhou.") && shown.contains("dispositivo de saída"));
    }

    #[test]
    fn partial_synthesis_failure_is_visual_only() {
        let partial = json!({
            "code":"AUDIO_ERROR",
            "stage":"synthesis",
            "partial":true,
            "message":"Falhou.",
            "attempted":2,
            "synthesized":1,
            "played":1
        });
        assert_eq!(notice_for_event("audio.error", &partial), None);
        let shown = audio_event_message("audio.error", &partial);
        assert!(shown.contains("Parte da resposta não pôde ser falada"));
        assert!(shown.contains("visível por escrito"));
        let total = json!({
            "code":"AUDIO_ERROR",
            "stage":"synthesis",
            "partial":false,
            "message":"Falhou."
        });
        assert_eq!(notice_for_event("audio.error", &total), Some("synthesis"));
        assert_eq!(audio_event_message("audio.error", &total), "Falhou.");
        let legacy = json!({"code":"AUDIO_ERROR","stage":"synthesis","message":"Falhou."});
        assert_eq!(notice_for_event("audio.error", &legacy), Some("synthesis"));
        let playback = json!({
            "code":"DEVICE_UNAVAILABLE",
            "stage":"playback",
            "message":"Falhou."
        });
        assert_eq!(notice_for_event("audio.error", &playback), None);
        assert!(audio_event_message("audio.error", &playback).contains("dispositivo de saída"));
    }

    #[test]
    fn output_failure_is_displayed_and_never_requests_another_notice() {
        let message = notice_output_failure("Saída indisponível");
        assert!(message.contains("Saída indisponível") && message.contains("dispositivo de saída"));
        // Output failures are not capture/synthesis events, so they cannot recurse.
        let output = json!({"code":"DEVICE_UNAVAILABLE","message":message});
        assert_eq!(notice_for_event("notice.error", &output), None);
        let mut gate = NoticeGate::default();
        assert!(gate.allow_failure("synthesis"));
        assert!(!gate.allow_failure("synthesis"));
    }
}
