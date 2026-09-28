//! One-shot Grok subscription calls. No shell, session resume or tool execution API.
use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

use crate::client::{MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, redact};
use crate::{Connection, GatewayError, GatewayRequest, GatewayResult, Secret};

// Sources: Grok 1.0.41 user-guide/14-headless-mode.md (internal IDs, Agent,
// always-on MCP meta-tools), 22-permissions-and-safety.md, and the tool inventory
// observed in the 2026-09-28 headless probes. Include both local and server tool
// spellings. --tools alone does NOT remove MCP meta-tools; deny rules and the
// kernel sandbox are independent boundaries. Requalify on CLI toolset changes.
const DISALLOWED_TOOLS: &str = "read_file,list_dir,list_directory,grep,run_terminal_cmd,bash,search_replace,write,write_file,web_search,web_fetch,web_open,Agent,spawn_subagent,await_task,task_list,task_revoke,get_command_or_subagent_output,kill_command_or_subagent,todo_write,monitor,search_tool,use_tool,workflow,enter_plan_mode,exit_plan_mode,ask_user_question,send_feedback,image_gen,image_edit,image_to_video,reference_to_video,x_user_search,x_semantic_search,x_keyword_search,x_thread_fetch";
const DENY_RULES: &[&str] = &[
    "*",
    "Read",
    "Grep",
    "Bash",
    "Edit",
    "Write",
    "WebFetch",
    "WebSearch",
    "MCPTool",
];
const TEXT_SYSTEM: &str = "Answer the supplied conversation as a text-only model. Do not use tools, access files, browse, spawn agents or invoke MCP.";

struct Prepared {
    prompt: Option<String>,
    system: String,
    model: Option<String>,
    effort: Option<String>,
    timeout: Duration,
}

fn prepare(c: &Connection, request: GatewayRequest) -> Result<Prepared, GatewayError> {
    let mut p = Prepared {
        prompt: None,
        system: TEXT_SYSTEM.into(),
        model: None,
        effort: None,
        timeout: Duration::from_secs(30),
    };
    let timeout = match request {
        GatewayRequest::Models { timeout_ms, .. } => timeout_ms,
        GatewayRequest::Chat {
            model,
            messages,
            parameters,
            timeout_ms,
            ..
        } => {
            // Bound the entire input before constructing command arguments or files.
            if serde_json::to_vec(&json!({"messages":messages,"parameters":parameters}))
                .map_err(|_| GatewayError::input("invalid chat input"))?
                .len()
                > MAX_REQUEST_BYTES
            {
                return Err(GatewayError::input("request exceeds the 1 MiB limit"));
            }
            p.model = model.or_else(|| c.default_model.clone());
            if p.model
                .as_ref()
                .is_some_and(|m| m.is_empty() || m.len() > 256 || m.contains('\0'))
            {
                return Err(GatewayError::input(
                    "model must contain 1 to 256 bytes without NUL",
                ));
            }
            p.effort = reasoning_effort(parameters)?;
            let mut systems = Vec::new();
            let mut turns = Vec::new();
            for message in messages {
                let object = message
                    .as_object()
                    .ok_or_else(|| GatewayError::input("messages must be text objects"))?;
                if object
                    .keys()
                    .any(|k| !matches!(k.as_str(), "role" | "content"))
                {
                    return Err(GatewayError::input(
                        "grok-cli messages only accept role and text content",
                    ));
                }
                let content = text_content(object.get("content"))?;
                match object.get("role").and_then(Value::as_str) {
                    Some("system") => systems.push(content),
                    Some(role @ ("user" | "assistant")) => {
                        turns.push(format!("[{role}]\n{content}"))
                    }
                    _ => {
                        return Err(GatewayError::input(
                            "grok-cli only accepts system, user and assistant roles",
                        ));
                    }
                }
            }
            if turns.is_empty() {
                return Err(GatewayError::input(
                    "grok-cli requires at least one user or assistant message",
                ));
            }
            if !systems.is_empty() {
                p.system = systems.join("\n\n");
            }
            // System override is an argv string; keep it below portable OS argv limits.
            if p.system.len() > 32 * 1024 || p.system.contains('\0') {
                return Err(GatewayError::input(
                    "system text must be at most 32 KiB without NUL",
                ));
            }
            p.prompt = Some(turns.join("\n\n"));
            timeout_ms
        }
        _ => {
            return Err(GatewayError::new(
                "unsupported-capability",
                "input",
                "grok-cli supports only models and chat",
            ));
        }
    };
    let millis = timeout.unwrap_or(30_000);
    if millis == 0 || millis > 3_600_000 {
        return Err(GatewayError::input(
            "timeoutMs must be between 1 and 3600000",
        ));
    }
    p.timeout = Duration::from_millis(millis);
    Ok(p)
}

fn text_content(value: Option<&Value>) -> Result<String, GatewayError> {
    if let Some(Value::String(text)) = value {
        return Ok(text.clone());
    }
    if let Some(Value::Array(blocks)) = value {
        let text: Option<Vec<&str>> = blocks
            .iter()
            .map(|b| {
                let b = b.as_object()?;
                if b.len() != 2 || b.get("type")?.as_str()? != "text" {
                    return None;
                }
                b.get("text")?.as_str()
            })
            .collect();
        if let Some(parts) = text.filter(|parts| !parts.is_empty()) {
            return Ok(parts.join("\n"));
        }
    }
    Err(GatewayError::input(
        "grok-cli rejects non-text message content",
    ))
}

fn reasoning_effort(parameters: BTreeMap<String, Value>) -> Result<Option<String>, GatewayError> {
    if parameters.keys().any(|k| k != "reasoningEffort") {
        return Err(GatewayError::input(
            "grok-cli parameters only support reasoningEffort",
        ));
    }
    match parameters.get("reasoningEffort") {
        None => Ok(None),
        Some(Value::String(s))
            if matches!(
                s.as_str(),
                "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
            ) =>
        {
            Ok(Some(s.clone()))
        }
        _ => Err(GatewayError::input(
            "reasoningEffort must be none, minimal, low, medium, high, xhigh or max",
        )),
    }
}

pub(crate) async fn call(
    c: Connection,
    request: GatewayRequest,
    passphrase: &Secret,
) -> Result<GatewayResult, GatewayError> {
    let operation = request.operation().to_owned();
    let prepared = prepare(&c, request)?; // Reject unsupported operations BEFORE resolving/spawning.
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| sandbox_error("HOME is required for the Grok sandbox"))?;
    call_prepared(c, operation, prepared, passphrase, &env::temp_dir(), &home).await
}

async fn call_prepared(
    c: Connection,
    operation: String,
    p: Prepared,
    passphrase: &Secret,
    base: &Path,
    home: &Path,
) -> Result<GatewayResult, GatewayError> {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return Err(sandbox_error(
            "grok-cli sandbox is only supported on macOS and Linux",
        ));
    }
    let binary = discover_executable(
        c.grok_path.as_deref().map(Path::new),
        env::var_os("OPSAIL_GROK_PATH").as_deref(),
        env::var_os("PATH").as_deref(),
    )
    .ok_or_else(|| {
        GatewayError::new(
            "grok-not-found",
            "acquire",
            "Grok CLI not found; set --grok-path, OPSAIL_GROK_PATH or PATH",
        )
    })?;
    let sandbox = Sandbox::create(base, home)?;
    let started = Instant::now();
    let result = async {
        let mut command = Command::new(binary);
        command.current_dir(sandbox.directory.path())
            .args(["--no-auto-update", "--no-subagents", "--disable-web-search", "--cwd"])
            .arg(sandbox.directory.path()).args(["--sandbox", &sandbox.profile])
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
            // This adapter uses the existing ~/.grok subscription identity only.
            .env_remove("XAI_API_KEY").env_remove("GROK_API_KEY").env_remove("GROK_HOME")
            .env_remove("GROK_LOG_FILE");
        if let Some(prompt) = &p.prompt {
            let prompt_path = sandbox.directory.path().join("prompt.txt");
            fs::write(&prompt_path, prompt)
                .map_err(|_| sandbox_error("could not write the temporary prompt"))?;
            command.args([
                "--output-format",
                "streaming-json",
                "--verbatim",
                "--max-turns",
                "1",
                "--tools",
                "todo_write",
                "--disallowed-tools",
                DISALLOWED_TOOLS,
                "--permission-mode",
                "dontAsk",
            ]);
            for rule in DENY_RULES {
                command.args(["--deny", rule]);
            }
            command
                .args(["--system-prompt-override", &p.system, "--prompt-file"])
                .arg(prompt_path);
            if let Some(model) = &p.model {
                command.args(["--model", model]);
            }
            if let Some(effort) = &p.effort {
                command.args(["--reasoning-effort", effort]);
            }
        } else {
            command.arg("models");
        }
        let bytes = run(command, p.timeout).await?;
        let text = std::str::from_utf8(&bytes).map_err(|_| invalid_response())?;
        let mut data = if p.prompt.is_some() {
            parse_events(text, p.model.as_deref())?
        } else {
            parse_models(text)?
        };
        data["sandbox"] = json!({"profile":sandbox.profile,"skippedDenyPaths":sandbox.skipped});
        redact(&mut data, &[passphrase.expose()]);
        Ok(GatewayResult {
            schema_version: 1,
            operation,
            connection: Some(c.name),
            http_status: None,
            elapsed_ms: elapsed(started),
            data,
        })
    }
    .await;
    result.map_err(|mut e: GatewayError| {
        e.elapsed_ms = Some(elapsed(started));
        e
    })
}

fn discover_executable(
    explicit: Option<&Path>,
    override_path: Option<&OsStr>,
    search_path: Option<&OsStr>,
) -> Option<PathBuf> {
    // Same fail-closed precedence as opsail-usage's Codex resolver: an invalid
    // explicit override never silently falls back. Resolve before changing cwd.
    if let Some(path) = explicit {
        return executable(path);
    }
    if let Some(path) = override_path.filter(|p| !p.is_empty()) {
        return executable(Path::new(path));
    }
    env::split_paths(search_path?).find_map(|dir| executable(&dir.join("grok")))
}
fn executable(path: &Path) -> Option<PathBuf> {
    let meta = path.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    path.canonicalize().ok()
}

struct Sandbox {
    directory: TempDir,
    profile: String,
    skipped: usize,
}
impl Sandbox {
    fn create(base: &Path, home: &Path) -> Result<Self, GatewayError> {
        let home = home
            .canonicalize()
            .map_err(|_| sandbox_error("could not resolve HOME"))?;
        let base = base
            .canonicalize()
            .map_err(|_| sandbox_error("could not resolve the temporary directory"))?;
        if home.parent().is_none() || base.starts_with(&home) {
            return Err(GatewayError::new(
                "unsafe-temp-directory",
                "input",
                "Grok temporary directory must be outside HOME; no fallback is used",
            ));
        }
        let (deny, skipped) = deny_paths(&home)?;
        let directory = tempfile::Builder::new()
            .prefix("opsail-gw-")
            .rand_bytes(20)
            .tempdir_in(base)
            .map_err(|_| sandbox_error("could not create the Grok temporary directory"))?;
        let profile = directory
            .path()
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| sandbox_error("invalid temporary directory name"))?
            .to_owned();
        let config = directory.path().join(".grok");
        fs::create_dir(&config)
            .map_err(|_| sandbox_error("could not create sandbox configuration directory"))?;
        // JSON string escaping is also valid for these TOML basic strings. Paths
        // with control characters or Grok's unsupported glob syntax are skipped.
        let content = format!(
            "[profiles.{profile}]\nextends = \"workspace\"\ndeny = {}\n",
            serde_json::to_string(&deny)
                .map_err(|_| sandbox_error("could not encode sandbox deny paths"))?
        );
        fs::write(config.join("sandbox.toml"), content)
            .map_err(|_| sandbox_error("could not write sandbox configuration"))?;
        Ok(Self {
            directory,
            profile,
            skipped,
        })
    }
}
fn deny_paths(home: &Path) -> Result<(Vec<String>, usize), GatewayError> {
    let mut deny = Vec::new();
    let mut skipped = 0;
    for entry in fs::read_dir(home).map_err(|_| sandbox_error("could not enumerate HOME"))? {
        let entry = entry.map_err(|_| sandbox_error("could not enumerate a HOME entry"))?;
        if entry.file_name() == ".grok" {
            continue;
        }
        let path = entry.path();
        let Some(path) = path.to_str().filter(|s| safe_deny_path(s)) else {
            skipped += 1;
            continue;
        };
        deny.push(path.to_owned());
        deny.push(format!("{path}/**"));
    }
    deny.sort();
    deny.push("/Volumes/**".into());
    Ok((deny, skipped))
}
fn safe_deny_path(s: &str) -> bool {
    // Grok 1.0.41 user-guide/18-sandbox.md: * ? [ always mean glob;
    // backslash escapes and brace alternation are unsupported. Never broaden
    // such a literal name into a glob; report the skipped top-level entry.
    s.trim() == s && !s.chars().any(|c| c.is_control() || "*?[]{}\\".contains(c))
}

struct OwnedChild(Box<dyn ChildWrapper>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}
async fn run(command: Command, timeout: Duration) -> Result<Vec<u8>, GatewayError> {
    let mut wrapped = CommandWrap::from(command);
    wrapped.wrap(KillOnDrop);
    #[cfg(unix)]
    wrapped.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    wrapped.wrap(process_wrap::tokio::JobObject);
    let mut child = OwnedChild(wrapped.spawn().map_err(|_| {
        GatewayError::new("grok-spawn-failed", "acquire", "could not start Grok CLI")
    })?);
    let stdout = child.0.stdout().take().ok_or_else(invalid_response)?;
    let stderr = child.0.stderr().take().ok_or_else(invalid_response)?;
    let result = tokio::time::timeout(timeout, async {
        let (out, _, status) = tokio::try_join!(
            read_bounded(stdout, false),
            read_bounded(stderr, true),
            async { child.0.wait().await.map_err(|_| invalid_response()) }
        )?;
        if !status.success() {
            return Err(GatewayError::new(
                "grok-failed",
                "acquire",
                "Grok CLI failed; check subscription login and model availability",
            ));
        }
        Ok(out)
    })
    .await
    .unwrap_or_else(|_| {
        Err(GatewayError::new(
            "request-timeout",
            "acquire",
            "Grok call timed out; the process group was terminated",
        ))
    });
    // Also kill descendants on successful parent exit and on output/protocol
    // errors. Drop handles cancellation; explicit wait reaps the direct child.
    let _ = child.0.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.0.wait()).await;
    result
}
async fn read_bounded(
    mut stream: impl AsyncRead + Unpin,
    stderr: bool,
) -> Result<Vec<u8>, GatewayError> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|_| invalid_response())?;
        if n == 0 {
            return Ok(bytes);
        }
        let limit = if stderr {
            64 * 1024
        } else {
            MAX_RESPONSE_BYTES
        };
        if bytes.len() + n > limit {
            return Err(GatewayError::new(
                "response-too-large",
                "acquire",
                "Grok output exceeds the gateway limit",
            ));
        }
        bytes.extend_from_slice(&chunk[..n]);
        if stderr && sandbox_warning(&String::from_utf8_lossy(&bytes)) {
            return Err(sandbox_error(
                "Grok could not apply the requested sandbox profile; output discarded",
            ));
        }
    }
}
fn sandbox_warning(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("sandbox could not be applied")
        || s.lines().any(|line| {
            (line.contains("profile") || line.contains("sandbox"))
                && [
                    "warning",
                    "not found",
                    "unknown",
                    "conflict",
                    "not exist",
                    "failed",
                    "could not",
                    "cannot",
                ]
                .iter()
                .any(|word| line.contains(word))
        })
}
fn parse_models(text: &str) -> Result<Value, GatewayError> {
    let mut default = None;
    let mut models = Vec::new();
    let mut in_models = false;
    for line in text.lines().map(str::trim) {
        if let Some(model) = line.strip_prefix("Default model:") {
            default = Some(model.trim().to_owned());
        }
        if line == "Available models:" {
            in_models = true;
            continue;
        }
        if in_models
            && let Some(model) = line.strip_prefix("* ").or_else(|| line.strip_prefix("- "))
        {
            let model = model.strip_suffix(" (default)").unwrap_or(model).trim();
            if model.is_empty()
                || model.len() > 256
                || model.chars().any(char::is_whitespace)
                || models.iter().any(|v: &String| v == model)
            {
                return Err(invalid_response());
            }
            models.push(model.to_owned());
        }
    }
    if models.is_empty() || default.as_ref().is_none_or(|d| !models.contains(d)) {
        return Err(invalid_response());
    }
    Ok(
        json!({"object":"list","data":models.iter().map(|m| json!({"id":m,"object":"model","owned_by":"grok","default":Some(m)==default.as_ref()})).collect::<Vec<_>>(),"defaultModel":default}),
    )
}
fn parse_chat(text: &str, requested_model: Option<&str>) -> Result<Value, GatewayError> {
    let mut data: Value = serde_json::from_str(text).map_err(|_| invalid_response())?;
    let reason = data
        .get("stopReason")
        .and_then(Value::as_str)
        .ok_or_else(invalid_response)?;
    let turns = data
        .get("num_turns")
        .and_then(Value::as_u64)
        .ok_or_else(invalid_response)?;
    if turns != 1 || reason != "end_turn" {
        return Err(GatewayError::new(
            "grok-incomplete-response",
            "protocol",
            "Grok attempted tools, exceeded its turn budget or returned a truncated response",
        ));
    }
    if !data
        .get("text")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
        || !data.get("usage").is_some_and(Value::is_object)
        || !data.get("sessionId").is_some_and(Value::is_string)
    {
        return Err(invalid_response());
    }
    let model = data
        .get("model")
        .and_then(Value::as_str)
        .or_else(|| {
            data.get("modelUsage")
                .and_then(Value::as_object)
                .filter(|m| m.len() == 1)
                .and_then(|m| m.keys().next().map(String::as_str))
        })
        .or(requested_model)
        .ok_or_else(invalid_response)?
        .to_owned();
    data["model"] = model.into();
    Ok(data)
}
fn parse_events(text: &str, requested_model: Option<&str>) -> Result<Value, GatewayError> {
    // Grok 1.0.41 can report end_turn + num_turns=1 even after a tool attempt
    // exhausts --max-turns. Observe native events as well, instead of accepting
    // its JSON summary's half-sentence preamble as a completed model answer.
    let mut answer = String::new();
    let mut thought = String::new();
    let mut end = None;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        if end.is_some() {
            return Err(invalid_response());
        }
        let mut event: Value = serde_json::from_str(line).map_err(|_| invalid_response())?;
        match event.get("type").and_then(Value::as_str) {
            Some("tool_call" | "tool_call_update" | "plan") => {
                return Err(GatewayError::new(
                    "grok-tool-attempt",
                    "protocol",
                    "Grok attempted a tool call; partial output discarded",
                ));
            }
            Some("usage") => {
                if event
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .is_some_and(|s| {
                        matches!(
                            s,
                            "tool_use" | "pause_turn" | "max_tokens" | "max_turn_requests"
                        )
                    })
                {
                    return Err(GatewayError::new(
                        "grok-incomplete-response",
                        "protocol",
                        "Grok model response was interrupted or attempted tools",
                    ));
                }
            }
            Some("text") => answer.push_str(
                event
                    .get("data")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid_response)?,
            ),
            Some("thought") => thought.push_str(
                event
                    .get("data")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid_response)?,
            ),
            Some("available_commands") => (),
            Some("end") => {
                event
                    .as_object_mut()
                    .ok_or_else(invalid_response)?
                    .remove("type");
                end = Some(event);
            }
            Some("error") => {
                return Err(GatewayError::new(
                    "grok-failed",
                    "acquire",
                    "Grok reported a model error; partial output discarded",
                ));
            }
            _ => return Err(invalid_response()),
        }
    }
    let mut data = end.ok_or_else(invalid_response)?;
    data["text"] = answer.into();
    if !thought.is_empty() {
        data["thought"] = thought.into();
    }
    parse_chat(&data.to_string(), requested_model)
}
fn sandbox_error(message: &str) -> GatewayError {
    GatewayError::new("sandbox-unavailable", "acquire", message)
}
fn invalid_response() -> GatewayError {
    GatewayError::new(
        "invalid-response",
        "protocol",
        "Grok CLI returned an incompatible response",
    )
}
fn elapsed(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests;
