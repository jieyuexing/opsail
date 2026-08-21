use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, ValueEnum};
use miette::{IntoDiagnostic, Result, WrapErr};
use opsail_usage::{
    ClientInfo, UsageEntry, UsageOptions, UsageProvider, UsageReport, UsageStatus, read_usage,
};

use crate::{parse_positive_u64, with_trailing_newline, write_stdout};

#[derive(Debug, Args)]
#[command(
    after_help = "Supported providers: codex, grok. When PROVIDER is omitted, query every supported provider. Codex uses a short-lived official `codex app-server`. Grok uses the CLI sign-in file and grok.com billing. This does not attach to ChatGPT.app, change the Codex sidebar, or print credentials. Codex resolution is --codex-path, then OPSAIL_CODEX_PATH, then PATH. Grok auth is --grok-auth, then OPSAIL_GROK_AUTH, then ~/.grok/auth.json."
)]
pub(crate) struct UsageArgs {
    /// Provider to query. When omitted, query every supported provider.
    #[arg(value_enum, value_name = "PROVIDER")]
    provider: Option<UsageProviderArg>,

    /// Output representation.
    #[arg(long, value_enum, default_value_t = UsageFormat::Json)]
    format: UsageFormat,

    /// Codex CLI executable.
    #[arg(long, value_name = "PATH")]
    codex_path: Option<PathBuf>,

    /// Grok CLI auth.json path.
    #[arg(long, value_name = "PATH")]
    grok_auth: Option<PathBuf>,

    /// Overall provider query timeout in seconds.
    #[arg(long, value_name = "SECONDS", value_parser = parse_positive_u64)]
    timeout: Option<u64>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum UsageProviderArg {
    Codex,
    Grok,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum UsageFormat {
    Json,
    Text,
}

impl From<UsageProviderArg> for UsageProvider {
    fn from(value: UsageProviderArg) -> Self {
        match value {
            UsageProviderArg::Codex => Self::Codex,
            UsageProviderArg::Grok => Self::Grok,
        }
    }
}

pub(crate) async fn run(args: UsageArgs) -> Result<()> {
    let mut options = UsageOptions::default();
    options.providers = args.provider.into_iter().map(UsageProvider::from).collect();
    options.codex_path = args.codex_path;
    options.grok_auth_path = args.grok_auth;
    options.timeout = args
        .timeout
        .map(Duration::from_secs)
        .unwrap_or(options.timeout);
    options.client = ClientInfo {
        name: "opsail".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    };

    let report = read_usage(&options).await;
    match args.format {
        UsageFormat::Json => write_json(&report),
        UsageFormat::Text => write_stdout(with_trailing_newline(format_text(&report)).as_bytes()),
    }
}

fn format_text(report: &UsageReport) -> String {
    report
        .providers
        .iter()
        .map(format_entry)
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_entry(entry: &UsageEntry) -> String {
    let name = entry.provider.display_name();
    match entry.status {
        UsageStatus::Ready => {
            let mut line = format!(
                "{name}\t{}% remaining",
                entry.remaining_percent.unwrap_or(0)
            );
            if let Some(plan) = &entry.plan_type {
                line.push('\t');
                line.push_str(plan);
            }
            if let Some(resets_at) = entry.resets_at {
                line.push_str("\tresets ");
                line.push_str(&resets_at.to_string());
            }
            line
        }
        UsageStatus::Unavailable => {
            format!(
                "{name}\t--\t{}",
                entry.detail.as_deref().unwrap_or("unavailable")
            )
        }
    }
}

fn write_json(value: &impl serde::Serialize) -> Result<()> {
    let output = with_trailing_newline(
        serde_json::to_string_pretty(value)
            .into_diagnostic()
            .wrap_err("failed to serialize usage result")?,
    );
    write_stdout(output.as_bytes())
}
