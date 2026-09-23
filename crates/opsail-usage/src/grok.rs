use std::env;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::io::AsyncReadExt;

use crate::model::{UsageEntry, UsageOptions, UsageProvider, UsageSnapshot};

const BILLING_ENDPOINT: &str = "https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig";
const CLI_BILLING_ENDPOINT: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const LOGIN_REQUIRED: &str = "Grok sign-in is no longer valid; run `grok login`";
const CLOUDFLARE_BLOCKED: &str = "Grok 计费接口被 Cloudflare 拦截（非登录问题）";
const UNAVAILABLE: &str = "the Grok billing endpoint is temporarily unavailable";
const UNRECOGNIZED: &str = "the Grok billing response was not recognized";
const OIDC_SCOPE_PREFIX: &str = "https://auth.x.ai::";
const LEGACY_SESSION_SCOPE: &str = "https://accounts.x.ai/sign-in";
const EMPTY_GRPC_WEB_FRAME: [u8; 5] = [0, 0, 0, 0, 0];
const MAX_GROK_AUTH_BYTES: u64 = 1024 * 1024;
const MAX_GROK_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

static INSTALL_TLS: Once = Once::new();

struct GrokToken(String);

impl std::fmt::Debug for GrokToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("<redacted>")
    }
}

struct GrokAuth {
    token: GrokToken,
    expired: bool,
}

pub(crate) async fn read_grok_usage(options: &UsageOptions) -> UsageEntry {
    let auth_path = resolve_grok_auth_path(options.grok_auth_path.as_deref());
    let content = match read_auth_file(&auth_path).await {
        Ok(content) => content,
        Err(GrokAuthFileError::NotFound) => {
            return UsageEntry::unavailable(
                UsageProvider::Grok,
                "Grok CLI is not signed in; run `grok login`",
            );
        }
        Err(error) => {
            return UsageEntry::unavailable(UsageProvider::Grok, error.detail());
        }
    };

    let auth = match parse_grok_auth(&content, now_millis()) {
        Ok(auth) => auth,
        Err(detail) => return UsageEntry::unavailable(UsageProvider::Grok, detail),
    };
    if auth.expired {
        return UsageEntry::unavailable(UsageProvider::Grok, LOGIN_REQUIRED);
    }
    match query_billing(&auth, options).await {
        Ok(snapshot) => UsageEntry::from_grok(snapshot),
        Err(detail) => UsageEntry::unavailable(UsageProvider::Grok, detail),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GrokAuthFileError {
    NotFound,
    Unreadable,
    NotRegular,
    TooLarge,
    InvalidUtf8,
}

impl GrokAuthFileError {
    fn detail(self) -> &'static str {
        match self {
            Self::NotFound => "Grok CLI is not signed in; run `grok login`",
            Self::Unreadable => "the Grok CLI sign-in file could not be read",
            Self::NotRegular => "the Grok CLI sign-in path is not a regular file",
            Self::TooLarge => "the Grok CLI sign-in file exceeds the 1 MiB safety limit",
            Self::InvalidUtf8 => "the Grok CLI sign-in file is not valid UTF-8 JSON",
        }
    }
}

async fn read_auth_file(path: &Path) -> Result<String, GrokAuthFileError> {
    let metadata = tokio::fs::metadata(path).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            GrokAuthFileError::NotFound
        } else {
            GrokAuthFileError::Unreadable
        }
    })?;
    if !metadata.is_file() {
        return Err(GrokAuthFileError::NotRegular);
    }
    if metadata.len() > MAX_GROK_AUTH_BYTES {
        return Err(GrokAuthFileError::TooLarge);
    }

    let file = tokio::fs::File::open(path).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            GrokAuthFileError::NotFound
        } else {
            GrokAuthFileError::Unreadable
        }
    })?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_GROK_AUTH_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| GrokAuthFileError::Unreadable)?;
    if bytes.len() as u64 > MAX_GROK_AUTH_BYTES {
        return Err(GrokAuthFileError::TooLarge);
    }
    String::from_utf8(bytes).map_err(|_| GrokAuthFileError::InvalidUtf8)
}

fn resolve_grok_auth_path(explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    if let Some(path) = env::var_os("OPSAIL_GROK_AUTH").filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".grok").join("auth.json")
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn parse_grok_auth(content: &str, now_ms: u64) -> Result<GrokAuth, String> {
    let root: Value = serde_json::from_str(content)
        .map_err(|_| "the Grok CLI sign-in file is not valid JSON".to_owned())?;
    let object = root
        .as_object()
        .ok_or_else(|| "the Grok CLI sign-in file is not valid JSON".to_owned())?;

    let mut oidc = None;
    let mut legacy = None;
    for (scope, entry) in object {
        let Some(map) = entry.as_object() else {
            continue;
        };
        let Some(key) = map
            .get("key")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let expires = map
            .get("expires_at")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        if scope.starts_with(OIDC_SCOPE_PREFIX) {
            oidc = Some((key.to_owned(), expires));
        } else if scope == LEGACY_SESSION_SCOPE || scope.contains("/sign-in") {
            legacy = Some((key.to_owned(), expires));
        }
    }
    let (key, expires_at) = oidc.or(legacy).ok_or_else(|| {
        "the Grok CLI sign-in file does not contain a usable OAuth token".to_owned()
    })?;
    let expired = expires_at
        .as_deref()
        .and_then(parse_rfc3339_millis)
        .is_some_and(|expires| expires <= now_ms);
    Ok(GrokAuth {
        token: GrokToken(key),
        expired,
    })
}

fn parse_rfc3339_millis(value: &str) -> Option<u64> {
    let date = OffsetDateTime::parse(value, &Rfc3339).ok()?;
    u64::try_from(date.unix_timestamp_nanos() / 1_000_000).ok()
}

async fn query_billing(auth: &GrokAuth, options: &UsageOptions) -> Result<UsageSnapshot, String> {
    INSTALL_TLS.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    let client = reqwest::Client::builder()
        .timeout(options.timeout)
        .connect_timeout(options.timeout.min(std::time::Duration::from_secs(8)))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| UNAVAILABLE.to_owned())?;

    let endpoint = CLI_BILLING_ENDPOINT;
    let fallback = BILLING_ENDPOINT;
    #[cfg(test)]
    let endpoint = options.grok_endpoint.as_deref().unwrap_or(endpoint);
    #[cfg(test)]
    let fallback = options
        .grok_fallback_endpoint
        .as_deref()
        .unwrap_or(fallback);

    // Leave room for the legacy read within the provider's shared deadline.
    let primary = query_cli_billing(&client, endpoint, auth, options).await;
    match primary {
        Ok(snapshot) => Ok(snapshot),
        Err(detail) if detail == LOGIN_REQUIRED => Err(detail),
        Err(primary_detail) => {
            match query_browser_billing(&client, fallback, auth, options).await {
                Ok(snapshot) => Ok(snapshot),
                Err(detail) if detail == UNAVAILABLE => Err(primary_detail),
                Err(detail) => Err(detail),
            }
        }
    }
}

fn auth_headers(auth: &GrokAuth, options: &UsageOptions) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    let mut bearer = HeaderValue::from_str(&format!("Bearer {}", auth.token.0))
        .map_err(|_| LOGIN_REQUIRED.to_owned())?;
    bearer.set_sensitive(true);
    headers.insert(reqwest::header::AUTHORIZATION, bearer);
    let user_agent = format!("{}/{}", options.client.name, options.client.version);
    headers.insert(
        reqwest::header::USER_AGENT,
        HeaderValue::from_str(&user_agent).unwrap_or(HeaderValue::from_static("opsail")),
    );
    Ok(headers)
}

async fn query_cli_billing(
    client: &reqwest::Client,
    endpoint: &str,
    auth: &GrokAuth,
    options: &UsageOptions,
) -> Result<UsageSnapshot, String> {
    let mut headers = auth_headers(auth, options)?;
    headers.insert("x-xai-token-auth", HeaderValue::from_static("xai-grok-cli"));
    headers.insert(
        reqwest::header::ACCEPT,
        HeaderValue::from_static("application/json"),
    );
    let response = client
        .get(endpoint)
        .headers(headers)
        .timeout(options.timeout / 2)
        .send()
        .await
        .map_err(|_| UNAVAILABLE.to_owned())?;
    let bytes = billing_response(response).await?;
    parse_cli_billing_payload(&bytes).ok_or_else(|| UNRECOGNIZED.to_owned())
}

async fn query_browser_billing(
    client: &reqwest::Client,
    endpoint: &str,
    auth: &GrokAuth,
    options: &UsageOptions,
) -> Result<UsageSnapshot, String> {
    let mut headers = auth_headers(auth, options)?;
    headers.insert(
        reqwest::header::ORIGIN,
        HeaderValue::from_static("https://grok.com"),
    );
    headers.insert(
        reqwest::header::REFERER,
        HeaderValue::from_static("https://grok.com/?_s=usage"),
    );
    headers.insert(reqwest::header::ACCEPT, HeaderValue::from_static("*/*"));
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/grpc-web+proto"),
    );
    headers.insert(
        HeaderName::from_static("x-grpc-web"),
        HeaderValue::from_static("1"),
    );
    headers.insert(
        HeaderName::from_static("x-user-agent"),
        HeaderValue::from_static("connect-es/2.1.1"),
    );
    let response = client
        .post(endpoint)
        .headers(headers)
        .body(EMPTY_GRPC_WEB_FRAME.to_vec())
        .send()
        .await
        .map_err(|_| UNAVAILABLE.to_owned())?;
    let bytes = billing_response(response).await?;
    let now_seconds = now_millis() / 1_000;
    let parsed =
        parse_grok_billing_payload(&bytes, now_seconds).ok_or_else(|| UNRECOGNIZED.to_owned())?;
    let used_percent = parsed.used_percent.clamp(0.0, 100.0);
    Ok(UsageSnapshot {
        remaining_percent: (100.0 - used_percent).round().clamp(0.0, 100.0) as u8,
        used_percent,
        resets_at: parsed.resets_at,
        window_duration_mins: None,
        plan_type: Some("grok-build".to_owned()),
        reset_credit_available_count: None,
        reset_credit_expires_at: None,
    })
}

async fn billing_response(response: reqwest::Response) -> Result<Vec<u8>, String> {
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    // Header-only classifications still work for oversized or broken error bodies.
    if let Some(detail) = response_failure(status, &headers, &[])
        && detail != UNAVAILABLE
    {
        return Err(detail);
    }
    let bytes = match read_bounded_response(response).await {
        Ok(bytes) => bytes,
        Err(_) if !(200..300).contains(&status) => return Err(UNAVAILABLE.to_owned()),
        Err(detail) => return Err(detail),
    };
    if let Some(detail) = response_failure(status, &headers, &bytes) {
        return Err(detail);
    }
    Ok(bytes)
}

fn response_failure(status: u16, headers: &HeaderMap, bytes: &[u8]) -> Option<String> {
    if status == 401 {
        return Some(LOGIN_REQUIRED.to_owned());
    }
    if status == 403 && is_html_or_cloudflare(headers, bytes) {
        return Some(CLOUDFLARE_BLOCKED.to_owned());
    }
    if let Some(failure) = grpc_header_failure(headers).or_else(|| grpc_trailer_failure(bytes)) {
        return Some(failure);
    }
    if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
        let error = value.get("error").unwrap_or(&value);
        if error.as_str().is_some_and(explicit_auth_failure)
            || ["code", "message"].iter().any(|key| {
                error
                    .get(key)
                    .and_then(Value::as_str)
                    .is_some_and(explicit_auth_failure)
            })
        {
            return Some(LOGIN_REQUIRED.to_owned());
        }
    }
    (!(200..300).contains(&status)).then(|| UNAVAILABLE.to_owned())
}

fn is_html_or_cloudflare(headers: &HeaderMap, bytes: &[u8]) -> bool {
    let body = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    header_text(headers, "content-type")
        .is_some_and(|v| v.to_ascii_lowercase().contains("text/html"))
        || header_text(headers, "server").is_some_and(|v| v.eq_ignore_ascii_case("cloudflare"))
        || headers.contains_key("cf-ray")
        || body.contains("<html")
        || body.contains("<!doctype html")
        || body.contains("cf-error-details")
        || body.contains("cf-chl-")
        || body.contains("cloudflare")
}

fn explicit_auth_failure(message: &str) -> bool {
    let normalized = message.trim().trim_end_matches('.').to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "unauthenticated" | "bad-credentials" | "bad credentials" | "bad_credentials"
    ) || normalized.starts_with("bad-credentials:")
}

fn parse_cli_billing_payload(bytes: &[u8]) -> Option<UsageSnapshot> {
    let root: Value = serde_json::from_slice(bytes).ok()?;
    let config = root.get("config")?.as_object()?;
    let current = config.get("currentPeriod").filter(|v| !v.is_null());
    let (start, end) = if let Some(period) = current {
        let period = period.as_object()?;
        (period.get("start"), period.get("end"))
    } else {
        (
            config.get("billingPeriodStart"),
            config.get("billingPeriodEnd"),
        )
    };
    let start = start.and_then(Value::as_str).and_then(parse_rfc3339_millis);
    let end = end.and_then(Value::as_str).and_then(parse_rfc3339_millis);
    let duration = start
        .zip(end)
        .and_then(|(start, end)| end.checked_sub(start))
        .filter(|duration| *duration > 0)
        .map(|duration| duration as f64 / 60_000.0);
    let used_percent = match config.get("creditUsagePercent") {
        Some(value) => value.as_f64()?,
        None if config.contains_key("monthlyLimit") => {
            let limit = cent_value(config.get("monthlyLimit")?)?;
            if limit <= 0.0 {
                return None;
            }
            let used = config.get("used").map(cent_value).unwrap_or(Some(0.0))?;
            used / limit * 100.0
        }
        // Proto3 omits a zero scalar. Require a complete, recognized credits period
        // before accepting this shape (the official CLI also treats it as zero).
        None if !config.contains_key("used")
            && duration.is_some()
            && current
                .and_then(|p| p.get("type"))
                .and_then(Value::as_str)
                .is_some_and(|kind| {
                    matches!(
                        kind,
                        "USAGE_PERIOD_TYPE_WEEKLY" | "USAGE_PERIOD_TYPE_MONTHLY"
                    )
                }) =>
        {
            0.0
        }
        None => return None,
    };
    if !used_percent.is_finite() {
        return None;
    }
    let used_percent = used_percent.clamp(0.0, 100.0);
    // Only known plan names are projected; arbitrary server strings may contain PII.
    let plan_type = root
        .get("subscription_tier")
        .and_then(Value::as_str)
        .filter(|name| {
            matches!(
                *name,
                "SuperGrok"
                    | "SuperGrok Heavy"
                    | "SuperGrok Lite"
                    | "SuperGrok Plus"
                    | "Premium"
                    | "Premium+"
                    | "Free"
            )
        })
        .map(ToOwned::to_owned);
    Some(UsageSnapshot {
        remaining_percent: (100.0 - used_percent).round() as u8,
        used_percent,
        resets_at: end.map(|millis| millis / 1_000),
        window_duration_mins: duration,
        plan_type,
        reset_credit_available_count: None,
        reset_credit_expires_at: None,
    })
}

fn cent_value(value: &Value) -> Option<f64> {
    let object = value.as_object()?;
    let amount = match object.get("val") {
        // A present, empty Cent is proto3's zero.
        None if object.is_empty() => 0.0,
        None => return None,
        Some(Value::String(value)) => value.parse::<f64>().ok()?,
        Some(value) => value.as_f64()?,
    };
    (amount.is_finite() && amount >= 0.0).then_some(amount)
}

async fn read_bounded_response(response: reqwest::Response) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_GROK_RESPONSE_BYTES as u64)
    {
        return Err("the Grok billing response exceeded the 2 MiB safety limit".to_owned());
    }

    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|_| "the Grok billing endpoint is temporarily unavailable".to_owned())?;
        if chunk.len() > MAX_GROK_RESPONSE_BYTES.saturating_sub(bytes.len()) {
            return Err("the Grok billing response exceeded the 2 MiB safety limit".to_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn grpc_header_failure(headers: &HeaderMap) -> Option<String> {
    let status = header_i32(headers, "grpc-status")?;
    if status == 0 {
        return None;
    }
    Some(grpc_failure(
        status,
        header_text(headers, "grpc-message")
            .as_deref()
            .unwrap_or(""),
    ))
}

fn header_i32(headers: &HeaderMap, name: &str) -> Option<i32> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

fn header_text(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(percent_decode)
}

fn grpc_trailer_failure(bytes: &[u8]) -> Option<String> {
    let trailers = grpc_web_trailers(bytes);
    let status = trailers
        .iter()
        .find(|(name, _)| name == "grpc-status")
        .and_then(|(_, value)| value.parse::<i32>().ok())?;
    if status == 0 {
        return None;
    }
    let message = trailers
        .iter()
        .find(|(name, _)| name == "grpc-message")
        .map(|(_, value)| value.as_str())
        .unwrap_or("");
    Some(grpc_failure(status, message))
}

fn grpc_failure(status: i32, message: &str) -> String {
    let normalized = message.to_ascii_lowercase();
    if status == 16 || explicit_auth_failure(message) {
        return LOGIN_REQUIRED.to_owned();
    }
    if status == 9 && normalized.trim_end_matches('.').trim() == "no personal team" {
        return "Grok team accounts do not expose personal remaining-usage windows".to_owned();
    }
    "the Grok billing endpoint is temporarily unavailable".to_owned()
}

struct BillingWindow {
    used_percent: f64,
    resets_at: Option<u64>,
}

fn parse_grok_billing_payload(input: &[u8], now_seconds: u64) -> Option<BillingWindow> {
    let mut frames = grpc_web_frames(input);
    if frames.is_empty() && looks_like_protobuf(input) {
        frames = vec![input.to_vec()];
    }
    if frames.is_empty() {
        return None;
    }

    let mut scan = Scan {
        fixed32: Vec::new(),
        varints: Vec::new(),
    };
    for frame in &frames {
        scan_protobuf(frame, 0, &[], 0, &mut scan);
    }

    let mut percentages = scan
        .fixed32
        .iter()
        .filter(|item| {
            item.path.last() == Some(&1)
                && item.value.is_finite()
                && (0.0..=100.0).contains(&item.value)
        })
        .cloned()
        .collect::<Vec<_>>();
    percentages.sort_by(|left, right| {
        left.path
            .len()
            .cmp(&right.path.len())
            .then(left.order.cmp(&right.order))
    });
    let used_percent = percentages.first().map(|item| f64::from(item.value));

    let resets = scan
        .varints
        .iter()
        .filter(|item| (1_700_000_000..=2_100_000_000).contains(&item.value))
        .filter(|item| item.value > now_seconds)
        .cloned()
        .collect::<Vec<_>>();
    let exact: Vec<u64> = resets
        .iter()
        .filter(|item| item.path.as_slice() == [1, 5, 1])
        .map(|item| item.value)
        .collect();
    let resets_at = if exact.is_empty() {
        resets.iter().map(|item| item.value).min()
    } else {
        exact.into_iter().min()
    };

    let has_usage_period = scan.varints.iter().any(|item| {
        (item.path.first() == Some(&1) && item.path.get(1) == Some(&6))
            || (item.path.as_slice() == [1, 8, 1] && (item.value == 1 || item.value == 2))
    });
    let normalized_used = used_percent.or_else(|| {
        if scan.fixed32.is_empty() && resets_at.is_some() && has_usage_period {
            Some(0.0)
        } else {
            None
        }
    })?;
    Some(BillingWindow {
        used_percent: normalized_used,
        resets_at,
    })
}

#[derive(Clone)]
struct Fixed32Item {
    path: Vec<u32>,
    value: f32,
    order: u32,
}

#[derive(Clone)]
struct VarintItem {
    path: Vec<u32>,
    value: u64,
}

struct Scan {
    fixed32: Vec<Fixed32Item>,
    varints: Vec<VarintItem>,
}

fn scan_protobuf(bytes: &[u8], depth: u32, path: &[u32], mut order: u32, scan: &mut Scan) -> u32 {
    let mut index = 0;
    while index < bytes.len() {
        let field_start = index;
        let Some((key, next)) = read_varint(bytes, index) else {
            index = field_start + 1;
            continue;
        };
        if key == 0 {
            index = field_start + 1;
            continue;
        }
        index = next;
        let field_number = (key >> 3) as u32;
        let wire_type = (key & 0x07) as u32;
        let mut field_path = path.to_vec();
        field_path.push(field_number);

        match wire_type {
            0 => match read_varint(bytes, index) {
                Some((value, next)) => {
                    scan.varints.push(VarintItem {
                        path: field_path,
                        value,
                    });
                    index = next;
                }
                None => index = field_start + 1,
            },
            1 => {
                if index + 8 > bytes.len() {
                    return order;
                }
                index += 8;
            }
            2 => {
                let Some((length, next)) = read_varint(bytes, index) else {
                    index = field_start + 1;
                    continue;
                };
                index = next;
                let end = index + length as usize;
                if length as usize > bytes.len().saturating_sub(index) {
                    index = field_start + 1;
                    continue;
                }
                if depth < 4 {
                    order = scan_protobuf(&bytes[index..end], depth + 1, &field_path, order, scan);
                }
                index = end;
            }
            5 => {
                if index + 4 > bytes.len() {
                    return order;
                }
                let value = f32::from_le_bytes(bytes[index..index + 4].try_into().unwrap());
                scan.fixed32.push(Fixed32Item {
                    path: field_path,
                    value,
                    order,
                });
                order += 1;
                index += 4;
            }
            _ => index = field_start + 1,
        }
    }
    order
}

fn read_varint(bytes: &[u8], mut index: usize) -> Option<(u64, usize)> {
    let mut value = 0u64;
    let mut shift = 0;
    while index < bytes.len() && shift < 64 {
        let byte = bytes[index];
        index += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some((value, index));
        }
        shift += 7;
    }
    None
}

fn grpc_web_frames(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if index + 5 > bytes.len() {
            return Vec::new();
        }
        let flags = bytes[index];
        let length = u32::from_be_bytes(bytes[index + 1..index + 5].try_into().unwrap()) as usize;
        let start = index + 5;
        let end = start + length;
        if end > bytes.len() {
            return Vec::new();
        }
        if flags & 0x80 == 0 {
            frames.push(bytes[start..end].to_vec());
        }
        index = end;
    }
    frames
}

fn looks_like_protobuf(bytes: &[u8]) -> bool {
    let Some(&first) = bytes.first() else {
        return false;
    };
    let field_number = first >> 3;
    let wire_type = first & 0x07;
    field_number > 0 && matches!(wire_type, 0 | 1 | 2 | 5)
}

fn grpc_web_trailers(bytes: &[u8]) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    let mut index = 0;
    while index + 5 <= bytes.len() {
        let flags = bytes[index];
        let length = u32::from_be_bytes(bytes[index + 1..index + 5].try_into().unwrap()) as usize;
        let start = index + 5;
        let end = start + length;
        if end > bytes.len() {
            break;
        }
        if flags & 0x80 != 0 {
            let text = String::from_utf8_lossy(&bytes[start..end]);
            for line in text.split(['\n', '\r']) {
                let Some((name, value)) = line.split_once(':') else {
                    continue;
                };
                fields.push((
                    name.trim().to_ascii_lowercase(),
                    percent_decode(value.trim()),
                ));
            }
        }
        index = end;
    }
    fields
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (from_hex(bytes[index + 1]), from_hex(bytes[index + 2]))
        {
            out.push((high << 4) | low);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn from_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const CLI_FIXTURE: &[u8] = include_bytes!("../tests/fixtures/grok-credits.json");

    fn parse_cli(value: Value) -> Option<UsageSnapshot> {
        parse_cli_billing_payload(&serde_json::to_vec(&value).unwrap())
    }

    fn mock_options(server: &wiremock::MockServer) -> (tempfile::TempDir, UsageOptions) {
        let directory = tempfile::tempdir().unwrap();
        let auth_path = directory.path().join("auth.json");
        std::fs::write(
            &auth_path,
            json!({"https://auth.x.ai::fixture": {
                "key": "private-token", "refresh_token": "private-refresh",
                "expires_at": "2099-01-01T00:00:00Z", "auth_mode": "oidc"
            }})
            .to_string(),
        )
        .unwrap();
        (
            directory,
            UsageOptions {
                providers: vec![UsageProvider::Grok],
                grok_auth_path: Some(auth_path),
                grok_endpoint: Some(format!("{}/v1/billing?format=credits", server.uri())),
                grok_fallback_endpoint: Some(format!("{}/legacy", server.uri())),
                ..UsageOptions::default()
            },
        )
    }

    #[test]
    fn cli_real_shape_recognizes_omitted_proto3_zero_without_including_paid_credits() {
        let snapshot = parse_cli_billing_payload(CLI_FIXTURE).unwrap();
        assert_eq!(snapshot.used_percent, 0.0);
        assert_eq!(snapshot.remaining_percent, 100);
        assert_eq!(snapshot.resets_at, Some(1_788_825_600));
        assert_eq!(snapshot.window_duration_mins, Some(10080.0));
        assert_eq!(snapshot.plan_type, None);
    }

    #[test]
    fn cli_percent_is_authoritative_and_clamped_and_only_known_plans_are_projected() {
        let mut fixture: Value = serde_json::from_slice(CLI_FIXTURE).unwrap();
        fixture["config"]["monthlyLimit"] = json!({"val": 100});
        fixture["config"]["used"] = json!({"val": 99});
        fixture["subscription_tier"] = json!("SuperGrok Heavy");
        for (value, expected, remaining) in [(37.5, 37.5, 63), (-1.0, 0.0, 100), (120.0, 100.0, 0)]
        {
            fixture["config"]["creditUsagePercent"] = json!(value);
            let snapshot = parse_cli(fixture.clone()).unwrap();
            assert_eq!(snapshot.used_percent, expected);
            assert_eq!(snapshot.remaining_percent, remaining);
            assert_eq!(snapshot.plan_type.as_deref(), Some("SuperGrok Heavy"));
        }
        fixture["subscription_tier"] = json!("private@example.invalid");
        assert!(parse_cli(fixture).unwrap().plan_type.is_none());
    }

    #[test]
    fn cli_legacy_cents_support_numbers_strings_and_proto3_zero() {
        let mut fixture = json!({"config": {
            "monthlyLimit": {"val": "2000"}, "used": {"val": 500},
            "billingPeriodStart": "2026-09-01T00:00:00Z",
            "billingPeriodEnd": "2026-10-01T00:00:00Z",
            "onDemandUsed": {"val": 9000}
        }});
        let snapshot = parse_cli(fixture.clone()).unwrap();
        assert_eq!(snapshot.used_percent, 25.0);
        assert_eq!(snapshot.window_duration_mins, Some(43200.0));
        for used in [json!({}), json!({"val": "0"})] {
            fixture["config"]["used"] = used;
            assert_eq!(parse_cli(fixture.clone()).unwrap().used_percent, 0.0);
        }
        fixture["config"].as_object_mut().unwrap().remove("used");
        assert_eq!(parse_cli(fixture.clone()).unwrap().used_percent, 0.0);
        for limit in [
            json!({}),
            json!({"val": 0}),
            json!({"val": -1}),
            json!({"val": "NaN"}),
        ] {
            fixture["config"]["monthlyLimit"] = limit;
            assert!(parse_cli(fixture.clone()).is_none());
        }
    }

    #[test]
    fn cli_does_not_turn_unknown_or_malformed_responses_into_full_allowance() {
        for value in [
            json!({}),
            json!({"config": null}),
            json!({"config": {}}),
            json!({"config": {"creditUsagePercent": null}}),
            json!({"config": {"creditUsagePercent": "25"}}),
            json!({"config": {"used": {"val": 100}}}),
            json!({"config": {"monthlyLimit": {"val": 100}, "used": {"other": 1}}}),
        ] {
            assert!(parse_cli(value).is_none());
        }
        for (field, invalid) in [
            ("type", "unknown"),
            ("start", "2026-02-30T00:00:00Z"),
            ("end", "2026-08-01T00:00:00Z"),
            ("end", "not-a-date"),
        ] {
            let mut fixture: Value = serde_json::from_slice(CLI_FIXTURE).unwrap();
            fixture["config"]["currentPeriod"][field] = json!(invalid);
            assert!(parse_cli(fixture).is_none());
        }
        assert!(parse_cli_billing_payload(b"<html>private-response</html>").is_none());
    }

    #[test]
    fn cli_period_uses_offsets_and_never_mixes_current_and_deprecated_dates() {
        let mut fixture = json!({"config": {
            "creditUsagePercent": 25,
            "currentPeriod": {"start": "2026-09-01T08:00:00.123456+08:00",
                "end": "2026-09-08T08:00:00.123456+08:00"},
            "billingPeriodStart": "2026-08-01T00:00:00Z",
            "billingPeriodEnd": "2026-10-01T00:00:00Z"
        }});
        let snapshot = parse_cli(fixture.clone()).unwrap();
        assert_eq!(snapshot.resets_at, Some(1_788_825_600));
        assert_eq!(snapshot.window_duration_mins, Some(10080.0));
        fixture["config"]["currentPeriod"]
            .as_object_mut()
            .unwrap()
            .remove("start");
        assert_eq!(parse_cli(fixture).unwrap().window_duration_mins, None);
    }

    #[test]
    fn expiry_respects_timezone_and_exact_deadline() {
        for expires in ["2026-09-01T08:00:00+08:00", "2026-09-01T00:00:00Z"] {
            let text =
                json!({"https://auth.x.ai::fixture": {"key": "fixture", "expires_at": expires}})
                    .to_string();
            assert!(parse_grok_auth(&text, 1_788_220_800_000).unwrap().expired);
            assert!(!parse_grok_auth(&text, 1_788_220_799_999).unwrap().expired);
        }
    }

    #[test]
    fn bearer_headers_are_sensitive() {
        let auth = GrokAuth {
            token: GrokToken("private-token".into()),
            expired: false,
        };
        let headers = auth_headers(&auth, &UsageOptions::default()).unwrap();
        assert!(headers[reqwest::header::AUTHORIZATION].is_sensitive());
        assert!(!format!("{headers:?}").contains("private-token"));
    }

    #[test]
    fn http_authentication_is_distinct_from_cloudflare_and_other_denials() {
        let empty = HeaderMap::new();
        for (status, body, expected) in [
            (401, "", LOGIN_REQUIRED),
            (
                403,
                "<!DOCTYPE html><html><div id='cf-error-details'>Attention Required</div>",
                CLOUDFLARE_BLOCKED,
            ),
            (403, "<html>Forbidden</html>", CLOUDFLARE_BLOCKED),
            (403, "cf-chl-challenge", CLOUDFLARE_BLOCKED),
            (403, r#"{"error":"bad-credentials"}"#, LOGIN_REQUIRED),
            (
                403,
                r#"{"error":{"code":"unauthenticated"}}"#,
                LOGIN_REQUIRED,
            ),
            (
                403,
                r#"{"error":"permission denied for this access token"}"#,
                UNAVAILABLE,
            ),
            (403, r#"{"error":"billing disabled"}"#, UNAVAILABLE),
            (429, "private-body", UNAVAILABLE),
            (500, "private-body", UNAVAILABLE),
            (302, "", UNAVAILABLE),
            (503, "<html>Cloudflare</html>", UNAVAILABLE),
        ] {
            assert_eq!(
                response_failure(status, &empty, body.as_bytes()).as_deref(),
                Some(expected)
            );
        }
        for (key, value) in [
            ("server", "cloudflare"),
            ("cf-ray", "redacted"),
            ("content-type", "text/html; charset=UTF-8"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(
                HeaderName::from_bytes(key.as_bytes()).unwrap(),
                HeaderValue::from_static(value),
            );
            assert_eq!(
                response_failure(403, &headers, b"").as_deref(),
                Some(CLOUDFLARE_BLOCKED)
            );
        }
        assert_eq!(response_failure(200, &empty, CLI_FIXTURE), None);
    }

    #[test]
    fn grpc_authentication_checks_headers_and_trailers_without_broad_token_matches() {
        for (status, message, expected) in [
            (16, "anything", LOGIN_REQUIRED),
            (7, "bad-credentials", LOGIN_REQUIRED),
            (7, "unauthenticated", LOGIN_REQUIRED),
            (7, "access token lacks this permission", UNAVAILABLE),
            (7, "permission denied", UNAVAILABLE),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("grpc-status", status.to_string().parse().unwrap());
            headers.insert("grpc-message", message.parse().unwrap());
            assert_eq!(
                response_failure(200, &headers, &[]).as_deref(),
                Some(expected)
            );
            let trailer = grpc_frame(
                0x80,
                format!("grpc-status: {status}\r\ngrpc-message: {message}\r\n").as_bytes(),
            );
            assert_eq!(
                response_failure(200, &HeaderMap::new(), &trailer).as_deref(),
                Some(expected)
            );
        }
    }

    #[tokio::test]
    async fn cli_get_precedes_legacy_and_keeps_auth_and_paid_balances_out_of_reports() {
        use wiremock::matchers::{header, method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let (_dir, options) = mock_options(&server);
        let before = std::fs::read(options.grok_auth_path.as_ref().unwrap()).unwrap();
        Mock::given(method("GET"))
            .and(path("/v1/billing"))
            .and(query_param("format", "credits"))
            .and(header("authorization", "Bearer private-token"))
            .and(header("x-xai-token-auth", "xai-grok-cli"))
            .and(header("accept", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(CLI_FIXTURE))
            .expect(1)
            .mount(&server)
            .await;
        let report = crate::read_usage(&options).await;
        assert_eq!(report.providers[0].status, crate::UsageStatus::Ready);
        assert_eq!(report.providers[0].remaining_percent, Some(100));
        let encoded = serde_json::to_string(&report).unwrap();
        for forbidden in [
            "private-token",
            "private-refresh",
            "prepaid",
            "onDemand",
            "topUp",
            "windows",
        ] {
            assert!(!encoded.contains(forbidden));
        }
        assert_eq!(
            before,
            std::fs::read(options.grok_auth_path.as_ref().unwrap()).unwrap()
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].body.is_empty());
        assert!(!requests[0].headers.contains_key("x-userid"));
        assert!(!requests[0].headers.contains_key("x-grok-client-version"));
    }

    #[tokio::test]
    async fn rejected_auth_does_not_fall_back_or_modify_credentials() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let (_dir, options) = mock_options(&server);
        let before = std::fs::read(options.grok_auth_path.as_ref().unwrap()).unwrap();
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401).set_body_string("private-response"))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            read_grok_usage(&options).await.detail.as_deref(),
            Some(LOGIN_REQUIRED)
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        assert_eq!(
            before,
            std::fs::read(options.grok_auth_path.as_ref().unwrap()).unwrap()
        );
    }

    #[tokio::test]
    async fn expired_auth_does_not_make_any_request_or_refresh() {
        let server = wiremock::MockServer::start().await;
        let (_dir, options) = mock_options(&server);
        let path = options.grok_auth_path.as_ref().unwrap();
        let mut auth: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        auth["https://auth.x.ai::fixture"]["expires_at"] = json!("2000-01-01T00:00:00Z");
        let before = serde_json::to_vec(&auth).unwrap();
        std::fs::write(path, &before).unwrap();
        assert_eq!(
            read_grok_usage(&options).await.detail.as_deref(),
            Some(LOGIN_REQUIRED)
        );
        assert!(server.received_requests().await.unwrap().is_empty());
        assert_eq!(before, std::fs::read(path).unwrap());
    }

    #[tokio::test]
    async fn browser_fallback_cloudflare_does_not_request_login() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let (_dir, options) = mock_options(&server);
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/legacy"))
            .respond_with(ResponseTemplate::new(403).set_body_string(
                "<html><div id='cf-error-details'>Attention Required</div></html>",
            ))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            read_grok_usage(&options).await.detail.as_deref(),
            Some(CLOUDFLARE_BLOCKED)
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn cli_timeout_or_unrecognized_json_can_still_use_legacy_within_the_deadline() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        for delayed in [false, true] {
            let server = MockServer::start().await;
            let (_dir, mut options) = mock_options(&server);
            options.timeout = std::time::Duration::from_millis(300);
            let response = ResponseTemplate::new(200).set_body_string("{}");
            let response = if delayed {
                response.set_delay(std::time::Duration::from_secs(2))
            } else {
                response
            };
            Mock::given(method("GET"))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/legacy"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_bytes(grpc_frame(0, &field_message(1, &field_float(1, 12.25)))),
                )
                .expect(1)
                .mount(&server)
                .await;
            let report = crate::read_usage(&options).await;
            assert_eq!(report.providers[0].used_percent, Some(12.25));
            assert_eq!(server.received_requests().await.unwrap().len(), 2);
        }
    }

    #[tokio::test]
    async fn cli_redirects_are_not_followed_and_responses_are_bounded() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        for oversized in [false, true] {
            let server = MockServer::start().await;
            let (_dir, options) = mock_options(&server);
            let response = if oversized {
                ResponseTemplate::new(200).set_body_bytes(vec![0; MAX_GROK_RESPONSE_BYTES + 1])
            } else {
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/leak", server.uri()))
            };
            Mock::given(method("GET"))
                .and(path("/v1/billing"))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/legacy"))
                .respond_with(ResponseTemplate::new(503))
                .expect(1)
                .mount(&server)
                .await;
            let entry = read_grok_usage(&options).await;
            assert_eq!(entry.status, crate::UsageStatus::Unavailable);
            if oversized {
                assert!(entry.detail.unwrap().contains("2 MiB"));
            } else {
                assert_eq!(entry.detail.as_deref(), Some(UNAVAILABLE));
            }
            let requests = server.received_requests().await.unwrap();
            assert_eq!(requests.len(), 2);
            assert!(requests.iter().all(|r| r.url.path() != "/leak"));
        }
    }

    fn varint(mut remaining: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let mut byte = (remaining & 0x7f) as u8;
            remaining >>= 7;
            if remaining != 0 {
                byte |= 0x80;
            }
            bytes.push(byte);
            if remaining == 0 {
                break;
            }
        }
        bytes
    }

    fn field_varint(number: u32, value: u64) -> Vec<u8> {
        let mut bytes = varint(u64::from(number) << 3);
        bytes.extend(varint(value));
        bytes
    }

    fn field_float(number: u32, value: f32) -> Vec<u8> {
        let mut bytes = varint((u64::from(number) << 3) | 5);
        bytes.extend(value.to_le_bytes());
        bytes
    }

    fn field_message(number: u32, payload: &[u8]) -> Vec<u8> {
        let mut bytes = varint((u64::from(number) << 3) | 2);
        bytes.extend(varint(payload.len() as u64));
        bytes.extend_from_slice(payload);
        bytes
    }

    fn grpc_frame(flags: u8, payload: &[u8]) -> Vec<u8> {
        let mut header = vec![flags, 0, 0, 0, 0];
        header[1..5].copy_from_slice(&(payload.len() as u32).to_be_bytes());
        header.extend_from_slice(payload);
        header
    }

    #[test]
    fn auth_prefers_oidc_and_redacts_the_token() {
        let auth = parse_grok_auth(
            &json!({
                "https://accounts.x.ai/sign-in": { "key": "legacy-token" },
                "https://auth.x.ai::client": { "key": "oidc-token", "expires_at": "2099-01-01T00:00:00Z" }
            })
            .to_string(),
            1_750_000_000_000,
        )
        .unwrap();
        assert!(!auth.expired);
        assert_eq!(auth.token.0, "oidc-token");
        assert!(!format!("{:?}", auth.token).contains("oidc-token"));
        assert_eq!(format!("{:?}", GrokToken("secret".into())), "<redacted>");
    }

    #[test]
    fn billing_parser_reads_the_shallow_percent_and_exact_reset_path() {
        let now = 1_750_000_000;
        let reset = now + 30 * 86_400;
        let inner = [
            field_message(2, &field_float(1, 99.0)),
            field_float(1, 37.5),
            field_message(5, &field_varint(1, reset)),
        ]
        .concat();
        let framed = grpc_frame(0, &field_message(1, &inner));
        let parsed = parse_grok_billing_payload(&framed, now).unwrap();
        assert_eq!(parsed.used_percent, 37.5);
        assert_eq!(parsed.resets_at, Some(reset));
        assert_eq!(grpc_web_frames(&framed).len(), 1);
    }

    #[test]
    fn billing_parser_recognizes_proto3_zero_usage() {
        let now = 1_750_000_000;
        let reset = now + 7 * 86_400;
        let inner = [
            field_message(5, &field_varint(1, reset)),
            field_message(6, &field_varint(1, 3)),
        ]
        .concat();
        let parsed =
            parse_grok_billing_payload(&grpc_frame(0, &field_message(1, &inner)), now).unwrap();
        assert_eq!(parsed.used_percent, 0.0);
        assert_eq!(parsed.resets_at, Some(reset));
    }

    #[test]
    fn percent_decode_does_not_keep_raw_escape_sequences() {
        assert_eq!(percent_decode("no+personal%20team"), "no+personal team");
    }

    #[test]
    fn rfc3339_epoch_parses() {
        assert_eq!(super::parse_rfc3339_millis("1970-01-01T00:00:00Z"), Some(0));
    }

    #[tokio::test]
    async fn auth_files_must_be_regular_and_bounded() {
        use tempfile::tempdir;

        use crate::model::{UsageOptions, UsageProvider, UsageStatus};
        use crate::read_usage;

        let directory = tempdir().unwrap();
        let report = read_usage(&UsageOptions {
            providers: vec![UsageProvider::Grok],
            grok_auth_path: Some(directory.path().to_path_buf()),
            ..UsageOptions::default()
        })
        .await;
        assert_eq!(report.providers[0].status, UsageStatus::Unavailable);
        assert!(
            report.providers[0]
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("regular file")
        );

        let oversized = directory.path().join("oversized-auth.json");
        let file = std::fs::File::create(&oversized).unwrap();
        file.set_len(super::MAX_GROK_AUTH_BYTES + 1).unwrap();
        let report = read_usage(&UsageOptions {
            providers: vec![UsageProvider::Grok],
            grok_auth_path: Some(oversized),
            ..UsageOptions::default()
        })
        .await;
        assert_eq!(report.providers[0].status, UsageStatus::Unavailable);
        assert!(
            report.providers[0]
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("1 MiB")
        );
    }

    #[tokio::test]
    async fn grok_query_returns_a_credential_free_projection() {
        use serde_json::json;
        use tempfile::tempdir;
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        use crate::model::{UsageOptions, UsageProvider, UsageStatus};
        use crate::read_usage;

        let now = 1_750_000_000;
        let reset = now + 7 * 86_400;
        let inner = [
            field_float(1, 12.25),
            field_message(5, &field_varint(1, reset)),
        ]
        .concat();
        let payload = grpc_frame(0, &field_message(1, &inner));
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig"))
            .and(header("authorization", "Bearer private-token"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(payload))
            .mount(&server)
            .await;

        let directory = tempdir().unwrap();
        let auth_path = directory.path().join("auth.json");
        std::fs::write(
            &auth_path,
            json!({
                "https://auth.x.ai::client": {
                    "key": "private-token",
                    "expires_at": "2099-01-01T00:00:00Z"
                }
            })
            .to_string(),
        )
        .unwrap();

        let report = read_usage(&UsageOptions {
            providers: vec![UsageProvider::Grok],
            grok_auth_path: Some(auth_path),
            grok_endpoint: Some(format!("{}/v1/billing?format=credits", server.uri())),
            grok_fallback_endpoint: Some(format!(
                "{}/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig",
                server.uri()
            )),
            ..UsageOptions::default()
        })
        .await;
        assert_eq!(report.providers.len(), 1);
        assert_eq!(report.providers[0].status, UsageStatus::Ready);
        assert_eq!(report.providers[0].remaining_percent, Some(88));
        assert_eq!(report.providers[0].used_percent, Some(12.25));
        assert_eq!(report.providers[0].plan_type.as_deref(), Some("grok-build"));
        let encoded = serde_json::to_string(&report).unwrap();
        assert!(!encoded.contains("private-token"));
    }

    #[tokio::test]
    async fn grok_query_rejects_oversized_billing_responses() {
        use tempfile::tempdir;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        use crate::model::{UsageOptions, UsageProvider, UsageStatus};
        use crate::read_usage;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![
                0;
                super::MAX_GROK_RESPONSE_BYTES
                    + 1
            ]))
            .mount(&server)
            .await;

        let directory = tempdir().unwrap();
        let auth_path = directory.path().join("auth.json");
        std::fs::write(
            &auth_path,
            json!({
                "https://auth.x.ai::client": {
                    "key": "private-token",
                    "expires_at": "2099-01-01T00:00:00Z"
                }
            })
            .to_string(),
        )
        .unwrap();

        let report = read_usage(&UsageOptions {
            providers: vec![UsageProvider::Grok],
            grok_auth_path: Some(auth_path),
            grok_endpoint: Some(format!("{}/v1/billing?format=credits", server.uri())),
            grok_fallback_endpoint: Some(format!(
                "{}/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig",
                server.uri()
            )),
            ..UsageOptions::default()
        })
        .await;
        assert_eq!(report.providers[0].status, UsageStatus::Unavailable);
        assert!(
            report.providers[0]
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("2 MiB")
        );
    }
}
