use std::{
    collections::BTreeMap,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use clap::{Args, Subcommand, ValueEnum};
use opsail_gateway::{
    Adapter, Auth, Connection, GatewayError, GatewayRequest, Secret, Vault, default_data_dir,
    execute,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use zeroize::Zeroizing;

const MAX_INPUT_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Args)]
pub(crate) struct GatewayArgs {
    /// Read one versioned JSON request (including ephemeral credentials) from stdin.
    #[arg(long)]
    machine: bool,
    /// Encrypted vault directory. Defaults to the user's Opsail configuration directory.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<GatewayCommand>,
}

#[derive(Debug, Subcommand)]
enum GatewayCommand {
    /// Create a vault; enter and confirm the passphrase using masked terminal input.
    Init,
    /// Manage encrypted connections.
    Connection {
        #[command(subcommand)]
        command: ConnectionCommand,
    },
    /// Change the vault passphrase using masked terminal input.
    Rekey,
    /// Send a bounded HTTP request described by JSON on stdin (or --input).
    Request(InputArgs),
    /// List models supported by an OpenAI-compatible endpoint.
    Models {
        connection: String,
        #[arg(long)]
        timeout_ms: Option<u64>,
    },
    /// Send a Chat Completions request described by JSON on stdin (or --input).
    Chat(InputArgs),
    /// Evaluate boolean, choice or score questions described by JSON on stdin (or --input).
    Evaluate(InputArgs),
}

#[derive(Debug, Args)]
struct InputArgs {
    connection: String,
    /// Request object without operation or connection. '-' reads stdin.
    #[arg(long, default_value = "-")]
    input: PathBuf,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum AdapterArg {
    Http,
    OpenaiCompatible,
    VercelAiGateway,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
enum AuthArg {
    None,
    Bearer,
    Header,
}

#[derive(Debug, Subcommand)]
enum ConnectionCommand {
    List,
    Remove {
        name: String,
    },
    /// Save a complete connection. Key and vault passphrase are entered with a masked prompt.
    Set {
        name: String,
        #[arg(long)]
        base_url: String,
        #[arg(long, value_enum)]
        adapter: AdapterArg,
        #[arg(long, value_enum, default_value = "bearer")]
        auth: AuthArg,
        #[arg(long)]
        auth_header: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        allow_http: bool,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Envelope {
    protocol_version: u64,
    request: GatewayRequest,
    passphrase: Secret,
    #[serde(default)]
    new_passphrase: Option<Secret>,
    #[serde(default)]
    data_dir: Option<PathBuf>,
}

pub(crate) async fn run(args: GatewayArgs) -> ExitCode {
    let result = run_inner(args).await;
    let (output, code) = match result {
        Ok(result) => (
            json!({"protocolVersion":1,"ok":true,"engine":engine(),"result":result}),
            ExitCode::SUCCESS,
        ),
        Err(error) => (
            json!({"protocolVersion":1,"ok":false,"engine":engine(),"error":error}),
            ExitCode::FAILURE,
        ),
    };
    let mut stdout = io::stdout().lock();
    if serde_json::to_writer(&mut stdout, &output).is_err() || stdout.write_all(b"\n").is_err() {
        return ExitCode::FAILURE;
    }
    code
}

async fn run_inner(args: GatewayArgs) -> Result<opsail_gateway::GatewayResult, GatewayError> {
    if args.machine && args.command.is_some() {
        return Err(GatewayError::new(
            "invalid-request",
            "input",
            "--machine cannot be combined with a gateway subcommand",
        ));
    }
    let envelope = if args.machine {
        let bytes = read_input(PathBuf::from("-")).await?;
        let envelope: Envelope = serde_json::from_slice(&bytes).map_err(|_| invalid_json())?;
        if envelope.protocol_version != 1 {
            return Err(GatewayError::new(
                "unsupported-protocol",
                "input",
                "expected gateway protocolVersion 1",
            ));
        }
        if args.data_dir.is_some() && envelope.data_dir.is_some() {
            return Err(GatewayError::new(
                "invalid-request",
                "input",
                "dataDir must be supplied in one place",
            ));
        }
        envelope
    } else {
        human_request(args.command).await?
    };
    let directory = args
        .data_dir
        .or(envelope.data_dir)
        .map(Ok)
        .unwrap_or_else(default_data_dir)?;
    execute(
        Vault::new(directory),
        envelope.request,
        envelope.passphrase,
        envelope.new_passphrase,
    )
    .await
}

async fn human_request(command: Option<GatewayCommand>) -> Result<Envelope, GatewayError> {
    let mut new_passphrase = None;
    let request = match command.ok_or_else(|| {
        GatewayError::new(
            "invalid-request",
            "input",
            "select a gateway subcommand or --machine",
        )
    })? {
        GatewayCommand::Init => GatewayRequest::Init,
        GatewayCommand::Rekey => GatewayRequest::Rekey,
        GatewayCommand::Connection { command } => match command {
            ConnectionCommand::List => GatewayRequest::List,
            ConnectionCommand::Remove { name } => GatewayRequest::Remove { name },
            ConnectionCommand::Set {
                name,
                base_url,
                adapter,
                auth,
                auth_header,
                model,
                allow_http,
            } => {
                let auth = match auth {
                    AuthArg::None => Auth::None,
                    AuthArg::Bearer => Auth::Bearer {
                        key: prompt("API key: ").await?,
                    },
                    AuthArg::Header => Auth::Header {
                        name: auth_header.ok_or_else(|| {
                            GatewayError::new(
                                "invalid-request",
                                "input",
                                "--auth-header is required for header authentication",
                            )
                        })?,
                        key: prompt("API key: ").await?,
                    },
                };
                let adapter = match adapter {
                    AdapterArg::Http => Adapter::Http,
                    AdapterArg::OpenaiCompatible => Adapter::OpenaiCompatible,
                    AdapterArg::VercelAiGateway => Adapter::VercelAiGateway,
                };
                GatewayRequest::Set {
                    connection: Connection {
                        name,
                        base_url,
                        adapter,
                        auth,
                        default_model: model,
                        allow_http,
                    },
                }
            }
        },
        GatewayCommand::Models {
            connection,
            timeout_ms,
        } => GatewayRequest::Models {
            connection,
            timeout_ms,
        },
        GatewayCommand::Request(input) => payload_request("request", input).await?,
        GatewayCommand::Chat(input) => payload_request("chat", input).await?,
        GatewayCommand::Evaluate(input) => payload_request("evaluate", input).await?,
    };
    let passphrase = prompt("Vault passphrase: ").await?;
    if matches!(request, GatewayRequest::Init) {
        confirm(&passphrase).await?;
    } else if matches!(request, GatewayRequest::Rekey) {
        let new = prompt("New vault passphrase: ").await?;
        confirm(&new).await?;
        new_passphrase = Some(new);
    }
    Ok(Envelope {
        protocol_version: 1,
        request,
        passphrase,
        new_passphrase,
        data_dir: None,
    })
}

async fn payload_request(
    operation: &str,
    input: InputArgs,
) -> Result<GatewayRequest, GatewayError> {
    let bytes = read_input(input.input).await?;
    let mut object: BTreeMap<String, Value> =
        serde_json::from_slice(&bytes).map_err(|_| invalid_json())?;
    if object.contains_key("operation") || object.contains_key("connection") {
        return Err(GatewayError::new(
            "invalid-request",
            "input",
            "input must omit operation and connection",
        ));
    }
    object.insert("operation".into(), operation.into());
    object.insert("connection".into(), input.connection.into());
    serde_json::from_value(json!(object)).map_err(|_| invalid_json())
}

async fn read_input(path: PathBuf) -> Result<Zeroizing<Vec<u8>>, GatewayError> {
    let mut bytes = Zeroizing::new(Vec::new());
    if path.as_os_str() == "-" {
        tokio::io::stdin()
            .take(MAX_INPUT_BYTES + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| invalid_json())?;
    } else {
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|_| invalid_json())?;
        file.take(MAX_INPUT_BYTES + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| invalid_json())?;
    }
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(GatewayError::new(
            "input-too-large",
            "input",
            "gateway input exceeds the 1 MiB limit",
        ));
    }
    Ok(bytes)
}

async fn prompt(label: &'static str) -> Result<Secret, GatewayError> {
    tokio::task::spawn_blocking(move || {
        let config = rpassword::ConfigBuilder::new()
            .password_feedback_mask('*')
            .build();
        rpassword::prompt_password_with_config(label, config).map(Secret::new)
    })
    .await
    .map_err(|_| terminal_error())?
    .map_err(|_| terminal_error())
}
async fn confirm(passphrase: &Secret) -> Result<(), GatewayError> {
    if prompt("Confirm vault passphrase: ").await?.expose() != passphrase.expose() {
        Err(GatewayError::new(
            "passphrase-mismatch",
            "input",
            "passphrase confirmation does not match",
        ))
    } else {
        Ok(())
    }
}
fn engine() -> Value {
    json!({"name":"opsail","version":env!("CARGO_PKG_VERSION")})
}
fn invalid_json() -> GatewayError {
    GatewayError::new(
        "invalid-request",
        "input",
        "input is not valid gateway JSON",
    )
}
fn terminal_error() -> GatewayError {
    GatewayError::new(
        "terminal-required",
        "input",
        "masked terminal input is unavailable; use --machine with private stdin",
    )
}
