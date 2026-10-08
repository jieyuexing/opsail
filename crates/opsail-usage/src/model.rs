use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::error::UsageError;

pub const SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

/// Identity presented to `codex app-server` during initialize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

impl Default for ClientInfo {
    fn default() -> Self {
        Self {
            name: "opsail".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

/// Options for one remaining-usage query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageOptions {
    /// Empty means every supported provider.
    pub providers: Vec<UsageProvider>,
    /// Explicit Codex CLI executable. When omitted, resolve `OPSAIL_CODEX_PATH` then `PATH`.
    pub codex_path: Option<PathBuf>,
    /// Explicit Grok CLI auth file. When omitted, resolve `OPSAIL_GROK_AUTH` then `~/.grok/auth.json`.
    pub grok_auth_path: Option<PathBuf>,
    /// Explicit Claude Code credentials file; bypasses Keychain when set.
    /// Otherwise use OPSAIL_CLAUDE_AUTH, then the Claude Code credential stores.
    pub claude_auth_path: Option<PathBuf>,
    pub timeout: Duration,
    pub client: ClientInfo,
    #[cfg(test)]
    pub(crate) grok_endpoint: Option<String>,
    #[cfg(test)]
    pub(crate) grok_fallback_endpoint: Option<String>,
    #[cfg(test)]
    pub(crate) claude_endpoint: Option<String>,
}

impl Default for UsageOptions {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            codex_path: None,
            grok_auth_path: None,
            claude_auth_path: None,
            timeout: DEFAULT_TIMEOUT,
            client: ClientInfo::default(),
            #[cfg(test)]
            grok_endpoint: None,
            #[cfg(test)]
            grok_fallback_endpoint: None,
            #[cfg(test)]
            claude_endpoint: None,
        }
    }
}

impl UsageOptions {
    pub fn selected_providers(&self) -> Vec<UsageProvider> {
        if self.providers.is_empty() {
            return UsageProvider::ALL.to_vec();
        }
        let mut selected = Vec::new();
        for provider in &self.providers {
            if !selected.contains(provider) {
                selected.push(*provider);
            }
        }
        selected
    }
}

/// Account runtime whose remaining windows were queried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsageProvider {
    Codex,
    Grok,
    Claude,
}

impl UsageProvider {
    pub const ALL: [Self; 3] = [Self::Codex, Self::Grok, Self::Claude];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Grok => "grok",
            Self::Claude => "claude",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Grok => "Grok",
            Self::Claude => "Claude",
        }
    }
}

/// Whether one provider returned a usable remaining-usage window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsageStatus {
    Ready,
    Unavailable,
}

/// Credential-free remaining-usage snapshot with optional named windows.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageSnapshot {
    pub remaining_percent: u8,
    pub used_percent: f64,
    pub resets_at: Option<u64>,
    pub window_duration_mins: Option<f64>,
    pub plan_type: Option<String>,
    pub reset_credit_available_count: Option<u64>,
    pub reset_credit_expires_at: Option<u64>,
    pub windows: Option<Vec<UsageWindow>>,
}

/// One named subscription window. Reset times use Unix seconds, like the legacy fields.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub id: String,
    /// Human-readable model name, when supplied by a structured scoped limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub remaining_percent: u8,
    pub used_percent: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_duration_mins: Option<f64>,
}

/// One provider row in a usage report.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageEntry {
    pub provider: UsageProvider,
    pub status: UsageStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_percent: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_duration_mins: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_credit_available_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_credit_expires_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Optional multi-window extension to schema version 1. Legacy providers omit it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub windows: Option<Vec<UsageWindow>>,
}

impl UsageEntry {
    pub(crate) fn from_codex(snapshot: UsageSnapshot) -> Self {
        Self {
            provider: UsageProvider::Codex,
            status: UsageStatus::Ready,
            remaining_percent: Some(snapshot.remaining_percent),
            used_percent: Some(snapshot.used_percent),
            resets_at: snapshot.resets_at,
            window_duration_mins: snapshot.window_duration_mins,
            plan_type: snapshot.plan_type,
            reset_credit_available_count: snapshot.reset_credit_available_count,
            reset_credit_expires_at: snapshot.reset_credit_expires_at,
            detail: None,
            windows: snapshot.windows,
        }
    }

    pub(crate) fn from_grok(snapshot: UsageSnapshot) -> Self {
        Self {
            provider: UsageProvider::Grok,
            status: UsageStatus::Ready,
            remaining_percent: Some(snapshot.remaining_percent),
            used_percent: Some(snapshot.used_percent),
            resets_at: snapshot.resets_at,
            window_duration_mins: snapshot.window_duration_mins,
            plan_type: snapshot.plan_type,
            reset_credit_available_count: snapshot.reset_credit_available_count,
            reset_credit_expires_at: snapshot.reset_credit_expires_at,
            detail: None,
            windows: None,
        }
    }

    pub(crate) fn unavailable(provider: UsageProvider, detail: impl Into<String>) -> Self {
        Self {
            provider,
            status: UsageStatus::Unavailable,
            remaining_percent: None,
            used_percent: None,
            resets_at: None,
            window_duration_mins: None,
            plan_type: None,
            reset_credit_available_count: None,
            reset_credit_expires_at: None,
            detail: Some(detail.into()),
            windows: None,
        }
    }
}

/// Versioned remaining-usage report for one or more providers.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageReport {
    pub schema_version: u32,
    pub providers: Vec<UsageEntry>,
}

pub(crate) fn snapshot_from_rate_limits(value: &Value) -> Result<UsageSnapshot, UsageError> {
    let bucket = rate_limit_bucket(value).ok_or_else(UsageError::no_primary_window)?;
    let primary = bucket
        .get("primary")
        .filter(|window| !window.is_null())
        .ok_or_else(UsageError::no_primary_window)?;
    let primary = codex_window("primary", primary)?;
    let mut windows = vec![primary.clone()];
    match bucket.get("secondary") {
        None | Some(Value::Null) => {}
        Some(secondary) => windows.push(codex_window("secondary", secondary)?),
    }
    let reset_credits = value.get("rateLimitResetCredits");
    let reset_credit_available_count = reset_credits
        .and_then(|credits| finite_number(credits.get("availableCount")))
        .map(|count| count.max(0.0).floor() as u64)
        .filter(|count| *count > 0);
    let reset_credit_expires_at = reset_credit_available_count.and_then(|_| {
        reset_credits
            .and_then(|credits| credits.get("credits"))
            .and_then(Value::as_array)
            .and_then(|credits| {
                credits
                    .iter()
                    .filter(|credit| {
                        credit.get("status").and_then(Value::as_str) == Some("available")
                    })
                    .filter_map(|credit| json_u64(credit.get("expiresAt")))
                    .min()
            })
    });

    Ok(UsageSnapshot {
        remaining_percent: primary.remaining_percent,
        used_percent: primary.used_percent,
        resets_at: primary.resets_at,
        window_duration_mins: primary.window_duration_mins,
        plan_type: bucket
            .get("planType")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        reset_credit_available_count,
        reset_credit_expires_at,
        windows: Some(windows),
    })
}

fn codex_window(id: &str, value: &Value) -> Result<UsageWindow, UsageError> {
    let object = value.as_object().ok_or_else(UsageError::protocol)?;
    let used_percent = finite_number(object.get("usedPercent"))
        .filter(|percent| (0.0..=100.0).contains(percent))
        .ok_or_else(UsageError::protocol)?;
    let window_duration_mins = match object.get("windowDurationMins") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            finite_number(Some(value))
                .filter(|duration| *duration > 0.0)
                .ok_or_else(UsageError::protocol)?,
        ),
    };
    let resets_at = match object.get("resetsAt") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let seconds = value.as_u64().ok_or_else(UsageError::protocol)?;
            let timestamp = i64::try_from(seconds).map_err(|_| UsageError::protocol())?;
            time::OffsetDateTime::from_unix_timestamp(timestamp)
                .map_err(|_| UsageError::protocol())?;
            Some(seconds)
        }
    };
    Ok(UsageWindow {
        id: id.to_owned(),
        label: None,
        remaining_percent: (100.0 - used_percent).round() as u8,
        used_percent,
        resets_at,
        window_duration_mins,
    })
}

fn rate_limit_bucket(value: &Value) -> Option<&Value> {
    if let Some(buckets) = value.get("rateLimitsByLimitId") {
        if let Some(codex) = buckets.get("codex") {
            return Some(codex);
        }
        if let Some(values) = buckets.as_object()
            && let Some(matched) = values
                .values()
                .find(|entry| entry.get("limitId").and_then(Value::as_str) == Some("codex"))
        {
            return Some(matched);
        }
    }
    value.get("rateLimits").filter(|entry| entry.is_object())
}

fn finite_number(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
}

fn json_u64(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    value.as_u64().or_else(|| {
        finite_number(Some(value))
            .filter(|number| *number > 0.0)
            .map(|number| number as u64)
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{UsageProvider, snapshot_from_rate_limits};
    use crate::UsageErrorCode;

    #[test]
    fn malformed_secondary_is_not_a_ready_primary_only_snapshot() {
        let result = snapshot_from_rate_limits(&json!({
            "rateLimits": {
                "primary": { "usedPercent": 14, "windowDurationMins": 300 },
                "secondary": { "usedPercent": "invalid-secondary" }
            }
        }));
        assert!(
            result.is_err(),
            "a malformed secondary must reject the snapshot"
        );
    }

    #[test]
    fn primary_codex_bucket_wins_over_legacy_rate_limits() {
        let snapshot = snapshot_from_rate_limits(&json!({
            "rateLimits": { "primary": { "usedPercent": 90 } },
            "rateLimitsByLimitId": {
                "codex": {
                    "primary": {
                        "usedPercent": 37.6,
                        "windowDurationMins": 300,
                        "resetsAt": 1_786_000_000u64
                    },
                    "planType": "plus"
                }
            },
            "rateLimitResetCredits": {
                "availableCount": 2,
                "credits": [
                    { "status": "available", "expiresAt": 1_786_500_000u64 },
                    { "status": "redeemed", "expiresAt": 1_786_100_000u64 }
                ]
            }
        }))
        .unwrap();

        assert_eq!(snapshot.remaining_percent, 62);
        assert_eq!(snapshot.used_percent, 37.6);
        assert_eq!(snapshot.resets_at, Some(1_786_000_000));
        assert_eq!(snapshot.window_duration_mins, Some(300.0));
        assert_eq!(snapshot.plan_type.as_deref(), Some("plus"));
        assert_eq!(snapshot.reset_credit_available_count, Some(2));
        assert_eq!(snapshot.reset_credit_expires_at, Some(1_786_500_000));
    }

    #[test]
    fn limit_id_selects_the_codex_bucket() {
        let snapshot = snapshot_from_rate_limits(&json!({
            "rateLimitsByLimitId": {
                "other": { "limitId": "other", "primary": { "usedPercent": 10 } },
                "primary": { "limitId": "codex", "primary": { "usedPercent": 40 } }
            }
        }))
        .unwrap();
        assert_eq!(snapshot.remaining_percent, 60);
    }

    #[test]
    fn omits_empty_reset_credits() {
        let empty_credits = snapshot_from_rate_limits(&json!({
            "rateLimits": { "primary": { "usedPercent": 10 } },
            "rateLimitResetCredits": { "availableCount": 0, "credits": [] }
        }))
        .unwrap();
        assert_eq!(empty_credits.reset_credit_available_count, None);
        assert_eq!(empty_credits.reset_credit_expires_at, None);
    }

    #[test]
    fn missing_primary_window_is_a_bounded_error() {
        let error = snapshot_from_rate_limits(&json!({
            "rateLimits": { "secondary": { "usedPercent": 5 } }
        }))
        .unwrap_err();
        assert_eq!(error.code(), UsageErrorCode::NoPrimaryWindow);
        assert!(!error.to_string().contains("usedPercent"));
    }

    #[test]
    fn empty_provider_list_selects_every_supported_runtime() {
        assert_eq!(
            super::UsageOptions::default().selected_providers(),
            UsageProvider::ALL.to_vec()
        );
    }

    #[test]
    fn grok_json_does_not_gain_a_windows_field() {
        let snapshot = snapshot_from_rate_limits(&json!({
            "rateLimits": { "primary": { "usedPercent": 20 } }
        }))
        .unwrap();
        let encoded = serde_json::to_value(super::UsageEntry::from_grok(snapshot)).unwrap();
        assert_eq!(encoded.as_object().unwrap().len(), 4);
        assert_eq!(encoded["remainingPercent"], 80);
        assert_eq!(encoded["usedPercent"], 20.0);
        assert!(encoded.get("windows").is_none());
    }

    #[test]
    fn codex_projects_both_selected_windows_and_preserves_legacy_primary() {
        let snapshot = snapshot_from_rate_limits(&json!({
            "rateLimits": {
                "primary": { "usedPercent": 99 },
                "secondary": { "usedPercent": "ignored-legacy" }
            },
            "rateLimitsByLimitId": {
                "other": { "primary": { "usedPercent": 99 } },
                "selected": {
                    "limitId": "codex",
                    "primary": {
                        "usedPercent": 12.75, "windowDurationMins": 10080,
                        "resetsAt": 1786000000, "unknown": "private-response-marker"
                    },
                    "secondary": {
                        "usedPercent": 100, "windowDurationMins": 90.5,
                        "resetsAt": 1786000100
                    }
                }
            }
        }))
        .unwrap();
        let entry = serde_json::to_value(super::UsageEntry::from_codex(snapshot)).unwrap();
        assert_eq!(entry["status"], "ready");
        let windows = entry["windows"].as_array().unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0]["id"], "primary");
        assert_eq!(windows[0]["usedPercent"], 12.75);
        assert_eq!(windows[0]["remainingPercent"], 87);
        assert_eq!(windows[0]["windowDurationMins"], 10080.0);
        assert_eq!(windows[1]["id"], "secondary");
        assert_eq!(windows[1]["remainingPercent"], 0);
        assert_eq!(windows[1]["windowDurationMins"], 90.5);
        assert_eq!(windows[1]["resetsAt"], 1786000100u64);
        for field in [
            "usedPercent",
            "remainingPercent",
            "windowDurationMins",
            "resetsAt",
        ] {
            assert_eq!(entry[field], windows[0][field], "legacy {field}");
        }
        assert!(!entry.to_string().contains("private-response-marker"));
    }

    #[test]
    fn codex_accepts_absent_or_null_secondary_and_unknown_optional_fields() {
        for bucket in [
            json!({ "primary": { "usedPercent": 0 } }),
            json!({ "primary": { "usedPercent": 0, "windowDurationMins": null, "resetsAt": null }, "secondary": null }),
        ] {
            let snapshot = snapshot_from_rate_limits(&json!({ "rateLimits": bucket })).unwrap();
            let entry = serde_json::to_value(super::UsageEntry::from_codex(snapshot)).unwrap();
            assert_eq!(entry["windows"].as_array().unwrap().len(), 1);
            assert_eq!(entry["remainingPercent"], 100);
            for value in [&entry, &entry["windows"][0]] {
                assert!(value.get("windowDurationMins").is_none());
                assert!(value.get("resetsAt").is_none());
            }
        }
        let snapshot = snapshot_from_rate_limits(&json!({ "rateLimits": {
            "primary": { "usedPercent": 1 },
            "secondary": { "usedPercent": 2.5, "windowDurationMins": null, "resetsAt": null }
        }}))
        .unwrap();
        let windows = snapshot.windows.unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[1].used_percent, 2.5);
        assert_eq!(windows[1].window_duration_mins, None);
        assert_eq!(windows[1].resets_at, None);
    }

    #[test]
    fn codex_rejects_malformed_primary_and_secondary_windows() {
        let mut malformed = vec![json!({}), json!(false), json!([]), json!("private-window")];
        for used in [
            json!(null),
            json!(-0.1),
            json!(100.1),
            json!("12"),
            json!(true),
            json!([]),
        ] {
            malformed.push(json!({ "usedPercent": used }));
        }
        for duration in [json!(0), json!(-1), json!("300"), json!(true), json!({})] {
            malformed.push(json!({ "usedPercent": 10, "windowDurationMins": duration }));
        }
        for timestamp in [
            json!(-1),
            json!(1.5),
            json!("1786000000"),
            json!(true),
            json!([]),
            json!(u64::MAX),
            json!(253402300800u64),
        ] {
            malformed.push(json!({ "usedPercent": 10, "resetsAt": timestamp }));
        }
        for id in ["primary", "secondary"] {
            for invalid in &malformed {
                let mut bucket =
                    json!({ "primary": { "usedPercent": 14 }, "secondary": { "usedPercent": 20 } });
                bucket[id] = invalid.clone();
                let error =
                    snapshot_from_rate_limits(&json!({ "rateLimits": bucket })).unwrap_err();
                assert_eq!(error.code(), UsageErrorCode::Protocol, "{id}: {invalid}");
                assert!(!error.to_string().contains("private-window"));
            }
        }
    }

    #[test]
    fn malformed_named_codex_bucket_does_not_fall_back_to_legacy() {
        for invalid in [json!(null), json!(false), json!([]), json!("invalid")] {
            assert!(
                snapshot_from_rate_limits(&json!({
                    "rateLimits": { "primary": { "usedPercent": 0 } },
                    "rateLimitsByLimitId": { "codex": invalid }
                }))
                .is_err()
            );
        }
    }
}
