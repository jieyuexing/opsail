//! Read Claude Code's existing login and the official subscription usage endpoint.
//! No login, token refresh, credential writes, or raw-response diagnostics.

use std::env;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use reqwest::header::HeaderValue;
use serde_json::Value;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::io::AsyncReadExt;
use unicode_normalization::UnicodeNormalization;

use crate::model::{UsageEntry, UsageOptions, UsageProvider, UsageStatus, UsageWindow};

const USAGE_ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const MAX_AUTH_BYTES: u64 = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const LOGIN_REQUIRED: &str =
    "Claude Code sign-in is missing or no longer valid; run `claude auth login`";
const INVALID_AUTH: &str = "the Claude Code credentials are not usable; run `claude auth login`";
const INVALID_RESPONSE: &str = "the Claude usage response was not recognized";
const ENDPOINT_UNAVAILABLE: &str = "the Claude usage endpoint is temporarily unavailable";

#[derive(Debug)]
struct ClaudeAuth {
    // HeaderValue's Debug is redacted by set_sensitive(true).
    authorization: HeaderValue,
    plan_type: Option<String>,
}

pub(crate) async fn read_claude_usage(options: &UsageOptions) -> UsageEntry {
    match read_and_query(options).await {
        Ok(entry) => entry,
        Err(detail) => UsageEntry::unavailable(UsageProvider::Claude, detail),
    }
}

async fn read_and_query(options: &UsageOptions) -> Result<UsageEntry, &'static str> {
    let content = read_credentials(options).await?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| INVALID_AUTH)?
        .as_millis();
    let auth = parse_auth(&content, now_ms)?;
    let endpoint = USAGE_ENDPOINT;
    #[cfg(test)]
    let endpoint = options.claude_endpoint.as_deref().unwrap_or(endpoint);
    query_usage(endpoint, &auth, options).await
}

async fn read_credentials(options: &UsageOptions) -> Result<Vec<u8>, &'static str> {
    // An explicit file is exclusive: never fall back to another account or Keychain.
    let explicit = options.claude_auth_path.clone().or_else(|| {
        env::var_os("OPSAIL_CLAUDE_AUTH")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    });
    if let Some(path) = explicit {
        return read_auth_file(&path).await;
    }
    let configured = env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| value.nfc().collect::<String>());
    #[cfg(target_os = "macos")]
    if let Some(content) = read_keychain(&keychain_service(configured.as_deref())).await? {
        return Ok(content);
    }
    let directory = configured.map(PathBuf::from).or_else(|| {
        env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(|home| PathBuf::from(home).join(".claude"))
    });
    read_auth_file(&directory.ok_or(LOGIN_REQUIRED)?.join(".credentials.json")).await
}

#[cfg(any(target_os = "macos", test))]
fn keychain_service(configured: Option<&str>) -> String {
    match configured {
        None => "Claude Code-credentials".to_owned(),
        Some(directory) => {
            // Matches Claude Code: SHA-256 of the NFC config-directory string, first 8 hex.
            let normalized = directory.nfc().collect::<String>();
            let digest = ring::digest::digest(&ring::digest::SHA256, normalized.as_bytes());
            let suffix: String = digest.as_ref()[..4]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            format!("Claude Code-credentials-{suffix}")
        }
    }
}

#[cfg(target_os = "macos")]
async fn read_keychain(service: &str) -> Result<Option<Vec<u8>>, &'static str> {
    use std::process::Stdio;
    use tokio::process::Command;

    let mut command = Command::new("/usr/bin/security");
    command.args(["find-generic-password", "-s", service, "-w"]);
    if let Some(account) = env::var_os("USER").filter(|value| !value.is_empty()) {
        command.arg("-a").arg(account);
    }
    let Ok(mut child) = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .ok_or(INVALID_AUTH)?
        .take(MAX_AUTH_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| INVALID_AUTH)?;
    if bytes.len() as u64 > MAX_AUTH_BYTES {
        return Err("the Claude Code credential exceeded the 1 MiB safety limit");
    }
    if !child.wait().await.map_err(|_| INVALID_AUTH)?.success() {
        // Claude Code may have used its file fallback while Keychain was locked.
        return Ok(None);
    }
    Ok(Some(bytes))
}

async fn read_auth_file(path: &Path) -> Result<Vec<u8>, &'static str> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|_| LOGIN_REQUIRED)?;
    if !metadata.is_file() {
        return Err("the Claude Code sign-in path is not a regular file");
    }
    if metadata.len() > MAX_AUTH_BYTES {
        return Err("the Claude Code credential exceeded the 1 MiB safety limit");
    }
    let mut bytes = Vec::new();
    tokio::fs::File::open(path)
        .await
        .map_err(|_| LOGIN_REQUIRED)?
        .take(MAX_AUTH_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| INVALID_AUTH)?;
    if bytes.len() as u64 > MAX_AUTH_BYTES {
        return Err("the Claude Code credential exceeded the 1 MiB safety limit");
    }
    Ok(bytes)
}

fn parse_auth(content: &[u8], now_ms: u128) -> Result<ClaudeAuth, &'static str> {
    let text = std::str::from_utf8(content)
        .map_err(|_| INVALID_AUTH)?
        .trim();
    // `security -w` can hex-encode a password containing non-ASCII characters.
    let decoded;
    let content = if !text.starts_with('{')
        && !text.is_empty()
        && text.len().is_multiple_of(2)
        && text.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        decoded = (0..text.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&text[index..index + 2], 16).map_err(|_| INVALID_AUTH))
            .collect::<Result<Vec<_>, _>>()?;
        decoded.as_slice()
    } else {
        text.as_bytes()
    };
    let root: Value = serde_json::from_slice(content).map_err(|_| INVALID_AUTH)?;
    let oauth = root.get("claudeAiOauth").ok_or(LOGIN_REQUIRED)?;
    let token = oauth
        .get("accessToken")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .ok_or(LOGIN_REQUIRED)?;
    let expires = oauth
        .get("expiresAt")
        .and_then(Value::as_u64)
        .ok_or(INVALID_AUTH)?;
    if u128::from(expires) <= now_ms {
        return Err(LOGIN_REQUIRED);
    }
    if let Some(scopes) = oauth.get("scopes")
        && !scopes
            .as_array()
            .is_some_and(|scopes| scopes.iter().any(|scope| scope == "user:profile"))
    {
        return Err("Claude Code sign-in lacks the usage scope; run `claude auth login`");
    }
    let mut authorization =
        HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| INVALID_AUTH)?;
    authorization.set_sensitive(true);
    // Never project arbitrary credential strings into output.
    let plan_type = oauth
        .get("subscriptionType")
        .and_then(Value::as_str)
        .filter(|plan| matches!(*plan, "free" | "pro" | "max" | "team" | "enterprise"))
        .map(str::to_owned);
    Ok(ClaudeAuth {
        authorization,
        plan_type,
    })
}

async fn query_usage(
    endpoint: &str,
    auth: &ClaudeAuth,
    options: &UsageOptions,
) -> Result<UsageEntry, &'static str> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(options.timeout)
        .build()
        .map_err(|_| ENDPOINT_UNAVAILABLE)?;
    let response = client
        .get(endpoint)
        .header(reqwest::header::AUTHORIZATION, auth.authorization.clone())
        .header("anthropic-beta", "oauth-2025-04-20")
        .header(reqwest::header::ACCEPT, "application/json")
        .header(
            reqwest::header::USER_AGENT,
            concat!("opsail/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .map_err(|_| ENDPOINT_UNAVAILABLE)?;
    match response.status().as_u16() {
        200 => {}
        401 | 403 => return Err(LOGIN_REQUIRED),
        429 => return Err("the Claude usage endpoint is rate limited; try again later"),
        _ => return Err(ENDPOINT_UNAVAILABLE),
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err("the Claude usage response exceeded the 2 MiB safety limit");
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ENDPOINT_UNAVAILABLE)?;
        if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(bytes.len()) {
            return Err("the Claude usage response exceeded the 2 MiB safety limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    parse_usage(&bytes, auth.plan_type.clone())
}

fn parse_usage(bytes: &[u8], plan_type: Option<String>) -> Result<UsageEntry, &'static str> {
    let root: Value = serde_json::from_slice(bytes).map_err(|_| INVALID_RESPONSE)?;
    let object = root.as_object().ok_or(INVALID_RESPONSE)?;
    let mut windows = match object.get("limits") {
        Some(Value::Array(limits)) if !limits.is_empty() => parse_limits(limits)?,
        None | Some(Value::Array(_)) => parse_legacy_windows(object)?,
        Some(_) => return Err(INVALID_RESPONSE),
    };
    // Keep stable ordering and the legacy model-only fallback, independent of limits order.
    windows.sort_by(|a, b| a.id.cmp(&b.id));
    let primary = windows
        .iter()
        .find(|window| window.id == "five_hour")
        .or_else(|| windows.iter().find(|window| window.id == "seven_day"))
        .or_else(|| windows.first())
        .ok_or(INVALID_RESPONSE)?;
    Ok(UsageEntry {
        provider: UsageProvider::Claude,
        status: UsageStatus::Ready,
        remaining_percent: Some(primary.remaining_percent),
        used_percent: Some(primary.used_percent),
        resets_at: primary.resets_at,
        window_duration_mins: Some(primary.window_duration_mins),
        plan_type,
        reset_credit_available_count: None,
        reset_credit_expires_at: None,
        detail: None,
        windows: Some(windows),
    })
}

fn parse_limits(limits: &[Value]) -> Result<Vec<UsageWindow>, &'static str> {
    let mut windows = Vec::new();
    for limit in limits {
        let (id, label, duration) = match limit.get("kind").and_then(Value::as_str) {
            Some("session") => ("five_hour".to_owned(), None, 300.0),
            Some("weekly_all") => ("seven_day".to_owned(), None, 10_080.0),
            Some("weekly_scoped") => {
                let Some(label) = limit
                    .pointer("/scope/model/display_name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.trim().is_empty())
                else {
                    continue;
                };
                let model: String = label
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() {
                            c.to_ascii_lowercase()
                        } else {
                            '_'
                        }
                    })
                    .collect();
                (
                    format!("seven_day_{model}"),
                    Some(label.to_owned()),
                    10_080.0,
                )
            }
            _ => continue,
        };
        let used = limit
            .get("percent")
            .and_then(Value::as_f64)
            .ok_or(INVALID_RESPONSE)?;
        windows.push(parse_window(id, label, duration, used, limit)?);
    }
    Ok(windows)
}

fn parse_legacy_windows(
    object: &serde_json::Map<String, Value>,
) -> Result<Vec<UsageWindow>, &'static str> {
    let mut windows = Vec::new();
    for (id, value) in object {
        let duration = if id == "five_hour" {
            300.0
        } else if id == "seven_day"
            || (id.starts_with("seven_day_")
                && id.len() > "seven_day_".len()
                && id.len() <= 64
                && id
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'))
        {
            10_080.0
        } else {
            continue;
        };
        // Metadata such as seven_day_breakdown is not a quota window.
        let used = match value.get("utilization") {
            None | Some(Value::Null) => continue,
            Some(value) => value.as_f64().ok_or(INVALID_RESPONSE)?,
        };
        windows.push(parse_window(id.clone(), None, duration, used, value)?);
    }
    Ok(windows)
}

fn parse_window(
    id: String,
    label: Option<String>,
    duration: f64,
    used: f64,
    value: &Value,
) -> Result<UsageWindow, &'static str> {
    if !used.is_finite() {
        return Err(INVALID_RESPONSE);
    }
    let used = used.clamp(0.0, 100.0);
    let resets_at = match value.get("resets_at") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let time = OffsetDateTime::parse(value.as_str().ok_or(INVALID_RESPONSE)?, &Rfc3339)
                .map_err(|_| INVALID_RESPONSE)?;
            Some(u64::try_from(time.unix_timestamp()).map_err(|_| INVALID_RESPONSE)?)
        }
    };
    Ok(UsageWindow {
        id,
        label,
        remaining_percent: (100.0 - used).round() as u8,
        used_percent: used,
        resets_at,
        window_duration_mins: duration,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn credentials() -> Value {
        json!({"claudeAiOauth": {
            "accessToken": "private-access-token",
            "refreshToken": "private-refresh-token",
            "expiresAt": 4_070_908_800_000u64,
            "subscriptionType": "max",
            "scopes": ["user:inference", "user:profile"]
        }})
    }

    fn payload() -> Value {
        json!({
            "five_hour": {"utilization": 37.6, "resets_at": "2026-09-24T18:00:00.123+08:00"},
            "seven_day": {"utilization": 84, "resets_at": "2026-09-30T10:00:00Z"},
            "seven_day_opus": null,
            "seven_day_sonnet": {"utilization": 9, "resets_at": null},
            "seven_day_fable": {"utilization": 19, "resets_at": "2026-09-30T05:00:00-05:00"},
            "extra_usage": {"utilization": 1, "used_credits": 100},
            "account": "private-account-field"
        })
    }

    fn structured_payload() -> Value {
        // Credential-free shape captured from the official endpoint; values are examples.
        json!({
            "five_hour": {"utilization": 9.0, "resets_at": "2026-09-24T02:30:00.390086+00:00", "limit_dollars": null, "used_dollars": null, "remaining_dollars": null, "locked_reason": null},
            "seven_day": {"utilization": 32.0, "resets_at": "2026-09-24T11:00:00.390107+00:00", "limit_dollars": null, "used_dollars": null, "remaining_dollars": null, "locked_reason": null},
            "seven_day_oauth_apps": null,
            "seven_day_opus": null,
            "seven_day_sonnet": null,
            "seven_day_cowork": null,
            "seven_day_omelette": null,
            "tangelo": null,
            "nimbus_quill": {"utilization": 0.0, "resets_at": null, "limit_dollars": null, "used_dollars": null, "remaining_dollars": null, "locked_reason": null},
            "extra_usage": {"is_enabled": false, "monthly_limit": null, "used_credits": null, "utilization": null, "currency": null, "decimal_places": null, "disabled_reason": null, "user_disabled": false, "spend_limit_reached": false, "credits_ever_enabled": false, "daily": null, "weekly": null},
            "limits": [
                {"kind": "session", "group": "session", "percent": 9, "severity": "normal", "resets_at": "2026-09-24T02:30:00.390086+00:00", "scope": null, "is_active": false},
                {"kind": "weekly_all", "group": "weekly", "percent": 32, "severity": "normal", "resets_at": "2026-09-24T11:00:00.390107+00:00", "scope": null, "is_active": true},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 29, "severity": "normal", "resets_at": "2026-09-24T11:00:00.390276+00:00", "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null}, "is_active": false}
            ],
            "spend": {"used": {"amount_minor": 0, "currency": "USD", "exponent": 2}, "limit": null, "percent": 0, "severity": "normal", "enabled": false, "disabled_reason": null, "cap": null, "balance": null, "auto_reload": null, "disclaimer": "…", "can_purchase_credits": false, "can_toggle": false},
            "member_dashboard_available": false,
            "seven_day_breakdown": {"as_of": "2026-09-24T01:00:00.000000+00:00", "window_started_at": "2026-09-17T11:00:00.000000+00:00", "rows": [{"key": "claude_code", "display_name": "Claude Code", "percent": 100}]}
        })
    }

    #[test]
    fn structured_limits_keep_three_windows_labels_and_microsecond_resets() {
        let entry = parse_usage(&serde_json::to_vec(&structured_payload()).unwrap(), None).unwrap();
        assert_eq!(entry.status, UsageStatus::Ready);
        assert_eq!(entry.remaining_percent, Some(91));
        assert_eq!(entry.used_percent, Some(9.0));
        assert_eq!(entry.resets_at, Some(1_790_217_000));
        assert_eq!(entry.window_duration_mins, Some(300.0));
        let report = crate::UsageReport {
            schema_version: crate::SCHEMA_VERSION,
            providers: vec![entry],
        };
        let output = serde_json::to_value(report).unwrap();
        assert_eq!(output["schemaVersion"], 1);
        assert_eq!(
            output["providers"][0]["windows"],
            json!([
                {"id": "five_hour", "remainingPercent": 91, "usedPercent": 9.0, "resetsAt": 1_790_217_000, "windowDurationMins": 300.0},
                {"id": "seven_day", "remainingPercent": 68, "usedPercent": 32.0, "resetsAt": 1_790_247_600, "windowDurationMins": 10_080.0},
                {"id": "seven_day_fable", "label": "Fable", "remainingPercent": 71, "usedPercent": 29.0, "resetsAt": 1_790_247_600, "windowDurationMins": 10_080.0}
            ])
        );
    }

    #[test]
    fn structured_limits_override_legacy_values_and_order_does_not_choose_primary() {
        let mut value = structured_payload();
        value["five_hour"]["utilization"] = json!("invalid-legacy-value");
        value["seven_day"]["utilization"] = json!(99);
        value["limits"].as_array_mut().unwrap().reverse();
        let entry = parse_usage(&serde_json::to_vec(&value).unwrap(), None).unwrap();
        assert_eq!(entry.used_percent, Some(9.0));
        assert_eq!(entry.windows.unwrap()[1].used_percent, 32.0);

        value["limits"].as_array_mut().unwrap().pop(); // Remove session, keep weekly windows.
        let entry = parse_usage(&serde_json::to_vec(&value).unwrap(), None).unwrap();
        assert_eq!(entry.used_percent, Some(32.0));
        assert_eq!(entry.remaining_percent, Some(68));
        assert_eq!(entry.resets_at, Some(1_790_247_600));
        assert_eq!(entry.window_duration_mins, Some(10_080.0));
    }

    #[test]
    fn unknown_kinds_and_scopes_without_a_model_name_are_ignored() {
        let mut value = structured_payload();
        let limits = value["limits"].as_array_mut().unwrap();
        limits.push(json!({"kind": "future_limit", "percent": "invalid", "resets_at": "invalid"}));
        for scope in [
            Value::Null,
            json!({"surface": "cli"}),
            json!({"model": {"display_name": ""}}),
        ] {
            limits.push(json!({"kind": "weekly_scoped", "scope": scope}));
        }
        let entry = parse_usage(&serde_json::to_vec(&value).unwrap(), None).unwrap();
        assert_eq!(entry.windows.unwrap().len(), 3);
    }

    #[test]
    fn model_ids_are_normalized_and_offset_resets_preserve_the_instant() {
        let mut value = structured_payload();
        value["limits"][2]["scope"]["model"]["display_name"] = json!("Fable 5.5/Pro");
        value["limits"][2]["resets_at"] = json!("2026-09-24T19:00:00.390276+08:00");
        let entry = parse_usage(&serde_json::to_vec(&value).unwrap(), None).unwrap();
        let windows = entry.windows.unwrap();
        assert_eq!(windows[2].id, "seven_day_fable_5_5_pro");
        assert_eq!(windows[2].label.as_deref(), Some("Fable 5.5/Pro"));
        assert_eq!(windows[2].resets_at, Some(1_790_247_600));
    }

    #[test]
    fn legacy_fallback_skips_breakdown_null_utilization_and_unrecognized_keys() {
        let mut value = structured_payload();
        value.as_object_mut().unwrap().remove("limits");
        value["seven_day_sonnet"] = json!({"utilization": null, "resets_at": "invalid"});
        value["seven_day_fable"] = json!({"utilization": 29});
        value["seven_day_"] = json!({"utilization": 99});
        value["seven_day_breakdown"]["resets_at"] = json!("invalid");
        for empty_limits in [false, true] {
            if empty_limits {
                value["limits"] = json!([]);
            }
            let entry = parse_usage(&serde_json::to_vec(&value).unwrap(), None).unwrap();
            assert_eq!(entry.used_percent, Some(9.0));
            let windows = entry.windows.unwrap();
            assert_eq!(
                windows.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(),
                ["five_hour", "seven_day", "seven_day_fable"]
            );
            assert!(windows.iter().all(|w| w.label.is_none()));
        }
    }

    #[test]
    fn malformed_structured_limits_do_not_fall_back_to_legacy_windows() {
        for limits in [
            Value::Null,
            json!({}),
            json!([{"kind": "future_limit"}]),
            json!([{"kind": "session", "percent": null}]),
            json!([{"kind": "session", "percent": "9"}]),
            json!([{"kind": "session", "percent": 9, "resets_at": "invalid"}]),
        ] {
            let mut value = structured_payload();
            value["limits"] = limits;
            assert_eq!(
                parse_usage(&serde_json::to_vec(&value).unwrap(), None).unwrap_err(),
                INVALID_RESPONSE
            );
        }
    }

    #[test]
    fn parses_all_windows_and_keeps_primary_fields_in_schema_one() {
        let entry =
            parse_usage(&serde_json::to_vec(&payload()).unwrap(), Some("max".into())).unwrap();
        assert_eq!(entry.remaining_percent, Some(62));
        assert_eq!(entry.used_percent, Some(37.6));
        assert_eq!(entry.window_duration_mins, Some(300.0));
        assert_eq!(entry.resets_at, Some(1_790_244_000));
        let windows = entry.windows.as_ref().unwrap();
        assert_eq!(
            windows.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(),
            [
                "five_hour",
                "seven_day",
                "seven_day_fable",
                "seven_day_sonnet"
            ]
        );
        assert_eq!(windows[1].window_duration_mins, 10_080.0);
        assert_eq!(windows[1].resets_at, windows[2].resets_at);
        assert_eq!(windows[3].resets_at, None);
        assert!(windows.iter().all(|window| window.label.is_none()));
        let report = crate::UsageReport {
            schema_version: 1,
            providers: vec![entry],
        };
        let output = serde_json::to_string(&report).unwrap();
        assert!(!output.contains("private-account"));
        assert!(!output.contains("extra_usage"));
        assert!(!output.contains("resetCredit"));
    }

    #[test]
    fn weekly_fallback_zero_and_clamping_are_explicit() {
        let entry =
            parse_usage(br#"{"five_hour":null,"seven_day":{"utilization":0}}"#, None).unwrap();
        assert_eq!(entry.remaining_percent, Some(100));
        assert_eq!(entry.window_duration_mins, Some(10_080.0));
        for (used, expected) in [(-1, 100), (101, 0)] {
            let entry = parse_usage(
                &serde_json::to_vec(&json!({"five_hour":{"utilization":used}})).unwrap(),
                None,
            )
            .unwrap();
            assert_eq!(entry.remaining_percent, Some(expected));
        }
    }

    #[test]
    fn malformed_payloads_and_dates_fail_without_echoing_content() {
        for payload in [
            "private-invalid-json",
            "{}",
            "[]",
            r#"{"five_hour":null}"#,
            r#"{"five_hour":{"utilization":"private-token"}}"#,
            r#"{"five_hour":{"utilization":1,"resets_at":"2026-02-30T00:00:00Z"}}"#,
            r#"{"five_hour":{"utilization":1,"resets_at":"2026-09-24T25:00:00Z"}}"#,
            r#"{"five_hour":{"utilization":1,"resets_at":"1960-01-01T00:00:00Z"}}"#,
            r#"{"five_hour":{"utilization":null}}"#,
            r#"{"five_hour":{"utilization":1},"seven_day":{"utilization":"32"}}"#,
            r#"{"five_hour":{"utilization":true}}"#,
            r#"{"five_hour":{"utilization":[]}}"#,
        ] {
            assert_eq!(
                parse_usage(payload.as_bytes(), None).unwrap_err(),
                INVALID_RESPONSE
            );
        }
    }

    #[test]
    fn credentials_and_debug_are_redacted_and_only_known_plans_are_projected() {
        let mut value = credentials();
        let bytes = serde_json::to_vec(&value).unwrap();
        let auth = parse_auth(&bytes, 1).unwrap();
        assert_eq!(auth.plan_type.as_deref(), Some("max"));
        assert!(!format!("{auth:?}").contains("private-"));
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            parse_auth(hex.as_bytes(), 1).unwrap().plan_type,
            auth.plan_type
        );
        value["claudeAiOauth"]["subscriptionType"] = json!("private-plan-string");
        assert_eq!(
            parse_auth(&serde_json::to_vec(&value).unwrap(), 1)
                .unwrap()
                .plan_type,
            None
        );
    }

    #[test]
    fn invalid_expired_and_wrong_scope_credentials_require_user_login() {
        for (field, value) in [
            ("accessToken", json!("")),
            ("accessToken", json!("private\ntoken")),
            ("expiresAt", json!(1)),
            ("expiresAt", json!("private-token")),
            ("scopes", json!(["user:inference"])),
        ] {
            let mut auth = credentials();
            auth["claudeAiOauth"][field] = value;
            let error = parse_auth(&serde_json::to_vec(&auth).unwrap(), 1).unwrap_err();
            assert!(error.contains("claude auth login"));
            assert!(!error.contains("private"));
        }
        for bytes in [
            b"private-invalid-json".as_slice(),
            b"{}",
            b"{\"apiKey\":\"private-key\"}",
        ] {
            assert!(
                parse_auth(bytes, 1)
                    .unwrap_err()
                    .contains("claude auth login")
            );
        }
    }

    #[test]
    fn keychain_namespace_matches_claude_config_directory() {
        assert_eq!(keychain_service(None), "Claude Code-credentials");
        assert_eq!(
            keychain_service(Some("test")),
            "Claude Code-credentials-9f86d081"
        );
        assert_eq!(
            keychain_service(Some("/test/caf\u{e9}")),
            keychain_service(Some("/test/cafe\u{301}"))
        );
    }

    #[tokio::test]
    async fn credential_files_are_bounded_regular_and_read_only() {
        let directory = tempdir().unwrap();
        let auth_path = directory.path().join("auth.json");
        assert_eq!(
            read_auth_file(&auth_path).await.unwrap_err(),
            LOGIN_REQUIRED
        );
        assert!(
            read_auth_file(directory.path())
                .await
                .unwrap_err()
                .contains("regular file")
        );
        let file = std::fs::File::create(&auth_path).unwrap();
        file.set_len(MAX_AUTH_BYTES + 1).unwrap();
        assert!(
            read_auth_file(&auth_path)
                .await
                .unwrap_err()
                .contains("1 MiB")
        );
        let bytes = serde_json::to_vec(&credentials()).unwrap();
        std::fs::write(&auth_path, &bytes).unwrap();
        assert_eq!(read_auth_file(&auth_path).await.unwrap(), bytes);
        assert_eq!(std::fs::read(&auth_path).unwrap(), bytes);
    }

    async fn query_fixture(response: ResponseTemplate) -> crate::UsageReport {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/oauth/usage"))
            .and(header("authorization", "Bearer private-access-token"))
            .and(header("anthropic-beta", "oauth-2025-04-20"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let directory = tempdir().unwrap();
        let auth_path = directory.path().join("credentials.json");
        let before = serde_json::to_vec(&credentials()).unwrap();
        std::fs::write(&auth_path, &before).unwrap();
        let report = crate::read_usage(&UsageOptions {
            providers: vec![UsageProvider::Claude],
            claude_auth_path: Some(auth_path.clone()),
            claude_endpoint: Some(format!("{}/api/oauth/usage", server.uri())),
            ..UsageOptions::default()
        })
        .await;
        assert_eq!(std::fs::read(auth_path).unwrap(), before);
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        let output = serde_json::to_string(&report).unwrap();
        assert!(!output.contains("private-"));
        assert!(!output.contains("Bearer"));
        report
    }

    #[tokio::test]
    async fn authenticated_get_returns_only_the_usage_projection() {
        let report = query_fixture(ResponseTemplate::new(200).set_body_json(payload())).await;
        assert_eq!(report.schema_version, 1);
        assert_eq!(report.providers[0].status, UsageStatus::Ready);
        assert_eq!(report.providers[0].windows.as_ref().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn authenticated_get_accepts_structured_limits_with_breakdown_metadata() {
        let report =
            query_fixture(ResponseTemplate::new(200).set_body_json(structured_payload())).await;
        assert_eq!(report.schema_version, 1);
        let entry = &report.providers[0];
        assert_eq!(entry.status, UsageStatus::Ready);
        assert_eq!(entry.remaining_percent, Some(91));
        let windows = entry.windows.as_ref().unwrap();
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[2].label.as_deref(), Some("Fable"));
    }

    #[tokio::test]
    async fn failures_never_echo_server_bodies_or_refresh_credentials() {
        for status in [401, 403, 429, 500] {
            let report = query_fixture(
                ResponseTemplate::new(status).set_body_string("private-server-diagnostic"),
            )
            .await;
            let entry = &report.providers[0];
            assert_eq!(entry.status, UsageStatus::Unavailable);
            assert!(entry.windows.is_none());
            let detail = entry.detail.as_deref().unwrap();
            if status == 401 || status == 403 {
                assert!(detail.contains("claude auth login"));
            }
            if status == 429 {
                assert!(detail.contains("rate limited"));
            }
        }
    }

    #[tokio::test]
    async fn redirect_is_not_followed_even_on_the_same_server() {
        let report = query_fixture(
            ResponseTemplate::new(302).insert_header("location", "/private-redirect"),
        )
        .await;
        assert_eq!(report.providers[0].status, UsageStatus::Unavailable);
    }

    #[tokio::test]
    async fn malformed_and_oversized_responses_are_unavailable() {
        for response in [
            ResponseTemplate::new(200).set_body_string("private-invalid-json"),
            ResponseTemplate::new(200).set_body_bytes(vec![b' '; MAX_RESPONSE_BYTES + 1]),
        ] {
            let report = query_fixture(response).await;
            assert_eq!(report.providers[0].status, UsageStatus::Unavailable);
        }
    }

    #[tokio::test]
    async fn expired_credentials_never_make_a_network_request() {
        let server = MockServer::start().await;
        let directory = tempdir().unwrap();
        let auth_path = directory.path().join("credentials.json");
        let mut auth = credentials();
        auth["claudeAiOauth"]["expiresAt"] = json!(1);
        let before = serde_json::to_vec(&auth).unwrap();
        std::fs::write(&auth_path, &before).unwrap();
        let report = crate::read_usage(&UsageOptions {
            providers: vec![UsageProvider::Claude],
            claude_auth_path: Some(auth_path.clone()),
            claude_endpoint: Some(format!("{}/api/oauth/usage", server.uri())),
            ..UsageOptions::default()
        })
        .await;
        assert_eq!(report.providers[0].status, UsageStatus::Unavailable);
        assert!(server.received_requests().await.unwrap().is_empty());
        assert_eq!(std::fs::read(auth_path).unwrap(), before);
    }
}
