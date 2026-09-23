use std::{collections::BTreeMap, fs, time::Duration};

use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};

use crate::*;

fn password() -> Secret {
    Secret::new("fixture-master-passphrase".into())
}
fn connection(base: &str) -> Connection {
    Connection {
        name: "test".into(),
        adapter: Adapter::OpenaiCompatible,
        base_url: base.into(),
        auth: Auth::Bearer {
            key: Secret::new("fixture-api-key".into()),
        },
        default_model: Some("fixture-model".into()),
        allow_http: false,
    }
}
fn request(path: &str) -> GatewayRequest {
    GatewayRequest::Request {
        connection: "test".into(),
        method: "GET".into(),
        path: path.into(),
        query: BTreeMap::new(),
        headers: BTreeMap::new(),
        body: None,
        timeout_ms: None,
    }
}
fn from_json(value: Value) -> GatewayRequest {
    serde_json::from_value(value).unwrap()
}

#[test]
fn vault_roundtrip_rekey_and_failed_unlock_preserve_ciphertext() {
    let temp = tempfile::tempdir().unwrap();
    let vault = Vault::new(temp.path().join("gateway"));
    vault.init(&password()).unwrap();
    vault
        .set(&password(), connection("https://example.test/v1"))
        .unwrap();
    let before = fs::read(vault.path()).unwrap();
    assert!(before.starts_with(b"age-encryption.org/v1"));
    for secret in [
        "fixture-api-key",
        "fixture-master-passphrase",
        "example.test",
    ] {
        assert!(!String::from_utf8_lossy(&before).contains(secret));
    }
    let list = vault.list(&password()).unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].has_key);
    assert!(
        !serde_json::to_string(&list)
            .unwrap()
            .contains("fixture-api-key")
    );
    assert!(!format!("{:?}", connection("https://example.test")).contains("fixture-api-key"));
    let wrong = Secret::new("wrong".into());
    assert_eq!(
        vault
            .set(&wrong, connection("https://other.test"))
            .unwrap_err()
            .code,
        "vault-unlock-failed"
    );
    assert!(vault.init(&password()).is_err());
    assert_eq!(fs::read(vault.path()).unwrap(), before);
    let new = Secret::new("replacement-passphrase".into());
    vault.rekey(&password(), &new).unwrap();
    assert!(vault.list(&password()).is_err());
    let reopened = Vault::new(temp.path().join("gateway"));
    assert_eq!(
        reopened.get(&new, "test").unwrap().auth.key(),
        Some("fixture-api-key")
    );
    reopened.remove(&new, "test").unwrap();
    assert!(reopened.list(&new).unwrap().is_empty());
    assert_eq!(
        reopened.remove(&new, "test").unwrap_err().code,
        "connection-not-found"
    );
}

#[test]
fn damaged_vault_and_concurrent_writer_do_not_lose_data() {
    use fs2::FileExt;
    let temp = tempfile::tempdir().unwrap();
    let vault = Vault::new(temp.path().into());
    vault.init(&password()).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(temp.path().join("vault.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    assert_eq!(
        vault
            .set(&password(), connection("https://example.test"))
            .unwrap_err()
            .code,
        "vault-busy"
    );
    drop(lock);
    vault
        .set(&password(), connection("https://example.test"))
        .unwrap();
    let mut other = connection("https://other.test");
    other.name = "second".into();
    Vault::new(temp.path().into())
        .set(&password(), other)
        .unwrap();
    assert_eq!(vault.list(&password()).unwrap().len(), 2);
    let mut damaged = fs::read(vault.path()).unwrap();
    *damaged.last_mut().unwrap() ^= 1;
    fs::write(vault.path(), &damaged).unwrap();
    assert!(vault.remove(&password(), "test").is_err());
    assert_eq!(fs::read(vault.path()).unwrap(), damaged);
}

#[cfg(unix)]
#[test]
fn vault_symlinks_and_failed_write_leave_original_untouched() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("gateway");
    let vault = Vault::new(dir.clone());
    vault.init(&password()).unwrap();
    assert_eq!(
        fs::metadata(vault.path()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let original = fs::read(vault.path()).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    let result = vault.set(&password(), connection("https://example.test"));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert_eq!(fs::read(vault.path()).unwrap(), original);
    let outside = temp.path().join("outside.age");
    fs::rename(vault.path(), &outside).unwrap();
    symlink(&outside, vault.path()).unwrap();
    assert_eq!(
        vault.rekey(&password(), &password()).unwrap_err().code,
        "unsafe-vault-path"
    );
    assert_eq!(fs::read(outside).unwrap(), original);
}

#[tokio::test]
async fn prefix_query_auth_and_text_body_are_preserved() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/echo"))
        .and(query_param("q", "a b"))
        .and(header("api-key", "fixture-api-key"))
        .and(header("x-test", "present"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"echo":"fixture-api-key","content":"ok","usage":{"total_tokens":3}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let mut c = connection(&format!("{}/v1", server.uri()));
    c.auth = Auth::Header {
        name: "api-key".into(),
        key: Secret::new("fixture-api-key".into()),
    };
    let result = crate::client::call(
        c,
        from_json(
            json!({"operation":"request","connection":"test","method":"POST","path":"/echo",
        "query":{"q":"a b"},"headers":{"x-test":"present"},"body":{"type":"text","value":"hello"}}),
        ),
        &password(),
    )
    .await
    .unwrap();
    assert_eq!(result.http_status, Some(200));
    assert_eq!(result.data["echo"], "[REDACTED]");
    assert_eq!(result.data["usage"]["total_tokens"], 3);
    assert_eq!(server.received_requests().await.unwrap()[0].body, b"hello");
}

#[tokio::test]
async fn chat_and_local_model_discovery_preserve_vendor_fields() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"local-model"}]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut c = connection(&format!("{}/v1", server.uri()));
    c.auth = Auth::None;
    crate::client::call(
        c.clone(),
        from_json(json!({"operation":"models","connection":"test"})),
        &password(),
    )
    .await
    .unwrap();
    assert!(
        !server.received_requests().await.unwrap()[0]
            .headers
            .contains_key("authorization")
    );
    Mock::given(path("/v1/chat/completions")).and(body_json(json!({"model":"fixture-model","messages":[{"role":"user","content":"Hi"}],"stream":false,"thinking":{"type":"disabled"}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices":[{"message":{"content":"hello","reasoning_content":"reason","tool_calls":[]}}],"usage":{"prompt_tokens":1}})))
        .expect(1).mount(&server).await;
    let response = crate::client::call(c, from_json(json!({"operation":"chat","connection":"test","messages":[{"role":"user","content":"Hi"}],"parameters":{"thinking":{"type":"disabled"}}})), &password()).await.unwrap();
    assert_eq!(
        response.data["choices"][0]["message"]["reasoning_content"],
        "reason"
    );
}

#[tokio::test]
async fn invalid_input_is_rejected_before_any_http_request() {
    let server = MockServer::start().await;
    let c = connection(&format!("{}/v1", server.uri()));
    for target in [
        "https://elsewhere.test",
        "//elsewhere.test",
        "../escape",
        "%2e%2e/escape",
        "x?query=wrong",
        "x#fragment",
        "\\escape",
    ] {
        assert!(
            crate::client::call(c.clone(), request(target), &password())
                .await
                .is_err()
        );
    }
    for name in ["Authorization", "HOST", "Content-Length", "api-key"] {
        let r = from_json(
            json!({"operation":"request","connection":"test","method":"GET","path":"x","headers":{name:"override"}}),
        );
        assert!(
            crate::client::call(c.clone(), r, &password())
                .await
                .is_err()
        );
    }
    for parameters in [
        json!({"stream":true}),
        json!({"model":"other"}),
        json!({"messages":[]}),
    ] {
        assert!(crate::client::call(c.clone(), from_json(json!({"operation":"chat","connection":"test","messages":[{"role":"user","content":"hi"}],"parameters":parameters})), &password()).await.is_err());
    }
    let big = from_json(
        json!({"operation":"request","connection":"test","method":"POST","path":"x","body":{"type":"text","value":"x".repeat(1024*1024+1)}}),
    );
    assert!(crate::client::call(c, big, &password()).await.is_err());
    assert!(server.received_requests().await.unwrap().is_empty());
    for base in [
        "http://example.test",
        "https://user:pass@example.test",
        "https://example.test?key=x",
        "file:///local",
    ] {
        assert!(crate::client::validate_connection(&connection(base)).is_err());
    }
    let mut lan = connection("http://192.168.1.8:8080/v1");
    lan.allow_http = true;
    assert!(crate::client::validate_connection(&lan).is_ok());
}

#[tokio::test]
async fn provider_errors_redirects_and_retries_are_bounded() {
    let server = MockServer::start().await;
    let target = MockServer::start().await;
    for status in [401, 403, 429, 500, 503] {
        server.reset().await;
        Mock::given(path("/failure"))
            .respond_with(ResponseTemplate::new(status).set_body_json(
                json!({"error":{"code":"provider_failure","message":"fixture-api-key"}}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let error = crate::client::call(connection(&server.uri()), request("failure"), &password())
            .await
            .unwrap_err();
        assert_eq!(error.http_status, Some(status));
        assert_eq!(error.provider_code.as_deref(), Some("provider_failure"));
        assert!(
            !serde_json::to_string(&error)
                .unwrap()
                .contains("fixture-api-key")
        );
        server.verify().await;
    }
    server.reset().await;
    Mock::given(path("/account")).respond_with(ResponseTemplate::new(403).set_body_json(json!({"error":{"type":"customer_verification_required","message":"private provider detail"}}))).expect(1).mount(&server).await;
    assert_eq!(
        crate::client::call(connection(&server.uri()), request("account"), &password())
            .await
            .unwrap_err()
            .provider_code
            .as_deref(),
        Some("customer_verification_required")
    );
    Mock::given(path("/redirect"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", target.uri()))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        crate::client::call(connection(&server.uri()), request("redirect"), &password())
            .await
            .unwrap_err()
            .http_status,
        Some(302)
    );
    assert!(target.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn timeout_cancellation_streaming_and_output_limit() {
    let server = MockServer::start().await;
    Mock::given(path("/slow"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(2))
                .set_body_string("late"),
        )
        .mount(&server)
        .await;
    let r = from_json(
        json!({"operation":"request","connection":"test","method":"GET","path":"slow","timeoutMs":10}),
    );
    assert_eq!(
        crate::client::call(connection(&server.uri()), r, &password())
            .await
            .unwrap_err()
            .code,
        "request-timeout"
    );
    let c = connection(&server.uri());
    let task =
        tokio::spawn(async move { crate::client::call(c, request("slow"), &password()).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    Mock::given(path("/stream"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("data: hi\n\n", "text/event-stream"))
        .mount(&server)
        .await;
    assert_eq!(
        crate::client::call(connection(&server.uri()), request("stream"), &password())
            .await
            .unwrap_err()
            .code,
        "unsupported-response"
    );
    Mock::given(path("/large"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'a'; 8 * 1024 * 1024 + 1]))
        .mount(&server)
        .await;
    assert_eq!(
        crate::client::call(connection(&server.uri()), request("large"), &password())
            .await
            .unwrap_err()
            .code,
        "response-too-large"
    );
}

#[tokio::test]
async fn jev_matches_ai_sdk_7_contract_and_keeps_probabilities() {
    let server = MockServer::start().await;
    let body: Value =
        serde_json::from_str(include_str!("../tests/fixtures/jev-request.json")).unwrap();
    let response: Value =
        serde_json::from_str(include_str!("../tests/fixtures/jev-response.json")).unwrap();
    Mock::given(method("POST"))
        .and(path("/v4/ai/evaluation-model"))
        .and(header("authorization", "Bearer fixture-api-key"))
        .and(header("ai-gateway-protocol-version", "0.0.1"))
        .and(header("ai-gateway-auth-method", "api-key"))
        .and(header("ai-evaluation-model-specification-version", "4"))
        .and(header("ai-model-id", "typesafe-ai/jev"))
        .and(body_json(body.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(response.clone()))
        .expect(1)
        .mount(&server)
        .await;
    let mut c = connection(&server.uri());
    c.adapter = Adapter::VercelAiGateway;
    c.default_model = Some("typesafe-ai/jev".into());
    let mut r = body;
    r["operation"] = "evaluate".into();
    r["connection"] = "test".into();
    let output = crate::client::call(c.clone(), from_json(r.clone()), &password())
        .await
        .unwrap();
    assert_eq!(output.data, response);
    assert_eq!(output.data["answers"]["refunded"]["probability"], 0.98);
    for invalid in [
        json!({"answers":{}}),
        json!({"answers":{"refunded":{"type":"boolean","probability":true}}}),
    ] {
        server.reset().await;
        Mock::given(path("/v4/ai/evaluation-model"))
            .respond_with(ResponseTemplate::new(200).set_body_json(invalid))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            crate::client::call(c.clone(), from_json(r.clone()), &password())
                .await
                .unwrap_err()
                .code,
            "invalid-response"
        );
    }
}
