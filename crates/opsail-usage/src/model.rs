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
    pub timeout: Duration,
    pub client: ClientInfo,
    pub(crate) grok_endpoint: Option<String>,
}

impl Default for UsageOptions {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            codex_path: None,
            grok_auth_path: None,
            timeout: DEFAULT_TIMEOUT,
            client: ClientInfo::default(),
            grok_endpoint: None,
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
}

impl UsageProvider {
    pub const ALL: [Self; 2] = [Self::Codex, Self::Grok];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Grok => "grok",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Grok => "Grok",
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

/// Credential-free remaining-usage snapshot for one ready Codex window.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageSnapshot {
    pub remaining_percent: u8,
    pub used_percent: f64,
    pub resets_at: Option<u64>,
    pub window_duration_mins: Option<f64>,
    pub plan_type: Option<String>,
    pub reset_credit_available_count: Option<u64>,
    pub reset_credit_expires_at: Option<u64>,
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
    let window = bucket
        .get("primary")
        .ok_or_else(UsageError::no_primary_window)?;
    let used_percent = finite_number(window.get("usedPercent"))
        .ok_or_else(UsageError::no_primary_window)?
        .clamp(0.0, 100.0);
    let remaining_percent = (100.0 - used_percent).round().clamp(0.0, 100.0) as u8;
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
        remaining_percent,
        used_percent,
        resets_at: json_u64(window.get("resetsAt")),
        window_duration_mins: finite_number(window.get("windowDurationMins"))
            .filter(|value| *value > 0.0),
        plan_type: bucket
            .get("planType")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        reset_credit_available_count,
        reset_credit_expires_at,
    })
}

fn rate_limit_bucket(value: &Value) -> Option<&Value> {
    if let Some(buckets) = value.get("rateLimitsByLimitId") {
        if let Some(codex) = buckets.get("codex").filter(|entry| entry.is_object()) {
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
    fn clamps_percentages_and_omits_empty_reset_credits() {
        let overflow = snapshot_from_rate_limits(&json!({
            "rateLimits": { "primary": { "usedPercent": 140 } }
        }))
        .unwrap();
        assert_eq!(overflow.remaining_percent, 0);
        assert_eq!(overflow.used_percent, 100.0);

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
}
