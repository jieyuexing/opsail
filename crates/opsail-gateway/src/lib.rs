//! Encrypted connection storage and bounded, non-streaming API calls.
mod client;
#[cfg(test)]
mod tests;
mod types;
mod vault;

pub use types::*;
pub use vault::{Vault, default_data_dir};

use serde_json::json;

/// Dropping this future cancels an in-flight HTTP request. Vault writes run to
/// their atomic commit boundary even if a caller cancels while waiting.
pub async fn execute(
    vault: Vault,
    request: GatewayRequest,
    passphrase: Secret,
    new_passphrase: Option<Secret>,
) -> Result<GatewayResult, GatewayError> {
    if new_passphrase.is_some() && !matches!(request, GatewayRequest::Rekey) {
        return Err(GatewayError::new(
            "invalid-request",
            "input",
            "newPassphrase is only accepted for rekey",
        ));
    }
    let operation = request.operation().to_owned();
    if let Some(name) = request.connection_name() {
        let name = name.to_owned();
        let password = passphrase.clone();
        let connection = blocking(move || vault.get(&password, &name)).await?;
        return client::call(connection, request, &passphrase).await;
    }
    let data = blocking(move || match request {
        GatewayRequest::Init => {
            vault.init(&passphrase)?;
            Ok(json!({"initialized":true}))
        }
        GatewayRequest::List => Ok(json!(vault.list(&passphrase)?)),
        GatewayRequest::Set { connection } => {
            let summary = vault.set(&passphrase, connection)?;
            Ok(json!(summary))
        }
        GatewayRequest::SetDefaultModel { name, model } => {
            let summary = vault.set_default_model(&passphrase, &name, model)?;
            Ok(json!(summary))
        }
        GatewayRequest::Remove { name } => {
            vault.remove(&passphrase, &name)?;
            Ok(json!({"removed":true}))
        }
        GatewayRequest::Rekey => {
            let new =
                new_passphrase.ok_or_else(|| GatewayError::input("newPassphrase is required"))?;
            vault.rekey(&passphrase, &new)?;
            Ok(json!({"rekeyed":true}))
        }
        _ => Err(GatewayError::input("unsupported operation")),
    })
    .await?;
    Ok(GatewayResult {
        schema_version: 1,
        operation,
        connection: None,
        http_status: None,
        elapsed_ms: 0,
        data,
    })
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, GatewayError> + Send + 'static,
) -> Result<T, GatewayError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| GatewayError::vault("vault-worker-failed", "vault operation failed"))?
}
