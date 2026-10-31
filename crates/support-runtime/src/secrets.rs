use crate::{AppError, Result};
use std::{collections::HashMap, sync::Mutex, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

/// Nunca persiste segredos no SQLite/settings. Sem Secret Service, usa somente RAM.
#[derive(Default)]
pub struct SecretStore {
    cache: Mutex<HashMap<String, String>>,
}
impl SecretStore {
    pub async fn get(&self, key: &str) -> Result<Option<String>> {
        if let Some(v) = self
            .cache
            .lock()
            .map_err(|_| AppError::new("VAULT_ERROR", "Cofre indisponível"))?
            .get(key)
            .cloned()
        {
            return Ok(Some(v));
        }
        let command = Command::new("secret-tool")
            .args(["lookup", "application", "aura", "key", key])
            .kill_on_drop(true)
            .output();
        if let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(3), command).await {
            if output.status.success() && output.stdout.len() < 65536 {
                let text = String::from_utf8(output.stdout)
                    .unwrap_or_default()
                    .trim_end_matches('\n')
                    .to_string();
                if !text.is_empty() {
                    self.cache
                        .lock()
                        .map_err(|_| AppError::new("VAULT_ERROR", "Cofre indisponível"))?
                        .insert(key.into(), text.clone());
                    return Ok(Some(text));
                }
            }
        }
        Ok(None)
    }
    pub async fn set(&self, key: &str, value: &str) -> Result<bool> {
        if value.len() > 32768 {
            return Err(AppError::invalid("Credencial muito longa"));
        }
        self.cache
            .lock()
            .map_err(|_| AppError::new("VAULT_ERROR", "Cofre indisponível"))?
            .insert(key.into(), value.into());
        let operation = async {
            let mut child = Command::new("secret-tool")
                .args(["store", "--label=Aura", "application", "aura", "key", key])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()?;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(value.as_bytes()).await?;
                stdin.shutdown().await?;
            }
            Ok::<bool, std::io::Error>(child.wait().await?.success())
        };
        Ok(matches!(
            tokio::time::timeout(Duration::from_secs(3), operation).await,
            Ok(Ok(true))
        ))
    }
    pub async fn remove(&self, key: &str) -> Result<()> {
        self.cache
            .lock()
            .map_err(|_| AppError::new("VAULT_ERROR", "Cofre indisponível"))?
            .remove(key);
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            Command::new("secret-tool")
                .args(["clear", "application", "aura", "key", key])
                .kill_on_drop(true)
                .output(),
        )
        .await;
        // Revogação remota e remoção local são independentes; reporta falha se o cofre existe mas recusou limpar.
        if let Ok(Ok(output)) = result {
            if !output.status.success() && output.status.code() != Some(1) {
                return Err(AppError::new(
                    "VAULT_ERROR",
                    "Não foi possível limpar o cofre do sistema",
                ));
            }
        }
        Ok(())
    }
}
