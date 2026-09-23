use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use reqwest::{
    Method,
    header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue},
};
use serde_json::{Value, json};
use url::{Host, Url};

use crate::{
    Adapter, Auth, Body, Connection, GatewayError, GatewayRequest, GatewayResult, Question, Secret,
};

pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn validate_connection(c: &Connection) -> Result<Url, GatewayError> {
    if c.name.is_empty()
        || c.name.len() > 64
        || !c
            .name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(GatewayError::input(
            "connection name must use 1 to 64 ASCII letters, digits, dots, underscores or hyphens",
        ));
    }
    let mut url = Url::parse(&c.base_url)
        .map_err(|_| GatewayError::input("baseUrl must be an absolute HTTP(S) URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(GatewayError::input(
            "baseUrl must be HTTP(S), without userinfo, query or fragment",
        ));
    }
    if url.scheme() == "http" && !is_loopback(&url) && !c.allow_http {
        return Err(GatewayError::input(
            "non-loopback HTTP requires allowHttp on the connection",
        ));
    }
    if c.default_model
        .as_ref()
        .is_some_and(|m| m.is_empty() || m.len() > 256)
    {
        return Err(GatewayError::input(
            "defaultModel must contain 1 to 256 bytes",
        ));
    }
    if let Auth::Header { name, .. } = &c.auth {
        let header = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| GatewayError::input("invalid authentication header name"))?;
        if transport_header(header.as_str()) {
            return Err(GatewayError::input(
                "authentication cannot use a transport header",
            ));
        }
    }
    if let Some(key) = c.auth.key()
        && (key.is_empty() || key.len() > 8192 || HeaderValue::from_str(key).is_err())
    {
        return Err(GatewayError::input(
            "key must be a nonempty valid header value of at most 8192 bytes",
        ));
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        Some(Host::Domain(name)) => name == "localhost",
        _ => false,
    }
}

fn transport_header(name: &str) -> bool {
    matches!(
        name,
        "host"
            | "content-length"
            | "transfer-encoding"
            | "connection"
            | "upgrade"
            | "proxy-authorization"
            | "proxy-connection"
            | "te"
            | "trailer"
    )
}

struct Prepared {
    method: Method,
    path: String,
    query: BTreeMap<String, String>,
    headers: BTreeMap<String, String>,
    body: Option<Body>,
    timeout_ms: u64,
    json_response: bool,
}

fn model(explicit: Option<String>, connection: &Connection) -> Result<String, GatewayError> {
    explicit
        .or_else(|| connection.default_model.clone())
        .filter(|v| !v.is_empty() && v.len() <= 256)
        .ok_or_else(|| GatewayError::input("model or connection.defaultModel is required"))
}

fn prepare(c: &Connection, request: GatewayRequest) -> Result<Prepared, GatewayError> {
    let mut p = Prepared {
        method: Method::GET,
        path: String::new(),
        query: BTreeMap::new(),
        headers: BTreeMap::new(),
        body: None,
        timeout_ms: 30_000,
        json_response: false,
    };
    let timeout = match request {
        GatewayRequest::Request {
            method,
            path,
            query,
            headers,
            body,
            timeout_ms,
            ..
        } => {
            p.method = Method::from_bytes(method.as_bytes())
                .map_err(|_| GatewayError::input("invalid HTTP method"))?;
            if !matches!(
                p.method,
                Method::GET
                    | Method::POST
                    | Method::PUT
                    | Method::PATCH
                    | Method::DELETE
                    | Method::HEAD
                    | Method::OPTIONS
            ) {
                return Err(GatewayError::input("unsupported HTTP method"));
            }
            p.path = path;
            p.query = query;
            p.headers = headers;
            p.body = body;
            timeout_ms
        }
        GatewayRequest::Models { timeout_ms, .. } => {
            compatible(c)?;
            p.path = "models".into();
            p.json_response = true;
            timeout_ms
        }
        GatewayRequest::Chat {
            model: explicit,
            messages,
            parameters,
            timeout_ms,
            ..
        } => {
            compatible(c)?;
            if messages.is_empty()
                || messages
                    .iter()
                    .any(|m| !m.is_object() || !m.get("role").is_some_and(Value::is_string))
            {
                return Err(GatewayError::input(
                    "messages must be a nonempty array of objects with a role",
                ));
            }
            if parameters
                .keys()
                .any(|k| matches!(k.as_str(), "model" | "messages" | "stream"))
            {
                return Err(GatewayError::input(
                    "parameters cannot override model, messages or stream",
                ));
            }
            let mut body = json!({"model":model(explicit,c)?,"messages":messages,"stream":false});
            body.as_object_mut()
                .expect("object literal")
                .extend(parameters);
            p.method = Method::POST;
            p.path = "chat/completions".into();
            p.body = Some(Body::Json { value: body });
            p.json_response = true;
            timeout_ms
        }
        GatewayRequest::Evaluate {
            model: explicit,
            state,
            questions,
            provider_options,
            timeout_ms,
            ..
        } => {
            if c.adapter != Adapter::VercelAiGateway {
                return Err(GatewayError::new(
                    "unsupported-capability",
                    "input",
                    "evaluate requires a vercel-ai-gateway connection",
                ));
            }
            if !matches!(c.auth, Auth::Bearer { .. }) {
                return Err(GatewayError::input(
                    "Vercel evaluation requires Bearer authentication",
                ));
            }
            if !evaluation_input(&state)
                || questions.is_empty()
                || questions.values().any(|q| match q {
                    Question::Boolean {
                        instructions,
                        criteria,
                    } => {
                        !evaluation_input(instructions)
                            || criteria.as_ref().is_some_and(|c| {
                                c.iter().any(|(k, v)| {
                                    !matches!(k.as_str(), "true" | "false")
                                        || v.as_ref().is_some_and(|v| !evaluation_input(v))
                                })
                            })
                    }
                    Question::Choice {
                        instructions,
                        criteria,
                    } => {
                        !evaluation_input(instructions)
                            || criteria.is_empty()
                            || criteria.iter().any(|(k, v)| {
                                k.is_empty() || v.as_ref().is_some_and(|v| !evaluation_input(v))
                            })
                    }
                    Question::Score {
                        instructions,
                        criteria,
                    } => {
                        !evaluation_input(instructions)
                            || criteria.len() < 2
                            || criteria.iter().flatten().any(|v| !evaluation_input(v))
                    }
                })
            {
                return Err(GatewayError::input(
                    "questions must contain valid boolean, choice or score instructions",
                ));
            }
            let mut body = json!({"state":state,"questions":questions});
            if let Some(options) = provider_options {
                body["providerOptions"] = options;
            }
            p.method = Method::POST;
            p.path = "v4/ai/evaluation-model".into();
            p.headers = BTreeMap::from([
                ("ai-gateway-protocol-version".into(), "0.0.1".into()),
                ("ai-gateway-auth-method".into(), "api-key".into()),
                (
                    "ai-evaluation-model-specification-version".into(),
                    "4".into(),
                ),
                ("ai-model-id".into(), model(explicit, c)?),
            ]);
            p.body = Some(Body::Json { value: body });
            p.json_response = true;
            timeout_ms
        }
        _ => {
            return Err(GatewayError::input(
                "operation does not make an HTTP request",
            ));
        }
    };
    p.timeout_ms = timeout.unwrap_or(30_000);
    if p.timeout_ms == 0 || p.timeout_ms > 3_600_000 {
        return Err(GatewayError::input(
            "timeoutMs must be between 1 and 3600000",
        ));
    }
    Ok(p)
}

fn compatible(c: &Connection) -> Result<(), GatewayError> {
    if c.adapter != Adapter::OpenaiCompatible {
        Err(GatewayError::new(
            "unsupported-capability",
            "input",
            "operation requires an openai-compatible connection",
        ))
    } else {
        Ok(())
    }
}

pub(crate) async fn call(
    c: Connection,
    request: GatewayRequest,
    passphrase: &Secret,
) -> Result<GatewayResult, GatewayError> {
    let operation = request.operation().to_owned();
    let base = validate_connection(&c)?;
    let expected_questions = match &request {
        GatewayRequest::Evaluate { questions, .. } => Some(questions.clone()),
        _ => None,
    };
    let p = prepare(&c, request)?;
    if p.path.starts_with("//") || p.path.contains(['\\', '?', '#']) || Url::parse(&p.path).is_ok()
    {
        return Err(GatewayError::input(
            "path must be relative, without a query or fragment",
        ));
    }
    let mut url = base
        .join(p.path.trim_start_matches('/'))
        .map_err(|_| GatewayError::input("invalid relative path"))?;
    if url.origin() != base.origin() || !url.path().starts_with(base.path()) {
        return Err(GatewayError::input(
            "request path must stay within the configured base URL",
        ));
    }
    if !p.query.is_empty() {
        url.query_pairs_mut().extend_pairs(&p.query);
    }
    let mut headers = HeaderMap::new();
    for (name, value) in &p.headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| GatewayError::input("invalid request header name"))?;
        let auth_header = match &c.auth {
            Auth::Header { name, .. } => name.as_str(),
            _ => "authorization",
        };
        if transport_header(name.as_str())
            || matches!(name.as_str(), "authorization" | "api-key" | "x-api-key")
            || name.as_str().eq_ignore_ascii_case(auth_header)
        {
            return Err(GatewayError::input(
                "request headers cannot override authentication or transport headers",
            ));
        }
        headers.insert(
            name,
            HeaderValue::from_str(value)
                .map_err(|_| GatewayError::input("invalid request header value"))?,
        );
    }
    let auth = match &c.auth {
        Auth::None => None,
        Auth::Bearer { key } => Some((
            HeaderName::from_static("authorization"),
            format!("Bearer {}", key.expose()),
        )),
        Auth::Header { name, key } => Some((
            HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| GatewayError::input("invalid authentication header"))?,
            key.expose().to_owned(),
        )),
    };
    if let Some((name, value)) = auth {
        let mut value = HeaderValue::from_str(&value)
            .map_err(|_| GatewayError::input("invalid authentication value"))?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    let body = match p.body {
        Some(Body::Json { value }) => {
            if value.get("stream") == Some(&Value::Bool(true)) {
                return Err(GatewayError::input("streaming is not supported"));
            }
            headers
                .entry(CONTENT_TYPE)
                .or_insert(HeaderValue::from_static("application/json"));
            serde_json::to_vec(&value).map_err(|_| GatewayError::input("invalid JSON body"))?
        }
        Some(Body::Text { value }) => {
            headers
                .entry(CONTENT_TYPE)
                .or_insert(HeaderValue::from_static("text/plain; charset=utf-8"));
            value.into_bytes()
        }
        None => Vec::new(),
    };
    if body.len() > MAX_REQUEST_BYTES
        || url.as_str().len()
            + headers
                .iter()
                .map(|(k, v)| k.as_str().len() + v.len())
                .sum::<usize>()
            > MAX_REQUEST_BYTES
    {
        return Err(GatewayError::input("request exceeds the 1 MiB limit"));
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never());
    if is_loopback(&url) {
        builder = builder.no_proxy();
    }
    let client = builder.build().map_err(|_| {
        GatewayError::new(
            "http-client-failed",
            "acquire",
            "could not initialize HTTP client",
        )
    })?;
    let started = Instant::now();
    let mut observed_status = None;
    let result = tokio::time::timeout(Duration::from_millis(p.timeout_ms), async {
        let mut response = client
            .request(p.method, url)
            .headers(headers)
            .body(body)
            .send()
            .await
            .map_err(network_error)?;
        let status = response.status().as_u16();
        observed_status = Some(status);
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if content_type.starts_with("text/event-stream") {
            return Err(GatewayError::new(
                "unsupported-response",
                "acquire",
                "streaming responses are not supported",
            ));
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
        {
            return Err(response_limit());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(network_error)? {
            if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(response_limit());
            }
            bytes.extend_from_slice(&chunk);
        }
        let text = String::from_utf8(bytes).map_err(|_| {
            GatewayError::new(
                "unsupported-response",
                "acquire",
                "response is not UTF-8 text",
            )
        })?;
        let parsed = serde_json::from_str::<Value>(&text);
        if !(200..300).contains(&status) {
            let mut error = GatewayError::new(
                "http-error",
                "acquire",
                "provider returned an unsuccessful HTTP status",
            );
            error.http_status = Some(status);
            // Return only a bounded machine code, never arbitrary upstream error text.
            error.provider_code = parsed
                .as_ref()
                .ok()
                .and_then(|v| {
                    v.pointer("/error/code")
                        .or_else(|| v.pointer("/error/type"))
                        .or_else(|| v.get("code"))
                })
                .and_then(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .or_else(|| v.as_i64().map(|n| n.to_string()))
                })
                .filter(|v| {
                    v.len() <= 128
                        && v.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_-./".contains(&b))
                })
                .filter(|v| {
                    !v.contains(passphrase.expose())
                        && !c.auth.key().is_some_and(|key| v.contains(key))
                });
            error.retryable = matches!(status, 408 | 425 | 429 | 500..=599);
            return Err(error);
        }
        let mut data = if p.json_response {
            parsed.map_err(|_| {
                GatewayError::new(
                    "invalid-response",
                    "protocol",
                    "provider did not return JSON",
                )
            })?
        } else {
            parsed.unwrap_or(Value::String(text))
        };
        if let Some(questions) = &expected_questions {
            validate_evaluation(&data, questions)?;
        }
        if matches!(operation.as_str(), "chat" | "models") {
            let field = if operation == "chat" {
                "choices"
            } else {
                "data"
            };
            if !data.get(field).is_some_and(Value::is_array) {
                return Err(GatewayError::new(
                    "invalid-response",
                    "protocol",
                    "provider returned an incompatible model response",
                ));
            }
        }
        redact(
            &mut data,
            &[c.auth.key().unwrap_or(""), passphrase.expose()],
        );
        Ok(GatewayResult {
            schema_version: 1,
            operation,
            connection: Some(c.name.clone()),
            http_status: Some(status),
            elapsed_ms: elapsed(started),
            data,
        })
    })
    .await;
    let mut result = result.unwrap_or_else(|_| {
        Err(GatewayError::new(
            "request-timeout",
            "acquire",
            "request timed out; a dispatched operation may have completed",
        ))
    });
    if let Err(error) = &mut result {
        error.elapsed_ms = Some(elapsed(started));
        error.http_status = error.http_status.or(observed_status);
    }
    result
}

fn validate_evaluation(
    data: &Value,
    questions: &BTreeMap<String, Question>,
) -> Result<(), GatewayError> {
    let valid = data
        .get("answers")
        .and_then(Value::as_object)
        .is_some_and(|answers| {
            answers.len() == questions.len()
                && questions.iter().all(|(name, question)| {
                    let Some(answer) = answers.get(name) else {
                        return false;
                    };
                    let probabilities_valid = answer.get("probabilities").is_none_or(|p| {
                        p.as_object().is_some_and(|p| {
                            p.values()
                                .all(|v| v.as_f64().is_some_and(|n| (0.0..=1.0).contains(&n)))
                        })
                    });
                    probabilities_valid
                        && match (question, answer.get("type").and_then(Value::as_str)) {
                            (Question::Boolean { .. }, Some("boolean")) => answer
                                .get("probability")
                                .and_then(Value::as_f64)
                                .is_some_and(|p| (0.0..=1.0).contains(&p)),
                            (Question::Choice { criteria, .. }, Some("choice")) => answer
                                .get("choice")
                                .and_then(Value::as_str)
                                .is_some_and(|name| criteria.contains_key(name)),
                            (Question::Score { .. }, Some("score")) => {
                                answer.get("score").is_some_and(Value::is_number)
                            }
                            _ => false,
                        }
                })
        });
    if valid {
        Ok(())
    } else {
        Err(GatewayError::new(
            "invalid-response",
            "protocol",
            "provider returned an incompatible evaluation response",
        ))
    }
}

fn evaluation_input(value: &Value) -> bool {
    value.is_string() || value.is_object() || value.is_array()
}

fn redact(data: &mut Value, secrets: &[&str]) {
    match data {
        Value::String(s) => {
            for secret in secrets.iter().filter(|s| !s.is_empty()) {
                *s = s.replace(secret, "[REDACTED]");
            }
        }
        Value::Array(values) => {
            for value in values {
                redact(value, secrets);
            }
        }
        Value::Object(values) => {
            let old = std::mem::take(values);
            for (key, mut value) in old {
                let mut key = key;
                for secret in secrets.iter().filter(|s| !s.is_empty()) {
                    key = key.replace(secret, "[REDACTED]");
                }
                redact(&mut value, secrets);
                values.insert(key, value);
            }
        }
        _ => (),
    }
}
fn network_error(_: reqwest::Error) -> GatewayError {
    GatewayError::new(
        "network-error",
        "acquire",
        "request failed; a dispatched operation may have completed",
    )
}
fn response_limit() -> GatewayError {
    GatewayError::new(
        "response-too-large",
        "acquire",
        "response exceeds the 8 MiB limit",
    )
}
fn elapsed(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}
