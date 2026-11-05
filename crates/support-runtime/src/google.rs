use crate::{AppError, Result, http, secrets::SecretStore};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use support_core::{
    calendar::{EventDraft, GOOGLE_CALENDAR, validate_interval},
    memory::Memory,
    store::{ConnectionInfo, Operation},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;

const TOKEN_KEY: &str = "google_calendar:tokens";
const SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/calendar.calendarlist.readonly",
    "https://www.googleapis.com/auth/calendar.events",
    "https://www.googleapis.com/auth/calendar.events.freebusy",
];
pub type Database = Arc<Mutex<Memory>>;
pub fn database(db: &Database) -> Result<std::sync::MutexGuard<'_, Memory>> {
    db.lock()
        .map_err(|_| AppError::new("MEMORY_UNAVAILABLE", "Memória local indisponível"))
}

#[derive(Clone, Serialize, Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: String,
    expires_at: i64,
    client_id: String,
    client_secret: String,
    account_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceStatus {
    pub connection: Option<ConnectionInfo>,
    pub connecting: bool,
    pub error: Option<AppError>,
    pub persistent_credentials: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct OAuthTicket {
    pub authorization_url: String,
}

pub struct GoogleClient {
    http: reqwest::Client,
    vault: Arc<SecretStore>,
    db: Database,
    tokens: tokio::sync::Mutex<Option<Tokens>>,
    pending: Mutex<Option<CancellationToken>>,
    error: Mutex<Option<AppError>>,
    persistent: std::sync::atomic::AtomicBool,
    auth_serial: tokio::sync::Mutex<()>,
}

impl GoogleClient {
    pub fn new(http: reqwest::Client, vault: Arc<SecretStore>, db: Database) -> Self {
        Self {
            http,
            vault,
            db,
            tokens: tokio::sync::Mutex::new(None),
            pending: Mutex::new(None),
            error: Mutex::new(None),
            persistent: std::sync::atomic::AtomicBool::new(false),
            auth_serial: tokio::sync::Mutex::new(()),
        }
    }
    pub fn status(&self) -> Result<ServiceStatus> {
        Ok(ServiceStatus {
            connection: database(&self.db)?.connection_info(GOOGLE_CALENDAR)?,
            connecting: self
                .pending
                .lock()
                .map_err(|_| AppError::new("AUTH_ERROR", "Autenticação indisponível"))?
                .is_some(),
            error: self.error.lock().ok().and_then(|e| e.clone()),
            persistent_credentials: self.persistent.load(std::sync::atomic::Ordering::Relaxed),
        })
    }
    pub async fn begin(
        self: &Arc<Self>,
        client_id: String,
        client_secret: String,
    ) -> Result<OAuthTicket> {
        if !client_id.ends_with(".apps.googleusercontent.com")
            || client_id.len() > 512
            || client_secret.len() > 1024
        {
            return Err(AppError::invalid(
                "Informe o client ID de um cliente OAuth Desktop do Google",
            ));
        }
        if database(&self.db)?
            .connection_info(GOOGLE_CALENDAR)?
            .is_some()
        {
            return Err(AppError::new(
                "ALREADY_CONNECTED",
                "Desconecte a conta atual antes de conectar outra",
            ));
        }
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| {
                AppError::new(
                    "OAUTH_CALLBACK_UNAVAILABLE",
                    "Não foi possível abrir o callback local de autenticação",
                )
            })?;
        let redirect = format!(
            "http://127.0.0.1:{}/oauth/callback",
            listener.local_addr()?.port()
        );
        let verifier = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = uuid::Uuid::new_v4().simple().to_string();
        let cancel = CancellationToken::new();
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| AppError::new("AUTH_ERROR", "Autenticação indisponível"))?;
            if pending.is_some() {
                return Err(AppError::new(
                    "AUTH_IN_PROGRESS",
                    "Já existe uma autenticação em andamento",
                ));
            }
            *pending = Some(cancel.clone());
        }
        if let Ok(mut error) = self.error.lock() {
            *error = None;
        }
        let mut url =
            url::Url::parse("https://accounts.google.com/o/oauth2/v2/auth").expect("URL estática");
        url.query_pairs_mut().extend_pairs([
            ("client_id", client_id.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("response_type", "code"),
            ("scope", &SCOPES.join(" ")),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("state", &state),
            ("access_type", "offline"),
            ("prompt", "consent"),
        ]);
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let result = tokio::select! {biased;_=cancel.cancelled()=>Err(AppError::cancelled()),r=tokio::time::timeout(Duration::from_secs(180),this.complete(listener,&state,&redirect,&verifier,&client_id,&client_secret))=>r.unwrap_or_else(|_|Err(AppError::new("OAUTH_TIMEOUT","Tempo de autenticação esgotado")))};
            if let Err(error) = result {
                if let Ok(mut last) = this.error.lock() {
                    *last = Some(error);
                }
            }
            if let Ok(mut pending) = this.pending.lock() {
                *pending = None;
            }
        });
        Ok(OAuthTicket {
            authorization_url: url.to_string(),
        })
    }
    async fn complete(
        &self,
        listener: TcpListener,
        state: &str,
        redirect: &str,
        verifier: &str,
        client_id: &str,
        client_secret: &str,
    ) -> Result<()> {
        let _guard = self.auth_serial.lock().await;
        let code = loop {
            let (mut socket, _) = listener.accept().await?;
            let mut bytes = vec![0; 8192];
            let read =
                match tokio::time::timeout(Duration::from_secs(3), socket.read(&mut bytes)).await {
                    Ok(Ok(n)) => n,
                    _ => continue,
                };
            let request = String::from_utf8_lossy(&bytes[..read]);
            let target = request
                .lines()
                .next()
                .and_then(|line| line.strip_prefix("GET "))
                .and_then(|line| line.strip_suffix(" HTTP/1.1"));
            let parsed = target
                .and_then(|target| url::Url::parse(&format!("http://127.0.0.1{target}")).ok());
            let result = parsed.as_ref().map(|u| validate_callback(u, state));
            match result {
                Some(Ok(code)) => {
                    let _=socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\nContent-Security-Policy: default-src 'none'\r\n\r\n<h1>Autorizacao recebida</h1><p>Volte ao Aura para concluir a conexao.</p>").await;
                    break code;
                }
                Some(Err(error)) if error.code == "OAUTH_DENIED" => {
                    let _=socket.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nAutorizacao cancelada. Volte ao Aura.").await;
                    return Err(error);
                }
                _ => {
                    let _=socket.write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\nCallback invalido").await;
                }
            }
        };
        let mut form = vec![
            ("client_id", client_id),
            ("code", code.as_str()),
            ("code_verifier", verifier),
            ("redirect_uri", redirect),
            ("grant_type", "authorization_code"),
        ];
        if !client_secret.is_empty() {
            form.push(("client_secret", client_secret));
        }
        let value = http::json(
            self.http
                .post("https://oauth2.googleapis.com/token")
                .form(&form)
                .send()
                .await
                .map_err(http::network_error)?,
        )
        .await?;
        let scopes = value["scope"]
            .as_str()
            .unwrap_or("")
            .split_whitespace()
            .collect::<Vec<_>>();
        if !SCOPES.iter().all(|scope| scopes.contains(scope)) {
            return Err(AppError::new(
                "OAUTH_SCOPES",
                "O Google não concedeu todas as permissões solicitadas",
            ));
        }
        let access = value["access_token"]
            .as_str()
            .ok_or_else(|| AppError::new("OAUTH_TOKEN", "Token de acesso ausente"))?
            .to_string();
        let refresh = value["refresh_token"]
            .as_str()
            .ok_or_else(|| {
                AppError::new(
                    "OAUTH_TOKEN",
                    "Autorização sem refresh token. Reconecte a conta.",
                )
            })?
            .to_string();
        let calendars = http::json(
            self.http
                .get("https://www.googleapis.com/calendar/v3/users/me/calendarList")
                .query(&[("fields", "items(id,summary,primary),nextPageToken")])
                .bearer_auth(&access)
                .send()
                .await
                .map_err(http::network_error)?,
        )
        .await?;
        let primary = calendars["items"]
            .as_array()
            .and_then(|items| items.iter().find(|c| c["primary"] == true))
            .ok_or_else(|| {
                AppError::new(
                    "CALENDAR_ACCOUNT",
                    "Não foi possível identificar a agenda principal da conta",
                )
            })?;
        let account = primary["id"]
            .as_str()
            .ok_or_else(|| AppError::new("CALENDAR_ACCOUNT", "Conta sem identificador"))?
            .to_string();
        let tokens = Tokens {
            access_token: access,
            refresh_token: refresh,
            expires_at: crate::now() + value["expires_in"].as_i64().unwrap_or(3600),
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            account_id: account.clone(),
        };
        let persistent = self
            .vault
            .set(TOKEN_KEY, &serde_json::to_string(&tokens)?)
            .await?;
        database(&self.db)?.save_connection(GOOGLE_CALENDAR, &account, &account)?;
        *self.tokens.lock().await = Some(tokens);
        self.persistent
            .store(persistent, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
    async fn access_token(&self, force: bool) -> Result<String> {
        let _guard = self.auth_serial.lock().await;
        let connection = database(&self.db)?
            .connection_info(GOOGLE_CALENDAR)?
            .ok_or_else(|| {
                AppError::new("AUTH_REQUIRED", "Conecte sua conta Google em Integrações")
            })?;
        let mut tokens = self.tokens.lock().await;
        if tokens.is_none() {
            if let Some(stored) = self.vault.get(TOKEN_KEY).await? {
                *tokens = serde_json::from_str(&stored).ok();
                self.persistent
                    .store(tokens.is_some(), std::sync::atomic::Ordering::Relaxed);
            }
        }
        let bundle = tokens
            .as_mut()
            .filter(|t| t.account_id == connection.account_id)
            .ok_or_else(|| {
                AppError::new(
                    "AUTH_REQUIRED",
                    "Reconecte a conta Google para renovar a autorização",
                )
            })?;
        if !force && bundle.expires_at > crate::now() + 60 {
            return Ok(bundle.access_token.clone());
        }
        let mut form = vec![
            ("client_id", bundle.client_id.as_str()),
            ("refresh_token", bundle.refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ];
        if !bundle.client_secret.is_empty() {
            form.push(("client_secret", bundle.client_secret.as_str()));
        }
        let response = self
            .http
            .post("https://oauth2.googleapis.com/token")
            .form(&form)
            .send()
            .await
            .map_err(http::network_error)?;
        if response.status().as_u16() == 400 {
            return Err(AppError::new(
                "AUTH_REQUIRED",
                "A autorização Google expirou ou foi revogada. Desconecte e reconecte a conta.",
            ));
        }
        let value = http::json(response).await?;
        bundle.access_token = value["access_token"]
            .as_str()
            .ok_or_else(|| AppError::new("OAUTH_TOKEN", "Token renovado inválido"))?
            .into();
        bundle.expires_at = crate::now() + value["expires_in"].as_i64().unwrap_or(3600);
        let persistent = self
            .vault
            .set(TOKEN_KEY, &serde_json::to_string(bundle)?)
            .await?;
        self.persistent
            .store(persistent, std::sync::atomic::Ordering::Relaxed);
        Ok(bundle.access_token.clone())
    }
    pub async fn disconnect(&self) -> Result<bool> {
        if let Some(cancel) = self.pending.lock().ok().and_then(|p| p.clone()) {
            cancel.cancel();
        }
        let _guard = self.auth_serial.lock().await;
        let mut tokens = self.tokens.lock().await;
        let stored = tokens.take().or_else(|| None);
        let raw = if let Some(t) = stored {
            Some(t.refresh_token)
        } else {
            self.vault
                .get(TOKEN_KEY)
                .await?
                .and_then(|s| serde_json::from_str::<Tokens>(&s).ok())
                .map(|t| t.refresh_token)
        };
        database(&self.db)?.remove_connection(GOOGLE_CALENDAR)?;
        self.vault.remove(TOKEN_KEY).await?;
        self.persistent
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(token) = raw {
            return Ok(self
                .http
                .post("https://oauth2.googleapis.com/revoke")
                .timeout(Duration::from_secs(8))
                .form(&[("token", token)])
                .send()
                .await
                .is_ok_and(|r| r.status().is_success()));
        }
        Ok(false)
    }
    async fn request(
        &self,
        method: reqwest::Method,
        segments: &[&str],
        query: &[(String, String)],
        body: Option<Value>,
    ) -> Result<Value> {
        let mut url =
            url::Url::parse("https://www.googleapis.com/calendar/v3/").expect("URL estática");
        {
            let mut path = url.path_segments_mut().expect("URL hierárquica");
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        url.query_pairs_mut()
            .extend_pairs(query.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let mut token = self.access_token(false).await?;
        for attempt in 0..2 {
            let mut request = self
                .http
                .request(method.clone(), url.clone())
                .bearer_auth(&token);
            if let Some(body) = &body {
                request = request.json(body);
            }
            let response = request.send().await.map_err(http::network_error)?;
            if response.status().as_u16() == 401 && attempt == 0 {
                token = self.access_token(true).await?;
                continue;
            }
            return http::json(response).await;
        }
        Err(AppError::new("AUTH_REQUIRED", "Reconecte sua conta Google"))
    }
}

fn validate_callback(url: &url::Url, state: &str) -> Result<String> {
    let fields = url
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    if url.path() != "/oauth/callback" || fields.get("state").map(|s| s.as_ref()) != Some(state) {
        return Err(AppError::new(
            "OAUTH_STATE",
            "Resposta de autenticação inválida",
        ));
    }
    if fields.contains_key("error") {
        return Err(AppError::new("OAUTH_DENIED", "Autorização não concedida"));
    }
    let code = fields
        .get("code")
        .filter(|c| !c.is_empty() && c.len() < 4096)
        .ok_or_else(|| AppError::new("OAUTH_CODE", "Código de autenticação ausente"))?;
    Ok(code.to_string())
}

#[async_trait]
pub trait CalendarApi: Send + Sync {
    async fn calendars(&self) -> Result<Value>;
    async fn events(
        &self,
        calendar: &str,
        start: &str,
        end: &str,
        query: &str,
        page: Option<&str>,
    ) -> Result<Value>;
    async fn free_busy(&self, calendars: &[String], start: &str, end: &str) -> Result<Value>;
    async fn create(&self, event: &EventDraft, id: &str) -> Result<Value>;
    async fn get_event(&self, calendar: &str, id: &str) -> Result<Value>;
}
#[async_trait]
impl CalendarApi for GoogleClient {
    async fn calendars(&self) -> Result<Value> {
        let mut all = Vec::new();
        let mut page = None;
        for _ in 0..20 {
            let mut query = vec![
                ("maxResults".into(), "250".into()),
                (
                    "fields".into(),
                    "items(id,summary,primary,accessRole,timeZone),nextPageToken".into(),
                ),
            ];
            if let Some(token) = page {
                query.push(("pageToken".into(), token));
            }
            let value = self
                .request(
                    reqwest::Method::GET,
                    &["users", "me", "calendarList"],
                    &query,
                    None,
                )
                .await?;
            if let Some(items) = value["items"].as_array() {
                all.extend(items.iter().cloned());
            }
            page = value["nextPageToken"].as_str().map(String::from);
            if page.is_none() {
                return Ok(json!({"items":all}));
            }
        }
        Err(AppError::new(
            "PAGINATION_LIMIT",
            "Há muitas agendas para carregar de uma vez",
        ))
    }
    async fn events(
        &self,
        calendar: &str,
        start: &str,
        end: &str,
        query: &str,
        page: Option<&str>,
    ) -> Result<Value> {
        validate_interval(start, end)?;
        let mut args = vec![
            ("timeMin".into(), start.into()),
            ("timeMax".into(), end.into()),
            ("singleEvents".into(), "true".into()),
            ("orderBy".into(), "startTime".into()),
            ("maxResults".into(), "30".into()),
            (
                "fields".into(),
                "items(id,summary,start,end,status,htmlLink),nextPageToken,timeZone".into(),
            ),
        ];
        if !query.is_empty() {
            args.push(("q".into(), query.into()));
        }
        if let Some(page) = page {
            args.push(("pageToken".into(), page.into()));
        }
        self.request(
            reqwest::Method::GET,
            &["calendars", calendar, "events"],
            &args,
            None,
        )
        .await
    }
    async fn free_busy(&self, calendars: &[String], start: &str, end: &str) -> Result<Value> {
        validate_interval(start, end)?;
        if calendars.is_empty() || calendars.len() > 10 {
            return Err(AppError::invalid("Escolha de 1 a 10 agendas"));
        }
        let value=self.request(reqwest::Method::POST,&["freeBusy"],&[],Some(json!({"timeMin":start,"timeMax":end,"items":calendars.iter().map(|id|json!({"id":id})).collect::<Vec<_>>()}))).await?;
        if value["calendars"].as_object().is_none_or(|map| {
            map.len() != calendars.len()
                || map
                    .values()
                    .any(|v| v["errors"].as_array().is_some_and(|e| !e.is_empty()))
        }) {
            return Err(AppError::new(
                "CALENDAR_PARTIAL_FAILURE",
                "Não foi possível verificar a disponibilidade de todas as agendas",
            ));
        }
        Ok(value)
    }
    async fn create(&self, event: &EventDraft, id: &str) -> Result<Value> {
        self.request(
            reqwest::Method::POST,
            &["calendars", &event.calendar_id, "events"],
            &[],
            Some(event.api_body(id)),
        )
        .await
    }
    async fn get_event(&self, calendar: &str, id: &str) -> Result<Value> {
        self.request(
            reqwest::Method::GET,
            &["calendars", calendar, "events", id],
            &[],
            None,
        )
        .await
    }
}

pub async fn ensure_writable(api: &dyn CalendarApi, id: &str) -> Result<()> {
    let calendars = api.calendars().await?;
    let calendar = calendars["items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|v| v["id"] == id || (id == "primary" && v["primary"] == true))
        })
        .ok_or_else(|| {
            AppError::new(
                "CALENDAR_NOT_FOUND",
                "Agenda não encontrada na conta conectada",
            )
        })?;
    if !["owner", "writer"].contains(&calendar["accessRole"].as_str().unwrap_or("")) {
        return Err(AppError::new(
            "CALENDAR_READ_ONLY",
            "Você não tem permissão para criar eventos nessa agenda",
        ));
    }
    Ok(())
}
pub async fn busy_for(api: &dyn CalendarApi, event: &EventDraft) -> Result<Value> {
    let (start, end) = event.interval()?;
    let value = api
        .free_busy(&[event.calendar_id.clone()], &start, &end)
        .await?;
    Ok(
        json!({"busy":value["calendars"].as_object().into_iter().flat_map(|map|map.values()).filter_map(|v|v["busy"].as_array()).flatten().cloned().collect::<Vec<_>>()}),
    )
}
pub async fn confirm_operation(
    db: &Database,
    api: &dyn CalendarApi,
    id: &str,
    session: i64,
    account: &str,
) -> Result<Operation> {
    let operation = database(db)?.operation(id)?;
    if operation.session_id != Some(session) || operation.account_id != account {
        return Err(AppError::new(
            "ACTION_NOT_CONFIRMABLE",
            "A proposta pertence a outra sessão ou conta",
        ));
    }
    if operation.status == "completed" {
        return Ok(operation);
    }
    if operation.status == "unknown" {
        return reconcile_operation(db, api, id, account).await;
    }
    if operation.status != "awaiting_confirmation" || operation.expires_at <= crate::now() {
        return Err(AppError::new(
            "ACTION_NOT_CONFIRMABLE",
            "Proposta expirada ou já processada. Prepare uma nova proposta.",
        ));
    }
    let event: EventDraft = operation.payload_as()?;
    ensure_writable(api, &event.calendar_id).await?;
    let busy = busy_for(api, &event).await?;
    if operation.result.as_ref() != Some(&busy) {
        database(db)?.operation_context(id, &busy)?;
        return Err(AppError::new(
            "AVAILABILITY_CHANGED",
            "A disponibilidade mudou. Confira os conflitos e confirme novamente.",
        ));
    }
    let operation = database(db)?.claim_operation(id, session, account, crate::now())?;
    match api.create(&event, &operation.id).await {
        Ok(event) if event["id"]==id => database(db)?.finish_operation(id, "completed", Some(&event)),
        Ok(_) => database(db)?.finish_operation(id,"unknown",Some(&json!({"error":"O serviço não retornou o identificador esperado; verifique o resultado antes de tentar novamente"}))),
        Err(error) if error.code == "CONFLICT" => reconcile_operation(db, api, id, account).await,
        Err(error) => {
            let uncertain = error.retryable || error.code == "STREAM_TRUNCATED";
            let result = json!({"error":error});
            database(db)?.finish_operation(
                id,
                if uncertain { "unknown" } else { "failed" },
                Some(&result),
            )
        }
    }
}
pub async fn reconcile_operation(
    db: &Database,
    api: &dyn CalendarApi,
    id: &str,
    account: &str,
) -> Result<Operation> {
    let op = database(db)?.operation(id)?;
    if op.account_id != account {
        return Err(AppError::new(
            "ACTION_ACCOUNT",
            "Reconecte a conta original para verificar esta operação",
        ));
    }
    let event: EventDraft = op.payload_as()?;
    match api.get_event(&event.calendar_id, id).await {
        Ok(value) if value["status"] == "cancelled" => database(db)?.finish_operation(
            id,
            "failed",
            Some(&json!({"error":"O evento foi cancelado no Google Calendar"})),
        ),
        Ok(value) if value["extendedProperties"]["private"]["auraOperationId"] == id => {
            database(db)?.finish_operation(id, "completed", Some(&value))
        }
        Ok(_) => database(db)?.finish_operation(
            id,
            "failed",
            Some(&json!({"error":"Identificador já utilizado por outro evento"})),
        ),
        Err(error) => database(db)?.finish_operation(id, "unknown", Some(&json!({"error":error}))),
    }
}

pub async fn retry_operation(
    db: &Database,
    api: &dyn CalendarApi,
    id: &str,
    session: i64,
    account: &str,
) -> Result<Operation> {
    let op = database(db)?.operation(id)?;
    if op.account_id != account || op.session_id != Some(session) || op.status != "unknown" {
        return Err(AppError::new(
            "ACTION_NOT_CONFIRMABLE",
            "Tentativa inválida para essa conta ou sessão",
        ));
    }
    let event: EventDraft = op.payload_as()?;
    match api.get_event(&event.calendar_id, id).await {
        Err(error) if error.code == "NOT_FOUND" => {
            ensure_writable(api, &event.calendar_id).await?;
            let busy = busy_for(api, &event).await?;
            database(db)?.retry_operation(id, session, account, &busy, crate::now())
        }
        Ok(_) => reconcile_operation(db, api, id, account).await,
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_foreign_oauth_state_and_denied_consent() {
        let url = url::Url::parse("http://127.0.0.1/oauth/callback?state=wrong&code=x").unwrap();
        assert!(validate_callback(&url, "expected").is_err());
        let url =
            url::Url::parse("http://127.0.0.1/oauth/callback?state=expected&error=access_denied")
                .unwrap();
        assert_eq!(
            validate_callback(&url, "expected").unwrap_err().code,
            "OAUTH_DENIED"
        );
    }
    struct FakeApi {
        writes: std::sync::atomic::AtomicUsize,
        timeout: bool,
        found: bool,
    }
    #[async_trait]
    impl CalendarApi for FakeApi {
        async fn calendars(&self) -> Result<Value> {
            Ok(json!({"items":[{"id":"primary","primary":true,"accessRole":"owner"}]}))
        }
        async fn events(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &str,
            _: Option<&str>,
        ) -> Result<Value> {
            Ok(json!({"items":[]}))
        }
        async fn free_busy(&self, _: &[String], _: &str, _: &str) -> Result<Value> {
            Ok(json!({"calendars":{"primary":{"busy":[]}}}))
        }
        async fn create(&self, event: &EventDraft, id: &str) -> Result<Value> {
            self.writes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.timeout {
                Err(AppError::new("NETWORK_TIMEOUT", "timeout").retryable())
            } else {
                Ok(event.api_body(id))
            }
        }
        async fn get_event(&self, _: &str, id: &str) -> Result<Value> {
            if !self.found {
                return Err(AppError::new("NOT_FOUND", "Evento não encontrado"));
            }
            Ok(json!({"id":id,"extendedProperties":{"private":{"auraOperationId":id}}}))
        }
    }
    fn fixture() -> (Database, i64, EventDraft) {
        let m = Memory::ephemeral().unwrap();
        let s = m.create_session("s").unwrap();
        let d = EventDraft::validate(support_core::calendar::EventInput {
            calendar_id: "primary".into(),
            summary: "s".into(),
            start: "2026-10-30T14:00:00-03:00".into(),
            end: "2026-10-30T15:00:00-03:00".into(),
            time_zone: "America/Bahia".into(),
            all_day: false,
            description: String::new(),
        })
        .unwrap();
        (Arc::new(Mutex::new(m)), s.id, d)
    }
    #[tokio::test]
    async fn confirmation_is_atomic_and_timeout_reconciles_without_second_insert() {
        for timeout in [false, true] {
            let (db, session, draft) = fixture();
            let api = FakeApi {
                writes: std::sync::atomic::AtomicUsize::new(0),
                timeout,
                found: true,
            };
            let op = database(&db)
                .unwrap()
                .prepare_calendar_event(session, "a", &draft, crate::now())
                .unwrap();
            database(&db)
                .unwrap()
                .operation_context(&op.id, &json!({"busy":[]}))
                .unwrap();
            assert_eq!(api.writes.load(std::sync::atomic::Ordering::SeqCst), 0);
            let first = confirm_operation(&db, &api, &op.id, session, "a")
                .await
                .unwrap();
            assert_eq!(first.status, if timeout { "unknown" } else { "completed" });
            let second = confirm_operation(&db, &api, &op.id, session, "a")
                .await
                .unwrap();
            assert_eq!(second.status, "completed");
            assert_eq!(api.writes.load(std::sync::atomic::Ordering::SeqCst), 1);
        }
    }
    #[tokio::test]
    async fn retry_requires_confirmation_again_and_preserves_event_identifier() {
        let (db, session, draft) = fixture();
        let api = FakeApi {
            writes: std::sync::atomic::AtomicUsize::new(0),
            timeout: true,
            found: false,
        };
        let op = database(&db)
            .unwrap()
            .prepare_calendar_event(session, "a", &draft, crate::now())
            .unwrap();
        database(&db)
            .unwrap()
            .operation_context(&op.id, &json!({"busy":[]}))
            .unwrap();
        let unknown = confirm_operation(&db, &api, &op.id, session, "a")
            .await
            .unwrap();
        assert_eq!(unknown.status, "unknown");
        let retry = retry_operation(&db, &api, &op.id, session, "a")
            .await
            .unwrap();
        assert_eq!(retry.id, op.id);
        assert_eq!(retry.status, "awaiting_confirmation");
        assert_eq!(api.writes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn changed_availability_and_foreign_confirmation_cannot_create() {
        let (db, session, draft) = fixture();
        let api = FakeApi {
            writes: std::sync::atomic::AtomicUsize::new(0),
            timeout: false,
            found: true,
        };
        let op = database(&db)
            .unwrap()
            .prepare_calendar_event(session, "a", &draft, crate::now())
            .unwrap();
        assert!(
            confirm_operation(&db, &api, &op.id, session, "b")
                .await
                .is_err()
        );
        database(&db)
            .unwrap()
            .operation_context(&op.id, &json!({"busy":[{"start":"previous"}]}))
            .unwrap();
        let error = confirm_operation(&db, &api, &op.id, session, "a")
            .await
            .unwrap_err();
        assert_eq!(error.code, "AVAILABILITY_CHANGED");
        assert_eq!(
            database(&db).unwrap().operation(&op.id).unwrap().status,
            "awaiting_confirmation"
        );
        assert_eq!(api.writes.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
