use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::error::UsageError;
use crate::model::ClientInfo;

const INITIALIZE: &str = "initialize";
const INITIALIZED: &str = "initialized";
const RATE_LIMITS_READ: &str = "account/rateLimits/read";

pub(crate) struct JsonRpc<R, W> {
    reader: BufReader<R>,
    writer: W,
    next_id: u64,
}

impl<R, W> JsonRpc<R, W>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    pub(crate) fn new(reader: R, writer: W) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
            next_id: 1,
        }
    }

    pub(crate) async fn read_rate_limits(
        &mut self,
        client: &ClientInfo,
    ) -> Result<Value, UsageError> {
        let result = self
            .request(
                INITIALIZE,
                json!({
                    "clientInfo": {
                        "name": client.name,
                        "version": client.version,
                    }
                }),
            )
            .await?;
        let _ = result;
        self.notify(INITIALIZED, json!({})).await?;
        self.request(RATE_LIMITS_READ, json!({})).await
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, UsageError> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&json!({
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;

        loop {
            let frame = self.read_frame().await?;
            if let Some(server_method) = frame.get("method").and_then(Value::as_str) {
                if let Some(server_id) = frame.get("id") {
                    self.write(&json!({
                        "id": server_id,
                        "error": {
                            "code": -32601,
                            "message": format!("unsupported Codex server request {server_method}"),
                        }
                    }))
                    .await?;
                }
                continue;
            }

            let Some(response_id) = frame.get("id") else {
                continue;
            };
            if response_id.as_u64() != Some(id) {
                return Err(UsageError::protocol());
            }
            if frame.get("error").is_some() {
                return Err(UsageError::request_failed());
            }
            return Ok(frame.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), UsageError> {
        self.write(&json!({
            "method": method,
            "params": params,
        }))
        .await
    }

    async fn write(&mut self, frame: &Value) -> Result<(), UsageError> {
        let mut line = serde_json::to_vec(frame).map_err(|_| UsageError::protocol())?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .await
            .map_err(|_| UsageError::protocol())?;
        self.writer
            .flush()
            .await
            .map_err(|_| UsageError::protocol())
    }

    async fn read_frame(&mut self) -> Result<Value, UsageError> {
        let mut line = String::new();
        loop {
            line.clear();
            let read = self
                .reader
                .read_line(&mut line)
                .await
                .map_err(|_| UsageError::protocol())?;
            if read == 0 {
                return Err(UsageError::protocol());
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let frame: Value = serde_json::from_str(trimmed).map_err(|_| UsageError::protocol())?;
            if !frame.is_object() {
                return Err(UsageError::protocol());
            }
            return Ok(frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex, split};

    use super::{INITIALIZE, INITIALIZED, JsonRpc, RATE_LIMITS_READ};
    use crate::UsageErrorCode;
    use crate::model::ClientInfo;

    async fn write_frame(writer: &mut (impl tokio::io::AsyncWrite + Unpin), frame: &Value) {
        let mut line = serde_json::to_vec(frame).unwrap();
        line.push(b'\n');
        writer.write_all(&line).await.unwrap();
        writer.flush().await.unwrap();
    }

    async fn read_frame(reader: &mut BufReader<impl tokio::io::AsyncRead + Unpin>) -> Value {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(line.trim()).unwrap()
    }

    #[tokio::test]
    async fn handshake_only_reads_rate_limits() {
        let (client, server) = duplex(8192);
        let (client_read, client_write) = split(client);
        let (server_read, mut server_write) = split(server);
        let mut server_reader = BufReader::new(server_read);
        let client_info = ClientInfo {
            name: "opsail".to_owned(),
            version: "0.2.0".to_owned(),
        };

        let server = tokio::spawn(async move {
            let initialize = read_frame(&mut server_reader).await;
            assert_eq!(initialize["id"], 1);
            assert_eq!(initialize["method"], INITIALIZE);
            assert_eq!(
                initialize["params"]["clientInfo"],
                json!({ "name": "opsail", "version": "0.2.0" })
            );
            write_frame(
                &mut server_write,
                &json!({ "id": 1, "result": { "userAgent": "test" } }),
            )
            .await;

            let initialized = read_frame(&mut server_reader).await;
            assert_eq!(initialized, json!({ "method": INITIALIZED, "params": {} }));

            let read = read_frame(&mut server_reader).await;
            assert_eq!(read["method"], RATE_LIMITS_READ);
            write_frame(
                &mut server_write,
                &json!({
                    "id": read["id"],
                    "result": { "rateLimits": { "primary": { "usedPercent": 25, "resetsAt": 1_786_000_000u64 } } }
                }),
            )
            .await;
        });

        let mut rpc = JsonRpc::new(client_read, client_write);
        let result = rpc.read_rate_limits(&client_info).await.unwrap();
        server.await.unwrap();
        assert_eq!(
            result,
            json!({ "rateLimits": { "primary": { "usedPercent": 25, "resetsAt": 1_786_000_000u64 } } })
        );
    }

    #[tokio::test]
    async fn rpc_errors_never_copy_server_payloads() {
        let (client, server) = duplex(8192);
        let (client_read, client_write) = split(client);
        let (server_read, mut server_write) = split(server);
        let mut server_reader = BufReader::new(server_read);

        let server = tokio::spawn(async move {
            let initialize = read_frame(&mut server_reader).await;
            write_frame(
                &mut server_write,
                &json!({
                    "id": initialize["id"],
                    "error": { "message": "Bearer secret-token-should-not-leak" }
                }),
            )
            .await;
        });

        let mut rpc = JsonRpc::new(client_read, client_write);
        let error = rpc
            .read_rate_limits(&ClientInfo::default())
            .await
            .unwrap_err();
        server.await.unwrap();
        assert_eq!(error.code(), UsageErrorCode::RequestFailed);
        let rendered = format!("{error:?}{error}");
        assert!(!rendered.contains("secret-token"));
        assert!(!rendered.contains("Bearer"));
    }

    #[tokio::test]
    async fn unsupported_server_requests_are_rejected() {
        let (client, server) = duplex(8192);
        let (client_read, client_write) = split(client);
        let (server_read, mut server_write) = split(server);
        let mut server_reader = BufReader::new(server_read);

        let server = tokio::spawn(async move {
            let initialize = read_frame(&mut server_reader).await;
            write_frame(
                &mut server_write,
                &json!({ "id": 99, "method": "account/login/start", "params": {} }),
            )
            .await;
            let rejection = read_frame(&mut server_reader).await;
            assert_eq!(rejection["id"], 99);
            assert_eq!(rejection["error"]["code"], -32601);
            write_frame(
                &mut server_write,
                &json!({
                    "id": initialize["id"],
                    "result": { "rateLimits": { "primary": { "usedPercent": 10 } } }
                }),
            )
            .await;
            let _initialized = read_frame(&mut server_reader).await;
            let read = read_frame(&mut server_reader).await;
            write_frame(
                &mut server_write,
                &json!({
                    "id": read["id"],
                    "result": { "rateLimits": { "primary": { "usedPercent": 10 } } }
                }),
            )
            .await;
        });

        let mut rpc = JsonRpc::new(client_read, client_write);
        let result = rpc.read_rate_limits(&ClientInfo::default()).await.unwrap();
        server.await.unwrap();
        assert_eq!(result["rateLimits"]["primary"]["usedPercent"], 10);
    }
}
