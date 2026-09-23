use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Sensitive input. Debug is redacted; serialization is only for encrypted vaults
/// and private machine stdin. Never serialize this type into diagnostics.
#[derive(Clone, Default, Deserialize, Serialize, Zeroize, ZeroizeOnDrop)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(value)
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Adapter {
    Http,
    OpenaiCompatible,
    VercelAiGateway,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Auth {
    None,
    Bearer { key: Secret },
    Header { name: String, key: Secret },
}

impl Auth {
    pub(crate) fn key(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Bearer { key } | Self::Header { key, .. } => Some(key.expose()),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Connection {
    pub name: String,
    pub adapter: Adapter,
    pub base_url: String,
    pub auth: Auth,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default)]
    pub allow_http: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSummary {
    pub name: String,
    pub adapter: Adapter,
    pub base_url: String,
    pub auth_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<String>,
    pub has_key: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    pub allow_http: bool,
}

impl From<&Connection> for ConnectionSummary {
    fn from(c: &Connection) -> Self {
        let (auth_type, auth_header) = match &c.auth {
            Auth::None => ("none", None),
            Auth::Bearer { .. } => ("bearer", None),
            Auth::Header { name, .. } => ("header", Some(name.clone())),
        };
        Self {
            name: c.name.clone(),
            adapter: c.adapter,
            base_url: c.base_url.clone(),
            auth_type: auth_type.into(),
            auth_header,
            has_key: c.auth.key().is_some(),
            default_model: c.default_model.clone(),
            allow_http: c.allow_http,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Body {
    Json { value: Value },
    Text { value: String },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Question {
    Boolean {
        instructions: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<BTreeMap<String, Option<Value>>>,
    },
    Choice {
        instructions: Value,
        criteria: BTreeMap<String, Option<Value>>,
    },
    Score {
        instructions: Value,
        criteria: Vec<Option<Value>>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(
    tag = "operation",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum GatewayRequest {
    Init,
    List,
    Set {
        connection: Connection,
    },
    Remove {
        name: String,
    },
    SetDefaultModel {
        name: String,
        #[serde(default)]
        model: Option<String>,
    },
    Rekey,
    Request {
        connection: String,
        method: String,
        path: String,
        #[serde(default)]
        query: BTreeMap<String, String>,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(default)]
        body: Option<Body>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    Models {
        connection: String,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    Chat {
        connection: String,
        #[serde(default)]
        model: Option<String>,
        messages: Vec<Value>,
        #[serde(default)]
        parameters: BTreeMap<String, Value>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    Evaluate {
        connection: String,
        #[serde(default)]
        model: Option<String>,
        state: Value,
        questions: BTreeMap<String, Question>,
        #[serde(default)]
        provider_options: Option<Value>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
}

impl GatewayRequest {
    pub fn operation(&self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::List => "list",
            Self::Set { .. } => "set",
            Self::Remove { .. } => "remove",
            Self::SetDefaultModel { .. } => "set-default-model",
            Self::Rekey => "rekey",
            Self::Request { .. } => "request",
            Self::Models { .. } => "models",
            Self::Chat { .. } => "chat",
            Self::Evaluate { .. } => "evaluate",
        }
    }
    pub(crate) fn connection_name(&self) -> Option<&str> {
        match self {
            Self::Request { connection, .. }
            | Self::Models { connection, .. }
            | Self::Chat { connection, .. }
            | Self::Evaluate { connection, .. } => Some(connection),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayResult {
    pub schema_version: u8,
    pub operation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    pub elapsed_ms: u64,
    /// Complete JSON value or UTF-8 string. No credential material is returned.
    pub data: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayError {
    pub code: String,
    pub stage: String,
    pub message: String,
    /// Retry advice never causes an automatic retry; dispatched writes may have completed.
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}

impl GatewayError {
    pub fn new(code: &str, stage: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            stage: stage.into(),
            message: message.into(),
            retryable: false,
            http_status: None,
            provider_code: None,
            elapsed_ms: None,
        }
    }
    pub(crate) fn input(message: &str) -> Self {
        Self::new("invalid-request", "input", message)
    }
    pub(crate) fn vault(code: &str, message: &str) -> Self {
        Self::new(code, "vault", message)
    }
}

impl fmt::Display for GatewayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for GatewayError {}
