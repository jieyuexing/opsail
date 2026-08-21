use std::env;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use tokio::io::AsyncReadExt;

use crate::model::{UsageEntry, UsageOptions, UsageProvider, UsageSnapshot};

const BILLING_ENDPOINT: &str = "https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig";
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
    let endpoint = options.grok_endpoint.as_deref().unwrap_or(BILLING_ENDPOINT);
    match query_billing(endpoint, &auth, options).await {
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
        .is_some_and(|expires| expires < now_ms);
    Ok(GrokAuth {
        token: GrokToken(key),
        expired,
    })
}

fn parse_rfc3339_millis(value: &str) -> Option<u64> {
    let value = value.trim();
    let (date, time_and_offset) = value.split_once('T')?;
    let time = time_and_offset
        .trim_end_matches('Z')
        .split(['+', '-'])
        .next()
        .unwrap_or(time_and_offset);
    let time = time.split('.').next()?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    let mut time_parts = time.split(':');
    let hour: u64 = time_parts.next()?.parse().ok()?;
    let minute: u64 = time_parts.next()?.parse().ok()?;
    let second: u64 = time_parts.next()?.parse().ok()?;
    let days = days_from_civil(year, month, day)?;
    let secs = days * 86_400 + hour as i64 * 3_600 + minute as i64 * 60 + second as i64;
    Some(u64::try_from(secs.max(0)).ok()? * 1_000)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || day == 0 || day > 31 {
        return None;
    }
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month_i = i64::from(month);
    let day_i = i64::from(day);
    let doy = (153 * (month_i + if month > 2 { -3 } else { 9 }) + 2) / 5 + day_i - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

async fn query_billing(
    endpoint: &str,
    auth: &GrokAuth,
    options: &UsageOptions,
) -> Result<UsageSnapshot, String> {
    INSTALL_TLS.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    let user_agent = format!("{}/{}", options.client.name, options.client.version);
    let client = reqwest::Client::builder()
        .timeout(options.timeout)
        .connect_timeout(options.timeout.min(std::time::Duration::from_secs(8)))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| unreachable_billing(auth.expired))?;

    let mut headers = HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", auth.token.0))
            .map_err(|_| unreachable_billing(auth.expired))?,
    );
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
    headers.insert(
        reqwest::header::USER_AGENT,
        HeaderValue::from_str(&user_agent).unwrap_or(HeaderValue::from_static("opsail")),
    );

    let response = client
        .post(endpoint)
        .headers(headers)
        .body(EMPTY_GRPC_WEB_FRAME.to_vec())
        .send()
        .await
        .map_err(|_| unreachable_billing(auth.expired))?;
    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return Err("Grok sign-in is no longer valid; run `grok login`".to_owned());
    }
    if !(200..300).contains(&status) {
        return Err("the Grok billing endpoint is temporarily unavailable".to_owned());
    }

    let header_map = response.headers().clone();
    if let Some(failure) = grpc_header_failure(&header_map) {
        return Err(failure);
    }
    let bytes = read_bounded_response(response).await?;
    if let Some(failure) = grpc_trailer_failure(&bytes) {
        return Err(failure);
    }
    let now_seconds = now_millis() / 1_000;
    let parsed = parse_grok_billing_payload(&bytes, now_seconds)
        .ok_or_else(|| "the Grok billing response was not recognized".to_owned())?;
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

fn unreachable_billing(expired: bool) -> String {
    if expired {
        "Grok sign-in may have expired; run `grok login`".to_owned()
    } else {
        "could not reach the Grok billing endpoint".to_owned()
    }
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
    if status == 16
        || (status == 7
            && (normalized.contains("bad-credentials")
                || normalized.contains("unauthenticated")
                || normalized.contains("access token")))
    {
        return "Grok sign-in is no longer valid; run `grok login`".to_owned();
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

    use super::{
        GrokToken, grpc_web_frames, parse_grok_auth, parse_grok_billing_payload, percent_decode,
    };

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
            grok_endpoint: Some(format!(
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
            grok_endpoint: Some(format!(
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
