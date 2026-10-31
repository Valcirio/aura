use crate::{AppError, Result};
use futures_util::StreamExt;
use serde_json::Value;

pub fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Aura/0.2 desktop")
        .build()
        .map_err(|_| AppError::new("HTTP_INIT", "Não foi possível iniciar o cliente de rede"))
}
pub fn network_error(error: reqwest::Error) -> AppError {
    if error.is_timeout() {
        AppError::new("NETWORK_TIMEOUT", "O serviço demorou a responder").retryable()
    } else {
        AppError::new(
            "SERVICE_UNREACHABLE",
            "Não foi possível conectar ao serviço",
        )
        .retryable()
    }
}
pub fn status_error(status: u16) -> AppError {
    match status {
        401 => AppError::new(
            "AUTH_REQUIRED",
            "Credencial inválida ou autorização expirada",
        ),
        403 => AppError::new(
            "ACCESS_DENIED",
            "Permissão insuficiente ou limite de uso do serviço",
        ),
        404 => AppError::new("NOT_FOUND", "Recurso não encontrado"),
        409 => AppError::new("CONFLICT", "Já existe um recurso com esse identificador"),
        429 => AppError::new(
            "RATE_LIMITED",
            "Limite de requisições atingido; tente novamente em alguns instantes",
        )
        .retryable(),
        500..=599 => AppError::new(
            "SERVICE_UNAVAILABLE",
            "Serviço temporariamente indisponível",
        )
        .retryable(),
        _ => AppError::new(
            "SERVICE_ERROR",
            format!("O serviço recusou a solicitação (HTTP {status})"),
        ),
    }
}
pub async fn bytes_limited(response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if !response.status().is_success() {
        return Err(status_error(response.status().as_u16()));
    }
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(AppError::new(
            "RESPONSE_TOO_LARGE",
            "Resposta maior que o limite permitido",
        ));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(network_error)?;
        if body.len() + chunk.len() > limit {
            return Err(AppError::new(
                "RESPONSE_TOO_LARGE",
                "Resposta maior que o limite permitido",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
pub async fn json(response: reqwest::Response) -> Result<Value> {
    Ok(serde_json::from_slice(
        &bytes_limited(response, 2_000_000).await?,
    )?)
}
