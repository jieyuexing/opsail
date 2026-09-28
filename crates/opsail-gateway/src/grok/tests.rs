use super::*;
use crate::{Adapter, Auth};

fn connection(path: &Path) -> Connection {
    Connection {
        name: "grok".into(),
        adapter: Adapter::GrokCli,
        base_url: "grok-cli:".into(),
        auth: Auth::None,
        default_model: None,
        grok_path: Some(path.to_string_lossy().into()),
        allow_http: false,
    }
}
fn request(value: Value) -> GatewayRequest {
    serde_json::from_value(value).unwrap()
}
fn chat() -> GatewayRequest {
    request(
        json!({"operation":"chat","connection":"grok","messages":[{"role":"user","content":"hello"}]}),
    )
}
fn success() -> Value {
    json!({"text":"Hello","stopReason":"end_turn","sessionId":"fixture-session","usage":{"input_tokens":4,"output_tokens":2},"num_turns":1,"modelUsage":{"grok-4.7":{"modelCalls":1}}})
}
fn stream_success() -> String {
    let mut end = success();
    end.as_object_mut().unwrap().remove("text");
    end["type"] = "end".into();
    format!("{}\n{end}\n", json!({"type":"text","data":"Hello"}))
}

#[test]
fn event_reader_rejects_tool_preamble_even_when_summary_claims_end_turn() {
    assert_eq!(
        parse_events(&stream_success(), None).unwrap()["text"],
        "Hello"
    );
    let bad = format!(
        "{}\n{}",
        json!({"type":"tool_call","toolName":"search_tool"}),
        stream_success()
    );
    assert_eq!(
        parse_events(&bad, None).unwrap_err().code,
        "grok-tool-attempt"
    );
    let bad = format!(
        "{}\n{}",
        json!({"type":"usage","stopReason":"tool_use"}),
        stream_success()
    );
    assert_eq!(
        parse_events(&bad, None).unwrap_err().code,
        "grok-incomplete-response"
    );
    assert!(parse_events("{\"type\":\"text\",\"data\":\"I'll read it\"}", None).is_err());
    assert!(parse_events(&format!("{}{}", stream_success(), stream_success()), None).is_err());
}

#[test]
fn model_listing_matches_openai_list_shape_and_marks_default() {
    let value = parse_models("You are logged in with grok.com.\n\nDefault model: grok-4.7\n\nAvailable models:\n  * grok-4.7 (default)\n  - grok-4.7-build-fast\n  - grok-4.6\n  - grok-4.5\n").unwrap();
    assert_eq!(value["data"].as_array().unwrap().len(), 4);
    assert_eq!(value["defaultModel"], "grok-4.7");
    assert_eq!(value["data"][0]["default"], true);
    assert_eq!(value["data"][1]["default"], false);
    assert!(parse_models("Available models:\n- other").is_err());
    assert!(parse_models("Default model: missing\nAvailable models:\n- other").is_err());
}

#[test]
fn rejects_truncation_missing_ledger_and_tool_rounds() {
    for (reason, turns) in [
        ("max_turn_requests", 1),
        ("tool_use", 1),
        ("max_tokens", 1),
        ("cancelled", 0),
        ("end_turn", 2),
    ] {
        let mut value = success();
        value["stopReason"] = reason.into();
        value["num_turns"] = turns.into();
        assert_eq!(
            parse_chat(&value.to_string(), None).unwrap_err().code,
            "grok-incomplete-response"
        );
    }
    let mut value = success();
    value.as_object_mut().unwrap().remove("num_turns");
    assert!(parse_chat(&value.to_string(), None).is_err());
    assert_eq!(
        parse_chat(&success().to_string(), None).unwrap()["model"],
        "grok-4.7"
    );
}

#[test]
fn sandbox_warning_detection_is_fail_closed() {
    for text in [
        "warning: sandbox could not be applied: unknown profile",
        "WARNING: project profile conflicts with user profile; using user profile",
        "Profile not found",
        "could not apply sandbox profile",
        "Warning: using user sandbox profile instead",
    ] {
        assert!(sandbox_warning(text), "{text}");
    }
    assert!(!sandbox_warning(""));
    assert!(!sandbox_warning("info: sandbox profile applied"));
}

#[test]
fn sandbox_lists_hidden_home_entries_skips_unsafe_globs_and_removes_cwd() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    for name in [
        ".grok",
        ".ssh",
        "project",
        "star*",
        "question?",
        "bracket[1]",
        "brace{a}",
    ] {
        fs::create_dir(home.join(name)).unwrap();
    }
    fs::write(home.join("top-file"), "fixture").unwrap();
    let sandbox = Sandbox::create(root.path(), &home).unwrap();
    assert_eq!(sandbox.skipped, 4);
    let path = sandbox.directory.path().to_owned();
    let content = fs::read_to_string(path.join(".grok/sandbox.toml")).unwrap();
    let paths: Vec<String> = serde_json::from_str(
        content
            .lines()
            .find_map(|l| l.strip_prefix("deny = "))
            .unwrap(),
    )
    .unwrap();
    assert!(paths.contains(&home.join(".ssh").to_str().unwrap().to_owned()));
    assert!(paths.contains(&format!("{}/**", home.join("top-file").display())));
    assert!(!paths.iter().any(|p| p.contains(".grok")));
    assert!(paths.contains(&"/Volumes/**".into()));
    assert_eq!(paths.len(), 7);
    let second = Sandbox::create(root.path(), &home).unwrap();
    assert_ne!(sandbox.profile, second.profile);
    drop(sandbox);
    assert!(!path.exists());
    assert_eq!(
        Sandbox::create(&home, &home).err().unwrap().code,
        "unsafe-temp-directory"
    );
    #[cfg(unix)]
    {
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&home, &alias).unwrap();
        assert_eq!(
            Sandbox::create(&alias, &home).err().unwrap().code,
            "unsafe-temp-directory"
        );
    }
}

#[tokio::test]
async fn rejects_input_and_unsupported_operations_before_spawn() {
    let c = connection(Path::new("/nonexistent-grok"));
    for op in [
        json!({"operation":"request","connection":"grok","method":"GET","path":"models"}),
        json!({"operation":"evaluate","connection":"grok","state":"x","questions":{}}),
    ] {
        assert_eq!(
            crate::client::call(c.clone(), request(op), &Secret::default())
                .await
                .unwrap_err()
                .code,
            "unsupported-capability"
        );
    }
    for message in [
        json!({"role":"user","content":[{"type":"image_url","image_url":{"url":"x"}}]}),
        json!({"role":"tool","content":"x"}),
        json!({"role":"assistant","content":"x","tool_calls":[]}),
        json!({"role":"user","content":null}),
    ] {
        assert_eq!(
            crate::client::call(
                c.clone(),
                request(json!({"operation":"chat","connection":"grok","messages":[message]})),
                &Secret::default()
            )
            .await
            .unwrap_err()
            .code,
            "invalid-request"
        );
    }
    for params in [
        json!({"temperature":0}),
        json!({"reasoningEffort":"invalid"}),
        json!({"reasoningEffort":1}),
    ] {
        assert!(prepare(&c,request(json!({"operation":"chat","connection":"grok","messages":[{"role":"user","content":"x"}],"parameters":params}))).is_err());
    }
    for timeout in [0, 3600001] {
        assert!(
            prepare(
                &c,
                request(json!({"operation":"models","connection":"grok","timeoutMs":timeout}))
            )
            .is_err()
        );
    }
    let mut keyed = c;
    keyed.auth = Auth::Bearer {
        key: Secret::new("fixture".into()),
    };
    assert!(crate::client::validate_connection(&keyed).is_err());
}

#[cfg(unix)]
struct Fixture {
    root: TempDir,
    home: PathBuf,
    binary: PathBuf,
}
#[cfg(unix)]
impl Fixture {
    fn new(body: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir(&home).unwrap();
        let binary = root.path().join("grok");
        fs::write(&binary, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        Self { root, home, binary }
    }
    async fn call(&self, c: Connection, r: GatewayRequest) -> Result<GatewayResult, GatewayError> {
        let operation = r.operation().to_owned();
        let p = prepare(&c, r)?;
        call_prepared(
            c,
            operation,
            p,
            &Secret::new("fixture-secret".into()),
            self.root.path(),
            &self.home,
        )
        .await
    }
}
#[cfg(unix)]
fn quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}

#[cfg(unix)]
#[test]
fn resolver_precedence_has_no_silent_fallback() {
    let f = Fixture::new("exit 0");
    let absent = f.root.path().join("absent");
    assert_eq!(
        discover_executable(
            Some(&absent),
            Some(f.binary.as_os_str()),
            Some(f.root.path().as_os_str())
        ),
        None
    );
    assert_eq!(
        discover_executable(Some(&f.binary), Some(absent.as_os_str()), None),
        Some(f.binary.canonicalize().unwrap())
    );
    assert_eq!(
        discover_executable(None, Some(f.binary.as_os_str()), None),
        Some(f.binary.canonicalize().unwrap())
    );
    assert_eq!(
        discover_executable(None, None, Some(f.root.path().as_os_str())),
        Some(f.binary.canonicalize().unwrap())
    );
    assert_eq!(
        discover_executable(
            None,
            Some(absent.as_os_str()),
            Some(f.root.path().as_os_str())
        ),
        None
    );
}

#[cfg(unix)]
#[tokio::test]
async fn fake_grok_maps_chat_system_model_effort_and_cleans_temp() {
    let f = Fixture::new("exit 0");
    let args = f.root.path().join("argv");
    let cwd = f.root.path().join("cwd");
    let prompt = f.root.path().join("prompt");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\0' \"$@\" > {}\npwd > {}\nwhile [ \"$#\" -gt 0 ]; do\n if [ \"$1\" = '--prompt-file' ]; then shift; cat \"$1\" > {}; fi\n shift\ndone\nprintf '%s' '{}'\n",
        quote(&args),
        quote(&cwd),
        quote(&prompt),
        stream_success()
    );
    fs::write(&f.binary, script).unwrap();
    let mut c = connection(&f.binary);
    c.default_model = Some("grok-default".into());
    let r = request(
        json!({"operation":"chat","connection":"grok","messages":[{"role":"system","content":"Spanish only"},{"role":"system","content":"No explanations"},{"role":"user","content":[{"type":"text","text":"first"}]},{"role":"assistant","content":"second"},{"role":"user","content":"third"}],"parameters":{"reasoningEffort":"low"}}),
    );
    let result = f.call(c.clone(), r).await.unwrap();
    assert_eq!(result.http_status, None);
    assert_eq!(result.data["model"], "grok-4.7");
    assert_eq!(
        fs::read_to_string(prompt).unwrap(),
        "[user]\nfirst\n\n[assistant]\nsecond\n\n[user]\nthird"
    );
    let bytes = fs::read(args).unwrap();
    let argv: Vec<&str> = bytes
        .split(|b| *b == 0)
        .filter(|b| !b.is_empty())
        .map(|b| std::str::from_utf8(b).unwrap())
        .collect();
    for pair in [
        [
            "--system-prompt-override",
            "Spanish only\n\nNo explanations",
        ],
        ["--model", "grok-default"],
        ["--reasoning-effort", "low"],
        ["--max-turns", "1"],
        ["--disallowed-tools", DISALLOWED_TOOLS],
        ["--deny", "MCPTool"],
        ["--deny", "*"],
    ] {
        assert!(argv.windows(2).any(|w| w == pair));
    }
    assert!(argv.contains(&"--disable-web-search"));
    assert!(argv.contains(&"--no-subagents"));
    assert!(
        Path::new(argv[argv.iter().position(|v| *v == "--prompt-file").unwrap() + 1]).is_absolute()
    );
    assert!(!Path::new(fs::read_to_string(cwd).unwrap().trim()).exists());
    let p = prepare(&c,request(json!({"operation":"chat","connection":"grok","model":"explicit","messages":[{"role":"user","content":"x"}]}))).unwrap();
    assert_eq!(p.model.as_deref(), Some("explicit"));
    c.default_model = None;
    assert!(prepare(&c, chat()).unwrap().model.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn fake_models_missing_binary_and_warning_exit_zero() {
    let f = Fixture::new(
        "printf 'Default model: grok-4.7\\nAvailable models:\\n  * grok-4.7 (default)\\n'",
    );
    let value = f
        .call(
            connection(&f.binary),
            request(json!({"operation":"models","connection":"grok"})),
        )
        .await
        .unwrap();
    assert_eq!(value.data["data"][0]["id"], "grok-4.7");
    assert_eq!(
        f.call(connection(&f.root.path().join("absent")), chat())
            .await
            .unwrap_err()
            .code,
        "grok-not-found"
    );
    for warning in [
        "warning: sandbox could not be applied: unknown profile",
        "warning: user profile conflicts with project profile",
    ] {
        fs::write(
            &f.binary,
            format!(
                "#!/bin/sh\nprintf '%s' '{warning}' >&2\nprintf '%s' '{}'\n",
                stream_success()
            ),
        )
        .unwrap();
        assert_eq!(
            f.call(connection(&f.binary), chat())
                .await
                .unwrap_err()
                .code,
            "sandbox-unavailable"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_and_cancellation_kill_descendants_and_remove_cwd() {
    for cancel in [false, true] {
        let f = Fixture::new("exit 0");
        let cwd = f.root.path().join("cwd");
        let pid = f.root.path().join("descendant");
        let escaped = f.root.path().join("escaped");
        let child_script = format!(
            "echo $$ > {}; sleep 1; echo leaked > {}; sleep 60",
            quote(&pid),
            quote(&escaped)
        );
        fs::write(
            &f.binary,
            format!(
                "#!/bin/sh\npwd > {}\n/bin/sh -c '{}' &\nwait\n",
                quote(&cwd),
                child_script.replace('\'', "'\\''")
            ),
        )
        .unwrap();
        let c = connection(&f.binary);
        let base = f.root.path().to_owned();
        let home = f.home.clone();
        let mut p = prepare(&c, chat()).unwrap();
        p.timeout = Duration::from_millis(if cancel { 5000 } else { 300 });
        let task = tokio::spawn(async move {
            call_prepared(c, "chat".into(), p, &Secret::default(), &base, &home).await
        });
        for _ in 0..100 {
            if pid.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(pid.exists(), "descendant never started");
        if cancel {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            assert_eq!(task.await.unwrap().unwrap_err().code, "request-timeout");
        }
        let descendant = fs::read_to_string(pid).unwrap();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(
            !escaped.exists(),
            "descendant survived and wrote after cancellation"
        );
        assert!(
            !std::process::Command::new("/bin/kill")
                .args(["-0", descendant.trim()])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success(),
            "orphan descendant still exists"
        );
        assert!(!Path::new(fs::read_to_string(cwd).unwrap().trim()).exists());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn stdout_is_bounded_and_nonzero_exit_is_not_success() {
    let f = Fixture::new("head -c 8388609 /dev/zero");
    assert_eq!(
        f.call(connection(&f.binary), chat())
            .await
            .unwrap_err()
            .code,
        "response-too-large"
    );
    fs::write(
        &f.binary,
        format!("#!/bin/sh\nprintf '%s' '{}'\nexit 1\n", success()),
    )
    .unwrap();
    assert_eq!(
        f.call(connection(&f.binary), chat())
            .await
            .unwrap_err()
            .code,
        "grok-failed"
    );
}
