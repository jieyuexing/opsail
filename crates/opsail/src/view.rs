use std::io::{self, BufWriter, Write as _};
use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use miette::{IntoDiagnostic, Result, WrapErr, miette};
use opsail_read::{XlsxFullExportOptions, stream_xlsx_jsonl};

use crate::parse_positive_usize;

const DEFAULT_MAX_CELLS: usize = 2_000_000;
const DEFAULT_MAX_EXPANDED_BYTES: usize = 512 * 1024 * 1024;
const DEFAULT_MAX_TEXT_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub(crate) struct ViewArgs {
    #[command(subcommand)]
    command: ViewCommand,
}

#[derive(Debug, Subcommand)]
enum ViewCommand {
    /// Stream every non-empty physical XLSX cell as JSONL.
    Extract(ExtractArgs),
}

#[derive(Debug, Args)]
struct ExtractArgs {
    /// Local .xlsx workbook to export.
    #[arg(value_name = "FILE.xlsx")]
    file: PathBuf,

    /// Output representation. JSONL is the only stable full-export protocol.
    #[arg(long, value_enum, default_value_t = ViewFormat::Jsonl)]
    format: ViewFormat,

    /// Fail before writing a completion record after this many cells.
    #[arg(long, value_parser = parse_positive_usize, default_value_t = DEFAULT_MAX_CELLS)]
    max_cells: usize,

    /// Maximum cumulative uncompressed OOXML bytes read.
    #[arg(long, value_parser = parse_positive_usize, default_value_t = DEFAULT_MAX_EXPANDED_BYTES)]
    max_expanded_bytes: usize,

    /// Maximum total UTF-8 bytes of emitted cell text and formulas.
    #[arg(long, value_parser = parse_positive_usize, default_value_t = DEFAULT_MAX_TEXT_BYTES)]
    max_text_bytes: usize,

    /// Export internal workbook image bytes into this controlled local directory.
    #[arg(long, value_name = "DIRECTORY")]
    assets_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ViewFormat {
    Jsonl,
}

pub(crate) async fn run(args: ViewArgs) -> Result<()> {
    match args.command {
        ViewCommand::Extract(args) => run_extract(args).await,
    }
}

async fn run_extract(args: ExtractArgs) -> Result<()> {
    if !args
        .file
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("xlsx"))
    {
        return Err(miette!(
            "view extract currently accepts only a local .xlsx file"
        ));
    }
    let options = XlsxFullExportOptions {
        max_cells: args.max_cells,
        max_expanded_bytes: args.max_expanded_bytes,
        max_text_bytes: args.max_text_bytes,
        assets_dir: args.assets_dir,
    };
    // This operation is CPU and disk bound.  stdout is buffered but records are
    // serialized one at a time; no workbook-sized representation is retained.
    tokio::task::spawn_blocking(move || {
        let stdout = io::stdout();
        let mut output = BufWriter::new(stdout.lock());
        stream_xlsx_jsonl(&args.file, &mut output, options)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to fully export `{}`", args.file.display()))?;
        output
            .flush()
            .into_diagnostic()
            .wrap_err("failed to flush JSONL output")
    })
    .await
    .map_err(|_| miette!("full XLSX export task failed"))?
}
