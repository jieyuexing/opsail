use thiserror::Error;

/// Stable diagnostic categories returned by a Codex CLI usage query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageErrorCode {
    CodexNotFound,
    SpawnFailed,
    TimedOut,
    Protocol,
    RequestFailed,
    NoPrimaryWindow,
}

impl UsageErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CodexNotFound => "codex-not-found",
            Self::SpawnFailed => "spawn-failed",
            Self::TimedOut => "timed-out",
            Self::Protocol => "protocol",
            Self::RequestFailed => "request-failed",
            Self::NoPrimaryWindow => "no-primary-window",
        }
    }
}

/// A bounded error that never retains RPC payloads, auth files, or credentials.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct UsageError {
    code: UsageErrorCode,
    message: String,
}

impl UsageError {
    pub(crate) fn new(code: UsageErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn codex_not_found() -> Self {
        Self::new(
            UsageErrorCode::CodexNotFound,
            "Codex CLI was not found; install it and run `codex login`",
        )
    }

    pub(crate) fn spawn_failed() -> Self {
        Self::new(
            UsageErrorCode::SpawnFailed,
            "failed to start the Codex app-server",
        )
    }

    pub(crate) fn timed_out() -> Self {
        Self::new(
            UsageErrorCode::TimedOut,
            "the Codex rate-limit query timed out",
        )
    }

    pub(crate) fn protocol() -> Self {
        Self::new(
            UsageErrorCode::Protocol,
            "the Codex app-server emitted an invalid response",
        )
    }

    pub(crate) fn request_failed() -> Self {
        Self::new(
            UsageErrorCode::RequestFailed,
            "the Codex account/rateLimits/read request failed",
        )
    }

    pub(crate) fn no_primary_window() -> Self {
        Self::new(
            UsageErrorCode::NoPrimaryWindow,
            "the Codex app-server did not return a primary rate-limit window",
        )
    }

    pub fn code(&self) -> UsageErrorCode {
        self.code
    }
}
