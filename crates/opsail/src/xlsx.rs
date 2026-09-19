//! Thin CLI transport for the native XLSX inspection/patch protocol.

use std::io::{self, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

const MAX_REQUEST_BYTES: usize = 1024 * 1024;

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub(crate) struct XlsxArgs {
    /// Read one schemaVersion=1 JSON request from stdin; emit one JSON result.
    #[arg(long)]
    machine: bool,
    #[command(subcommand)]
    command: Option<XlsxCommand>,
}

#[derive(Debug, Subcommand)]
enum XlsxCommand {
    /// Inspect exact cells and stored formatting. This does not render the workbook.
    Inspect {
        source: PathBuf,
        #[arg(long = "range", required = true)]
        ranges: Vec<String>,
        #[arg(long, default_value_t = 200)]
        max_cells: usize,
    },
    /// Apply a fixed-SHA plan to a new candidate. Never overwrites the source or output.
    Patch {
        source: PathBuf,
        /// JSON object containing expectedSha256 and operations.
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Compare package parts and stored cell semantics, independently of style indexes.
    Diff {
        before: PathBuf,
        after: PathBuf,
        #[arg(long, default_value_t = 200)]
        max_cells: usize,
    },
}

async fn request(args: XlsxArgs) -> Result<Value, String> {
    if args.machine && args.command.is_some() {
        return Err("--machine cannot be combined with a subcommand".into());
    }
    if args.machine {
        let mut bytes = Vec::new();
        tokio::io::stdin()
            .take((MAX_REQUEST_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| e.to_string())?;
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err("XLSX request exceeds 1 MiB".into());
        }
        return serde_json::from_slice(&bytes).map_err(|e| e.to_string());
    }
    match args
        .command
        .ok_or("choose inspect, patch, diff, or --machine")?
    {
        XlsxCommand::Inspect {
            source,
            ranges,
            max_cells,
        } => Ok(json!({
            "schemaVersion": 1, "operation": "inspect", "source": source,
            "ranges": ranges, "maxCells": max_cells,
        })),
        XlsxCommand::Diff {
            before,
            after,
            max_cells,
        } => Ok(json!({
            "schemaVersion": 1, "operation": "diff", "before": before,
            "after": after, "maxCells": max_cells,
        })),
        XlsxCommand::Patch {
            source,
            plan,
            output,
        } => {
            let mut bytes = Vec::new();
            tokio::fs::File::open(plan)
                .await
                .map_err(|e| e.to_string())?
                .take((MAX_REQUEST_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .await
                .map_err(|e| e.to_string())?;
            if bytes.len() > MAX_REQUEST_BYTES {
                return Err("XLSX patch plan exceeds 1 MiB".into());
            }
            let mut plan: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            let object = plan
                .as_object_mut()
                .ok_or("patch plan must be a JSON object")?;
            if object.keys().any(|key| {
                ![
                    "expectedSha256",
                    "operations",
                    "maxBytes",
                    "maxExpandedBytes",
                ]
                .contains(&key.as_str())
            }) {
                return Err("patch plan accepts only expectedSha256, operations, maxBytes and maxExpandedBytes".into());
            }
            object.insert("schemaVersion".into(), json!(1));
            object.insert("operation".into(), json!("patch"));
            object.insert("source".into(), json!(source));
            object.insert("output".into(), json!(output));
            Ok(plan)
        }
    }
}

pub(crate) async fn run(args: XlsxArgs) -> ExitCode {
    let result = match request(args).await {
        Ok(request) => {
            match tokio::task::spawn_blocking(move || opsail_xlsx::execute(request)).await {
                Ok(result) => result.map_err(|e| e.to_string()),
                Err(error) => Err(error.to_string()),
            }
        }
        Err(error) => Err(error),
    };
    let (response, exit) = match result {
        Ok(response) => (response, ExitCode::SUCCESS),
        Err(message) => (
            json!({ "schemaVersion": 1, "error": { "message": message } }),
            ExitCode::from(2),
        ),
    };
    match writeln!(io::stdout().lock(), "{response}") {
        Ok(()) => exit,
        Err(_) => ExitCode::FAILURE,
    }
}
