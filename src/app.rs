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
    async fn stream_js(command: &str, args: &str, callback: &JsValue) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name=applyOrbLevel)]
    fn orb_level(level: f64);
    #[wasm_bindgen(js_name=formatDate)]
    fn format_date(value: &str, time_zone: &str, all_day: bool, exclusive_end: bool) -> String;
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
    message_cursor: RwSignal<Option<i64>>,
    more_messages: RwSignal<bool>,
    notice: RwSignal<String>,
    busy: RwSignal<bool>,
    input: RwSignal<String>,
    memory_query: RwSignal<String>,
    memory_results: RwSignal<Vec<Value>>,
    service: RwSignal<Value>,
    calendars: RwSignal<Vec<Value>>,
    models: RwSignal<Vec<String>>,
    probe: RwSignal<Value>,
    artifacts: RwSignal<Value>,
    devices: RwSignal<Vec<Value>>,
    operation: RwSignal<Value>,
    provider_key: RwSignal<String>,
    brave_key: RwSignal<String>,
    google_secret: RwSignal<String>,
    download: RwSignal<String>,
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
            message_cursor: RwSignal::new(None),
            more_messages: RwSignal::new(false),
            notice: RwSignal::new(String::new()),
            busy: RwSignal::new(false),
            input: RwSignal::new(String::new()),
            memory_query: RwSignal::new(String::new()),
            memory_results: RwSignal::new(Vec::new()),
            service: RwSignal::new(json!({})),
            calendars: RwSignal::new(Vec::new()),
            models: RwSignal::new(Vec::new()),
            probe: RwSignal::new(Value::Null),
            artifacts: RwSignal::new(Value::Null),
            devices: RwSignal::new(Vec::new()),
            operation: RwSignal::new(Value::Null),
            provider_key: RwSignal::new(String::new()),
            brave_key: RwSignal::new(String::new()),
            google_secret: RwSignal::new(String::new()),
            download: RwSignal::new(String::new()),
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
    }
    async fn refresh_sessions(self) -> Result<(), String> {
        self.sessions
            .set(array(&request("session_list", json!({})).await?));
        Ok(())
    }
    async fn refresh_service(self) -> Result<(), String> {
        self.service
            .set(request("service_status", json!({})).await?);
        Ok(())
    }
    async fn refresh_models(self) -> Result<(), String> {
        self.artifacts
            .set(request("model_status", json!({})).await?);
        Ok(())
    }
    async fn save(self) -> Result<(), String> {
        let optional = |s: String| if s.is_empty() { Value::Null } else { json!(s) };
        let result=request("settings_save",json!({"settings":self.settings.get_untracked(),"provider_key":optional(self.provider_key.get_untracked()),"brave_key":optional(self.brave_key.get_untracked()),"google_secret":optional(self.google_secret.get_untracked())})).await?;
        self.provider_key.set(String::new());
        self.brave_key.set(String::new());
        self.google_secret.set(String::new());
        self.probe.set(request("provider_status", json!({})).await?);
        self.notice.set(
            if result["persistent_credentials"] == false {
                "Configurações salvas. Cofre indisponível: credenciais apenas nesta execução."
            } else {
                "Configurações salvas."
            }
            .into(),
        );
        Ok(())
    }
    async fn new_session(self) -> Result<i64, String> {
        self.stop().await?;
        let session = request(
            "session_create",
            json!({"title":"Conversa · Google Calendar"}),
        )
        .await?;
        let id = session["id"].as_i64().ok_or("Sessão inválida")?;
        self.session.set(id);
        self.messages.set(Vec::new());
        self.sources.set(Vec::new());
        self.operation.set(Value::Null);
        self.page.set("conversation");
        self.refresh_sessions().await?;
        Ok(id)
    }
    async fn stop(self) -> Result<(), String> {
        self.epoch.update(|n| *n += 1);
        request("conversation_cancel", json!({})).await?;
        self.running.set(false);
        self.generation.update(|n| *n += 1);
        self.state.set("idle".into());
        orb_level(0.);
        if self.session.get_untracked() > 0 {
            if let Ok(operation) = request(
                "action_get",
                json!({"session_id":self.session.get_untracked()}),
            )
            .await
            {
                self.operation.set(operation);
            }
        }
        Ok(())
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
        if event["kind"] == "run.started" {
            self.generation.set(generation);
            self.running.set(true);
        }
        if generation != self.generation.get_untracked() {
            return;
        }
        let payload = &event["payload"];
        let turn = text(&event, "turn_id");
        match event["kind"].as_str().unwrap_or("") {
            "state.changed" => self.state.set(text(payload, "state")),
            "audio.level" => orb_level(payload["rms"].as_f64().unwrap_or(0.)),
            "transcript.final" => {
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
            "action.changed" => self.operation.set(payload.clone()),
            "sources" => self.add_sources(array(payload)),
            "error" | "audio.error" => self.notice.set(text(payload, "message")),
            "turn.cancelled" => {
                self.running.set(false);
                self.state.set("idle".into());
                orb_level(0.);
            }
            "run.completed" => {
                self.running.set(false);
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
        let id = if self.session.get_untracked() == 0 {
            self.new_session().await?
        } else {
            self.session.get_untracked()
        };
        self.epoch.update(|n| *n += 1);
        let epoch = self.epoch.get_untracked();
        self.running.set(true);
        let callback = Closure::<dyn FnMut(String)>::new(move |event| {
            if self.epoch.get_untracked() == epoch {
                self.event(event);
            }
        })
        .into_js_value();
        let result = stream_js(
            "conversation_start",
            &json!({"session_id":id,"text":message}).to_string(),
            &callback,
        )
        .await
        .map_err(|error| {
            self.running.set(false);
            js_error(error)
        })?;
        if self.epoch.get_untracked() != epoch {
            return Ok(());
        }
        let ticket: Value = serde_json::from_str(&result.as_string().unwrap_or_default())
            .map_err(|_| "Resposta inválida")?;
        self.generation
            .set(ticket["generation"].as_u64().unwrap_or(0));
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

fn state_copy(state: &str) -> (&'static str, &'static str) {
    match state {
        "preparing" => ("Preparando áudio", "Um instante para abrir o microfone."),
        "listening" => ("Ouvindo", "Pode falar. Estou ouvindo."),
        "transcribing" => ("Transcrevendo", "Transformando sua fala em palavras."),
        "thinking" => ("Pensando", "Um instante para entender."),
        "knowledge" => ("Documentação", "Buscando uma orientação oficial."),
        "memory" => ("Memória", "Lembrando o que já conversamos."),
        "searching" => ("Pesquisando", "Buscando mais informações."),
        "consulting" => ("Consultando agenda", "Verificando seus compromissos."),
        "awaiting_confirmation" => ("Aguardando confirmação", "Confira antes de salvar."),
        "executing" => ("Salvando", "Criando seu compromisso."),
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
            ui.notice.set("Esta é a interface Leptos. Abra o aplicativo Tauri para conectar modelos, áudio e Google Calendar.".into());
            return;
        }
        ui.task(async move {
            ui.settings.set(request("settings_get", json!({})).await?);
            ui.probe.set(request("provider_status", json!({})).await?);
            ui.refresh_sessions().await?;
            ui.refresh_service().await?;
            if let Ok(value) = request("audio_devices", json!({})).await {
                ui.devices.set(array(&value));
            }
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
                <nav class="nav" aria-label="Navegação principal"><Nav ui page="conversation" symbol="◯" label="Conversa"/><Nav ui page="memory" symbol="◷" label="Memória"/><Nav ui page="services" symbol="◇" label="Serviços"/><Nav ui page="settings" symbol="⚙" label="Configurações"/></nav>
                <div class="sidebar-bottom"><p><span class="local-dot"></span>"Áudio local · sua escolha de modelo"</p><div class="sidebar-note">"Uma conversa de cada vez."<br/>"Com calma. Com contexto."</div></div>
            </aside>
            <main class="workspace"><header class="topbar"><div class="breadcrumb">"Seu assistente"<span>"/ Google Calendar"</span></div><div class="header-status"><span class="pill">{move||if ui.running.get(){state_copy(&ui.state.get()).0}else if ui.probe.get().is_null() || ui.probe.get()["config"]!=ui.settings.get()["provider"]{"Configure seu modelo"}else{"Compatibilidade validada"}}</span><Show when=move||ui.running.get()><button class="text-button" on:click=move |_|ui.task(async move{ui.stop().await})>"Parar"</button></Show></div></header>
                <div class="content"><Show when=move||!ui.notice.get().is_empty()><div class="notice" role="status">{move||ui.notice.get()}</div></Show>
                    <section class="page" hidden=move||ui.page.get()!="conversation"><Conversation ui/></section>
                    <section class="page" hidden=move||ui.page.get()!="memory"><MemoryPage ui/></section>
                    <section class="page" hidden=move||ui.page.get()!="services"><ServicesPage ui/></section>
                    <section class="page" hidden=move||ui.page.get()!="settings"><SettingsPage ui/></section>
                    <footer class="footer"><span>"Whisper + Piper · processamento local de áudio"</span><span>"Sua agenda só muda após confirmação"</span></footer>
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
        <div class="page-heading"><div><div class="eyebrow">"Aqui para ouvir"</div><h1>"Vamos conversar."</h1><div class="subtitle">"Sua voz, sua agenda, suas respostas."</div></div><button class="text-button" disabled=move||ui.busy.get() on:click=move |_|ui.task(async move{ui.new_session().await?;Ok(())})>"+ Nova conversa"</button></div>
        <div class="voice-stage"><div class="orb-scene" aria-hidden="true"><div class="orbit"></div><div class="orbit"></div><div class="orb"></div></div>
            <div class="state-label">{move||state_copy(&ui.state.get()).0}</div><h2 class="stage-title">{move||state_copy(&ui.state.get()).1}</h2>
            <p class="stage-description" role="status" aria-live="polite">{move||if ui.running.get(){"Interrompa a resposta pelo botão. Ao terminar, volto a ouvir."}else{"Converse sobre o Calendar, consulte compromissos ou prepare um novo evento."}}</p>
            <div class="stage-controls"><button class="primary" disabled=move||ui.busy.get()||ui.running.get() on:click=move |_|ui.task(async move{ui.start(None).await})>"● Começar conversa"</button>
                <button class="secondary" hidden=move||ui.state.get()!="listening" on:click=move |_|ui.task(async move{request("conversation_finish_input",json!({})).await?;Ok(())})>"Concluir fala"</button>
                <button class="round" aria-label="Interromper áudio e conversa" on:click=move |_|ui.task(async move{ui.stop().await})>"■"</button></div>
            <p class="fine-print">"O microfone é aberto apenas ao começar. O áudio bruto não é salvo."</p>
        </div>
        <div class="configuration-row"><button class="config-card" on:click=move |_|ui.page.set("settings")><span class="config-icon">"✧"</span><span class="config-copy"><small>"Modelo de suporte"</small><strong>{move||text(&ui.settings.get()["provider"],"model")}</strong></span><span>"↗"</span></button>
            <div class="config-card"><span class="config-icon">"≋"</span><div class="config-copy"><label for="active-voice">"Voz da resposta"</label><select id="active-voice" prop:value=move||text(&ui.settings.get(),"voice") disabled=move||ui.running.get() on:change=move|ev|{ui.set("voice",json!(event_target_value(&ev)));ui.task(async move{ui.save().await});}><option value="pt_BR-cadu-medium" disabled=move||!installed(ui,"pt_BR-cadu-medium")>"Cadu · Português brasileiro"</option><option value="pt_BR-faber-medium" disabled=move||!installed(ui,"pt_BR-faber-medium")>"Faber · Português brasileiro"</option></select></div></div></div>
        <ActionCard ui/>
        <section class="conversation-preview"><div class="section-header"><h2>"A conversa, por escrito"</h2><span class="fine-print">{move||if ui.session.get()==0{"Nova conversa".into()}else{format!("Sessão {}",ui.session.get())}}</span></div>
            <Show when=move||ui.messages.get().is_empty()><p class="empty">"Experimente: “Quais compromissos tenho amanhã?” ou “Como funcionam os lembretes do Google Calendar?”"</p></Show>
            <div class="transcript" aria-live="off">{move||ui.messages.get().into_iter().map(|message|view!{<div class="transcript-line"><span class="avatar">{if message.role=="user"{"V"}else{"a"}}</span><p><strong>{if message.role=="user"{"Você"}else{"Aura"}}</strong>{message.text}</p></div>}).collect_view()}</div>
            <Show when=move||ui.more_messages.get()><button class="text-button" disabled=move||ui.running.get()||ui.busy.get() on:click=move |_|ui.task(async move{ui.load_messages(ui.message_cursor.get_untracked()).await})>"Carregar mais mensagens"</button></Show>
            <form class="message-form" on:submit=move|ev|{ev.prevent_default();let text=ui.input.get_untracked();ui.task(async move{ui.start(Some(text)).await});}><label class="sr-only" for="message">"Escreva sua mensagem"</label><input id="message" placeholder="Ou escreva uma mensagem…" maxlength="12000" prop:value=move||ui.input.get() on:input=move|ev|ui.input.set(event_target_value(&ev))/><button class="primary" disabled=move||ui.busy.get()||ui.input.get().trim().is_empty()>"Enviar"</button></form>
            <Sources ui/>
        </section>
    }
}

#[component]
fn ActionCard(ui: Ui) -> impl IntoView {
    view! {<Show when=move||!ui.operation.get().is_null()>{move||{let op=ui.operation.get();let event=&op["event"];let status=text(&op,"status");let pending=status=="awaiting_confirmation";let conflicts=op["result"]["busy"].as_array().is_some_and(|a|!a.is_empty());let reconcile=status=="unknown"||status=="executing";
        view!{<section class="panel action-card" aria-label="Proposta de compromisso"><div class="eyebrow">{if pending{"Confira antes de salvar"}else{"Resultado da operação"}}</div><h2>{text(event,"summary")}</h2><dl><dt>"Agenda"</dt><dd>{if event["calendar_id"]=="primary"{"Principal".into()}else{text(event,"calendar_id")}}</dd><dt>"Início"</dt><dd>{format_date(&text(event,"start"),&text(event,"time_zone"),event["all_day"]==true,false)}</dd><dt>"Término"</dt><dd>{format_date(&text(event,"end"),&text(event,"time_zone"),event["all_day"]==true,true)}</dd><dt>"Fuso"</dt><dd>{text(event,"time_zone")}</dd></dl>
            <p class="subtitle">{match status.as_str(){"awaiting_confirmation"=>"O evento ainda não foi criado. Confirme por voz ou pelo botão.","completed"=>"Criação confirmada pelo Google Calendar.","unknown"=>"O resultado ainda não foi confirmado. Verifique antes de tentar novamente.","executing"=>"A criação está em andamento.","cancelled"=>"Proposta cancelada. Nenhum evento foi criado por esta proposta.",_=>"Não foi possível concluir esta operação."}}</p>
            <Show when=move||pending><Show when=move||conflicts><p class="conflict-note">"Há compromissos nesse intervalo. Confirmar salvará mesmo com sobreposição."</p></Show><div class="page-actions"><button class="primary" disabled=move||ui.busy.get() on:click=move |_|ui.task(async move{ui.action("action_confirm").await})>"Confirmar e salvar"</button><button class="secondary" disabled=move||ui.busy.get() on:click=move |_|ui.task(async move{ui.action("action_cancel").await})>"Cancelar"</button></div></Show>
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
    <div class="panel"><form on:submit=move|ev|{ev.prevent_default();ui.task(async move{ui.memory_results.set(array(&request("memory_search",json!({"query":ui.memory_query.get_untracked()})).await?));Ok(())});}><label class="field">"O que você quer lembrar?"<input type="search" placeholder="Ex.: reunião, lembretes, agenda" prop:value=move||ui.memory_query.get() on:input=move|ev|ui.memory_query.set(event_target_value(&ev))/></label><button class="primary" disabled=move||ui.busy.get()>"Buscar na memória"</button></form>
        {move||ui.memory_results.get().into_iter().map(|result|{let session=result["session_id"].as_i64().unwrap_or(0);view!{<div class="result"><p>{text(&result,"text")}</p><small>{text(&result,"created_at")}</small><button class="text-button" on:click=move |_|ui.task(async move{ui.open_session(session).await})>"Abrir conversa"</button></div>}}).collect_view()}</div>
    <div class="panel"><div class="section-header"><h2>"Conversas"</h2><button class="text-button" on:click=move |_|ui.task(async move{ui.refresh_sessions().await})>"Atualizar"</button></div>
        <Show when=move||ui.sessions.get().is_empty()><p class="empty">"Sua primeira conversa aparecerá aqui."</p></Show>
        {move||ui.sessions.get().into_iter().map(|session|{let id=session["id"].as_i64().unwrap_or(0);view!{<div class="result"><strong>{text(&session,"title")}</strong><small>{text(&session,"created_at")}</small><div class="page-actions"><button class="text-button" on:click=move |_|ui.task(async move{ui.open_session(id).await})>"Abrir e retomar"</button><button class="text-button" on:click=move |_|ui.task(async move{request("session_delete",json!({"session_id":id})).await?;if ui.session.get_untracked()==id{ui.session.set(0);ui.messages.set(Vec::new());ui.operation.set(Value::Null);}ui.refresh_sessions().await})>"Excluir"</button></div></div>}}).collect_view()}
        <button class="text-button" on:click=move |_|ui.task(async move{let before=ui.sessions.get_untracked().last().and_then(|s|s["id"].as_i64());let next=array(&request("session_list",json!({"before":before})).await?);ui.sessions.update(|all|all.extend(next));Ok(())})>"Carregar conversas anteriores"</button>
    </div>}
}

#[component]
fn ServicesPage(ui: Ui) -> impl IntoView {
    view! {
        <div class="page-heading"><div><div class="eyebrow">"Assistência que acompanha você"</div><h1>"Seus serviços."</h1><p class="subtitle">"Google Calendar é o primeiro. Novos serviços compartilharão a mesma conversa."</p></div></div>
        <div class="panel"><div class="section-header"><h2>"Google Calendar"</h2><span class="pill">{move||if ui.service.get()["google_calendar"]["connection"].is_object(){"Conectado"}else{"Não conectado"}}</span></div>
            <p class="subtitle">"Consulte eventos e disponibilidade. Toda criação de compromisso pede sua confirmação."</p>
            <Show when=move||ui.service.get()["google_calendar"]["connection"].is_object()><p>{move||text(&ui.service.get()["google_calendar"]["connection"],"label")}</p></Show>
            <Show when=move||ui.service.get()["google_calendar"]["connecting"]==true><p class="subtitle">"Conclua a autorização no navegador. A conexão será atualizada automaticamente."</p></Show>
            <Show when=move||ui.service.get()["google_calendar"]["error"].is_object()><p class="conflict-note">{move||text(&ui.service.get()["google_calendar"]["error"],"message")}</p></Show>
            <div class="page-actions"><button class="primary" disabled=move||ui.busy.get()||ui.service.get()["google_calendar"]["connection"].is_object()||ui.service.get()["google_calendar"]["connecting"]==true on:click=move |_|ui.task(async move{
                ui.save().await?;let value=request("service_connect",json!({"service_id":"google_calendar"})).await?;ui.refresh_service().await?;
                ui.notice.set(if value["browser_opened"]==true{"Autorize a conexão no navegador do sistema.".into()}else{format!("Abra no navegador: {}",text(&value,"authorization_url"))});Ok(())
            })>"Conectar conta Google"</button><button class="secondary" disabled=move||ui.busy.get() on:click=move |_|ui.task(async move{request("service_disconnect",json!({"service_id":"google_calendar"})).await?;ui.calendars.set(Vec::new());ui.refresh_service().await})>"Desconectar"</button></div>
            <button class="text-button" on:click=move |_|ui.page.set("settings")>"Configurar credenciais do aplicativo"</button>
        </div>
        <div class="panel"><h2>"Sua agenda"</h2><button class="secondary" disabled=move||ui.busy.get() on:click=move |_|ui.task(async move{let value=request("calendar_list",json!({})).await?;ui.calendars.set(array(&value["items"]));Ok(())})>"Carregar agendas"</button>
            <label class="field">"Agenda padrão"<select prop:value=move||text(&ui.settings.get(),"calendar_id") on:change=move|ev|{ui.set("calendar_id",json!(event_target_value(&ev)));ui.task(async move{ui.save().await});}><option value="primary">"Agenda principal"</option>{move||ui.calendars.get().into_iter().map(|calendar|view!{<option value=text(&calendar,"id")>{format!("{} · {}",text(&calendar,"summary"),if ["owner","writer"].contains(&calendar["accessRole"].as_str().unwrap_or("")){"permite criação"}else{"somente leitura"})}</option>}).collect_view()}</select></label>
            <button class="text-button" disabled=move||ui.busy.get() on:click=move |_|ui.task(async move{ui.page.set("conversation");ui.start(Some("Quais compromissos tenho amanhã?".into())).await})>"Consultar compromissos de amanhã →"</button>
        </div>
        <div class="panel"><h2>"Conhecimento do serviço"</h2><p class="subtitle">"A documentação oficial orienta as respostas. A pesquisa na web pode complementar, sempre identificando as fontes."</p>
            <p class="subtitle">{move||format!("{} documentos locais",ui.service.get()["documents"].as_i64().unwrap_or(0))}</p>
            <button class="secondary" disabled=move||ui.busy.get() on:click=move |_|ui.task(async move{let report=request("knowledge_refresh",json!({"service_id":"google_calendar"})).await?;ui.refresh_service().await?;ui.notice.set(format!("{} documentos atualizados; {} fontes indisponíveis. Versões anteriores foram preservadas.",report["updated"],array(&report["failures"]).len()));Ok(())})>"Atualizar documentação oficial"</button>
        </div>
    }
}

fn installed(ui: Ui, id: &str) -> bool {
    ui.artifacts.get()["artifacts"]
        .as_array()
        .is_some_and(|items| {
            items
                .iter()
                .any(|v| v["id"] == id && v["installed"] == true)
        })
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
            <label class="field">"Chave de API (opcional para Ollama)"<input type="password" autocomplete="off" placeholder="Deixe em branco para manter a chave atual" prop:value=move||ui.provider_key.get() on:input=move|ev|ui.provider_key.set(event_target_value(&ev))/></label>
            <div class="page-actions"><button class="secondary" on:click=move |_|ui.task(async move{ui.save().await?;let value=request("provider_models",json!({})).await?;ui.models.set(array(&value).into_iter().filter_map(|v|v.as_str().map(String::from)).collect());ui.notice.set("Modelos encontrados. Escolha um e teste a compatibilidade.".into());Ok(())})>"Buscar modelos"</button>
                <button class="primary" on:click=move |_|ui.task(async move{ui.save().await?;ui.probe.set(request("provider_probe",json!({})).await?);ui.notice.set("Modelo validado: português, streaming, ferramentas e cancelamento.".into());Ok(())})>"Testar compatibilidade"</button></div>
        </div>
        <div class="panel"><h2>"Áudio local"</h2><p class="subtitle">"Baixe transcrição, detecção de fala e uma voz. Os modelos permanecem no computador."</p>
            <div class="model-list"><ArtifactCard ui id="whisper-base" label="Whisper base · transcrição"/><ArtifactCard ui id="silero-vad" label="Silero · detecção de fala"/><ArtifactCard ui id="pt_BR-cadu-medium" label="Cadu · voz brasileira"/><ArtifactCard ui id="pt_BR-faber-medium" label="Faber · voz brasileira"/></div>
            <p class="subtitle" role="status">{move||ui.download.get()}</p><button class="text-button" on:click=move |_|ui.task(async move{ui.refresh_models().await})>"Verificar instalação"</button>
            <div class="form-grid"><label class="field">"Voz"<select prop:value=move||text(&ui.settings.get(),"voice") on:change=move|ev|ui.set("voice",json!(event_target_value(&ev)))><option value="pt_BR-cadu-medium" disabled=move||!installed(ui,"pt_BR-cadu-medium")>"Cadu"</option><option value="pt_BR-faber-medium" disabled=move||!installed(ui,"pt_BR-faber-medium")>"Faber"</option></select></label>
            <label class="field">"Velocidade"<input type="number" min="0.85" max="1.15" step="0.05" prop:value=move||ui.settings.get()["speed"].to_string() on:input=move|ev|{if let Ok(value)=event_target_value(&ev).parse::<f64>(){ui.set("speed",json!(value));}}/></label></div>
            <label class="check-field"><input type="checkbox" prop:checked=move||ui.settings.get()["speak_text"]==true on:change=move|ev|ui.set("speak_text",json!(event_target_checked(&ev)))/>"Ler respostas em voz alta"</label>
            <div class="form-grid"><DeviceSelect ui direction="input" label="Microfone"/><DeviceSelect ui direction="output" label="Saída de áudio"/></div>
            <button class="text-button" on:click=move |_|ui.task(async move{ui.devices.set(array(&request("audio_devices",json!({})).await?));Ok(())})>"Atualizar dispositivos"</button>
        </div>
        <div class="panel"><h2>"Google Calendar"</h2><p class="subtitle">"Use as credenciais de um cliente OAuth do tipo Desktop. A autorização abre no navegador do sistema."</p>
            <label class="field">"Client ID"<input autocomplete="off" prop:value=move||text(&ui.settings.get(),"google_client_id") on:input=move|ev|ui.set("google_client_id",json!(event_target_value(&ev)))/></label>
            <label class="field">"Client secret do cliente Desktop"<input type="password" autocomplete="off" placeholder="Deixe em branco para manter o valor configurado" prop:value=move||ui.google_secret.get() on:input=move|ev|ui.google_secret.set(event_target_value(&ev))/></label>
            <label class="field">"Fuso das suas falas e compromissos"<input placeholder="America/Bahia" prop:value=move||text(&ui.settings.get(),"time_zone") on:input=move|ev|ui.set("time_zone",json!(event_target_value(&ev)))/></label>
        </div>
        <div class="panel"><h2>"Pesquisa na internet"</h2><label class="field">"Chave Brave Search"<input type="password" autocomplete="off" placeholder="Deixe em branco para manter a chave atual" prop:value=move||ui.brave_key.get() on:input=move|ev|ui.brave_key.set(event_target_value(&ev))/></label><p class="subtitle">"Somente a consulta pública é enviada à pesquisa. O agente é instruído a preservar o conteúdo privado da agenda."</p></div>
        <div class="page-actions"><button class="primary" on:click=move |_|ui.task(async move{ui.save().await})>"Salvar configurações"</button></div>
        </fieldset>
    }
}

#[component]
fn ArtifactCard(ui: Ui, id: &'static str, label: &'static str) -> impl IntoView {
    view! {<div class="model-item"><strong>{label}</strong><small>{move||if installed(ui,id){"Instalado localmente"}else{"Download necessário"}}</small><div class="page-actions"><button class="text-button" on:click=move |_|ui.task(async move{
        let callback=Closure::<dyn FnMut(String)>::new(move|event|ui.event(event)).into_js_value();
        let result=stream_js("model_install",&json!({"id":id}).to_string(),&callback).await.map_err(js_error);
        ui.download.set(String::new());result?;ui.refresh_models().await?;ui.notice.set("Modelo instalado e integridade verificada.".into());Ok(())
    })>{move||if installed(ui,id){"Verificar / atualizar"}else{"Instalar"}}</button>
    <Show when=move||id.starts_with("pt_BR")><button class="text-button" disabled=move||!installed(ui,id) on:click=move |_|ui.task(async move{request("voice_preview",json!({"id":id})).await?;Ok(())})>"Ouvir voz"</button></Show></div></div>}
}

#[component]
fn DeviceSelect(ui: Ui, direction: &'static str, label: &'static str) -> impl IntoView {
    let key = if direction == "input" {
        "input_device"
    } else {
        "output_device"
    };
    view! {<label class="field">{label}<select prop:value=move||text(&ui.settings.get(),key) on:change=move|ev|{let value=event_target_value(&ev);ui.set(key,if value.is_empty(){Value::Null}else{json!(value)});}>
    <option value="">"Padrão do sistema"</option>{move||ui.devices.get().into_iter().filter(|d|d["direction"]==direction).map(|device|view!{<option value=text(&device,"id")>{text(&device,"name")}</option>}).collect_view()}</select></label>}
}
