//! Remaining-usage queries for signed-in Claude, Codex and Grok CLIs.
//!
//! Codex is read through a short-lived official `codex app-server --stdio`
//! process. Grok is read through the CLI sign-in file and the official
//! CLI proxy billing endpoint, with a grok.com fallback. Neither path attaches to ChatGPT.app, injects
//! renderer UI, or retains tokens, RPC bodies, or auth files in snapshots.

mod claude;
mod error;
mod grok;
mod model;
mod query;
mod resolve;
mod rpc;

pub use error::{UsageError, UsageErrorCode};
pub use model::{
    ClientInfo, DEFAULT_TIMEOUT, SCHEMA_VERSION, UsageEntry, UsageOptions, UsageProvider,
    UsageReport, UsageSnapshot, UsageStatus, UsageWindow,
};
pub use query::read_usage;
