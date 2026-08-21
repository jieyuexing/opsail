use std::env;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use futures_util::future::join_all;
#[cfg(windows)]
use process_wrap::tokio::{ChildWrapper, CommandWrap, JobObject, KillOnDrop};
#[cfg(not(windows))]
use tokio::process::Child;
use tokio::process::Command;
use tokio::time::timeout;

use crate::error::UsageError;
use crate::grok;
use crate::model::{
    SCHEMA_VERSION, UsageEntry, UsageOptions, UsageProvider, UsageReport, UsageSnapshot,
    snapshot_from_rate_limits,
};
use crate::resolve::resolve_codex_executable;
use crate::rpc::JsonRpc;

/// Read remaining-usage windows for the selected providers.
pub async fn read_usage(options: &UsageOptions) -> UsageReport {
    let providers = join_all(
        options
            .selected_providers()
            .into_iter()
            .map(|provider| read_provider(provider, options)),
    )
    .await;
    UsageReport {
        schema_version: SCHEMA_VERSION,
        providers,
    }
}

async fn read_provider(provider: UsageProvider, options: &UsageOptions) -> UsageEntry {
    match provider {
        UsageProvider::Codex => match timeout(options.timeout, read_codex_usage(options)).await {
            Ok(Ok(snapshot)) => UsageEntry::from_codex(snapshot),
            Ok(Err(error)) => UsageEntry::unavailable(provider, error.to_string()),
            Err(_) => UsageEntry::unavailable(provider, UsageError::timed_out().to_string()),
        },
        UsageProvider::Grok => match timeout(options.timeout, grok::read_grok_usage(options)).await
        {
            Ok(entry) => entry,
            Err(_) => UsageEntry::unavailable(provider, "the Grok usage query timed out"),
        },
    }
}

async fn read_codex_usage(options: &UsageOptions) -> Result<UsageSnapshot, UsageError> {
    let binary = resolve_codex_executable(options.codex_path.as_deref())
        .ok_or_else(UsageError::codex_not_found)?;
    let mut child = spawn_app_server(&binary)?;
    let (stdin, stdout) = take_child_stdio(&mut child)?;
    let mut rpc = JsonRpc::new(stdout, stdin);
    let result = rpc.read_rate_limits(&options.client).await;
    stop_child(child).await;
    result.and_then(|payload| snapshot_from_rate_limits(&payload))
}

#[cfg(not(windows))]
type CodexChild = Child;
#[cfg(windows)]
type CodexChild = Box<dyn ChildWrapper>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppServerInvocation {
    program: OsString,
    args: Vec<OsString>,
}

fn spawn_app_server(binary: &Path) -> Result<CodexChild, UsageError> {
    let invocation = app_server_invocation(binary);
    let mut command = Command::new(invocation.program);
    command
        .args(invocation.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_remove("CODEX_THREAD_ID")
        .env_remove("CODEX_CI");

    if let Some(parent) = binary.parent().filter(|path| path.is_absolute()) {
        let mut path_value = parent.as_os_str().to_os_string();
        path_value.push(if cfg!(windows) { ";" } else { ":" });
        if let Some(existing) = env::var_os("PATH").or_else(|| env::var_os("Path")) {
            path_value.push(existing);
        }
        command.env("PATH", path_value);
    }

    spawn_owned_command(command)
}

#[cfg(not(windows))]
fn spawn_owned_command(mut command: Command) -> Result<CodexChild, UsageError> {
    command.kill_on_drop(true);
    command.spawn().map_err(|_| UsageError::spawn_failed())
}

#[cfg(windows)]
fn spawn_owned_command(command: Command) -> Result<CodexChild, UsageError> {
    let mut command = CommandWrap::from(command);
    command.wrap(KillOnDrop);
    command.wrap(JobObject);
    command.spawn().map_err(|_| UsageError::spawn_failed())
}

#[cfg(not(windows))]
fn take_child_stdio(
    child: &mut CodexChild,
) -> Result<(tokio::process::ChildStdin, tokio::process::ChildStdout), UsageError> {
    let stdin = child.stdin.take().ok_or_else(UsageError::spawn_failed)?;
    let stdout = child.stdout.take().ok_or_else(UsageError::spawn_failed)?;
    Ok((stdin, stdout))
}

#[cfg(windows)]
fn take_child_stdio(
    child: &mut CodexChild,
) -> Result<(tokio::process::ChildStdin, tokio::process::ChildStdout), UsageError> {
    let stdin = child.stdin().take().ok_or_else(UsageError::spawn_failed)?;
    let stdout = child.stdout().take().ok_or_else(UsageError::spawn_failed)?;
    Ok((stdin, stdout))
}

fn app_server_invocation(binary: &Path) -> AppServerInvocation {
    let comspec = env::var_os("COMSPEC").or_else(|| env::var_os("ComSpec"));
    app_server_invocation_for(binary, cfg!(windows), comspec.as_deref())
}

fn app_server_invocation_for(
    binary: &Path,
    windows: bool,
    comspec: Option<&OsStr>,
) -> AppServerInvocation {
    if windows && is_command_script(binary) {
        return AppServerInvocation {
            program: comspec
                .unwrap_or_else(|| OsStr::new("cmd.exe"))
                .to_os_string(),
            args: ["/d", "/s", "/v:off", "/c"]
                .into_iter()
                .map(OsString::from)
                .chain([
                    binary.as_os_str().to_os_string(),
                    OsString::from("app-server"),
                    OsString::from("--stdio"),
                ])
                .collect(),
        };
    }
    AppServerInvocation {
        program: binary.as_os_str().to_os_string(),
        args: ["app-server", "--stdio"]
            .into_iter()
            .map(OsString::from)
            .collect(),
    }
}

fn is_command_script(binary: &Path) -> bool {
    binary
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
}

async fn stop_child(mut child: CodexChild) {
    let _ = child.start_kill();
    let _ = timeout(Duration::from_secs(2), child.wait()).await;
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::{app_server_invocation_for, read_usage};
    use crate::model::{UsageOptions, UsageProvider, UsageStatus};

    #[tokio::test]
    async fn missing_binary_is_an_unavailable_codex_row() {
        let report = read_usage(&UsageOptions {
            providers: vec![UsageProvider::Codex],
            codex_path: Some(PathBuf::from("/opsail-missing-codex/codex")),
            timeout: Duration::from_secs(1),
            ..UsageOptions::default()
        })
        .await;
        assert_eq!(report.providers.len(), 1);
        assert_eq!(report.providers[0].provider, UsageProvider::Codex);
        assert_eq!(report.providers[0].status, UsageStatus::Unavailable);
        assert!(
            report.providers[0]
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("`codex login`")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_app_server_returns_a_snapshot() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        use tempfile::tempdir;

        let directory = tempdir().unwrap();
        let path = directory.path().join("codex");
        fs::write(
            &path,
            r#"#!/usr/bin/env python3
import json, sys

def read_frame():
    line = sys.stdin.readline()
    if not line:
        raise SystemExit(0)
    return json.loads(line)

def write_frame(frame):
    sys.stdout.write(json.dumps(frame) + "\n")
    sys.stdout.flush()

initialize = read_frame()
write_frame({"id": initialize["id"], "result": {}})
initialized = read_frame()
assert initialized["method"] == "initialized"
request = read_frame()
assert request["method"] == "account/rateLimits/read"
write_frame({
    "id": request["id"],
    "result": {
        "rateLimits": {
            "primary": {"usedPercent": 25, "resetsAt": 1786000000, "windowDurationMins": 300}
        }
    }
})
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();

        let report = read_usage(&UsageOptions {
            providers: vec![UsageProvider::Codex],
            codex_path: Some(path),
            timeout: Duration::from_secs(2),
            ..UsageOptions::default()
        })
        .await;
        let snapshot = &report.providers[0];
        assert_eq!(snapshot.status, UsageStatus::Ready);
        assert_eq!(snapshot.remaining_percent, Some(75));
        assert_eq!(snapshot.used_percent, Some(25.0));
        assert_eq!(snapshot.resets_at, Some(1_786_000_000));
    }

    #[test]
    fn windows_command_shims_use_comspec_with_fixed_app_server_arguments() {
        let invocation = app_server_invocation_for(
            PathBuf::from(r"C:\Users\agent\AppData\Roaming\npm\codex.cmd").as_path(),
            true,
            Some(OsStr::new(r"C:\Windows\System32\cmd.exe")),
        );
        assert_eq!(invocation.program, r"C:\Windows\System32\cmd.exe");
        assert_eq!(
            invocation.args,
            [
                "/d",
                "/s",
                "/v:off",
                "/c",
                r"C:\Users\agent\AppData\Roaming\npm\codex.cmd",
                "app-server",
                "--stdio",
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn windows_native_executables_do_not_cross_a_shell_boundary() {
        let invocation = app_server_invocation_for(
            PathBuf::from(r"C:\tools\codex.exe").as_path(),
            true,
            Some(OsStr::new(r"C:\Windows\System32\cmd.exe")),
        );
        assert_eq!(invocation.program, r"C:\tools\codex.exe");
        assert_eq!(invocation.args, ["app-server", "--stdio"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn every_provider_shares_one_concurrent_deadline() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        use serde_json::json;
        use tempfile::tempdir;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let directory = tempdir().unwrap();
        let codex = directory.path().join("codex");
        fs::write(
            &codex,
            "#!/usr/bin/env python3\nimport time\ntime.sleep(5)\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&codex, permissions).unwrap();

        let auth_path = directory.path().join("auth.json");
        fs::write(
            &auth_path,
            json!({
                "https://auth.x.ai::client": {
                    "key": "private-token",
                    "expires_at": "2099-01-01T00:00:00Z"
                }
            })
            .to_string(),
        )
        .unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;

        let started = Instant::now();
        let report = read_usage(&UsageOptions {
            codex_path: Some(codex),
            grok_auth_path: Some(auth_path),
            timeout: Duration::from_millis(100),
            grok_endpoint: Some(format!(
                "{}/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig",
                server.uri()
            )),
            ..UsageOptions::default()
        })
        .await;
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(report.providers.len(), 2);
        assert!(report.providers.iter().all(|entry| {
            entry.status == UsageStatus::Unavailable
                && entry.detail.as_deref().unwrap_or("").contains("timed out")
        }));
    }
}
