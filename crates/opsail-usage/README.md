# opsail-usage

`opsail-usage` is the Rust library behind
[`opsail usage`](https://github.com/lencx/opsail#query-remaining-usage). It
queries remaining-usage windows from supported CLI providers and returns a
versioned, credential-free report. Claude, Codex and Grok are currently supported.

This crate does not attach to ChatGPT.app, inject renderer UI, or share code
with `opsail-refit-codex`. Sidebar display remains a refit concern.

## Capabilities

- Query all current providers or one named provider.
- Resolve and query each provider through its own adapter.
- Return ready and unavailable rows without blocking other providers.
- Keep credentials and raw provider responses out of reports and diagnostics.

The current Codex adapter uses a short-lived `codex app-server`. The current
Grok adapter uses the CLI auth file and the official grok.com billing endpoint.
The Claude adapter reads Claude Code's existing subscription OAuth credentials
and makes one GET to `https://api.anthropic.com/api/oauth/usage` with the
`anthropic-beta: oauth-2025-04-20` header. This is the endpoint used by Claude
Code's usage UI, not a versioned public API contract; unexpected responses
become unavailable, and HTTP 429 is reported without retrying.

## Installation

```toml
[dependencies]
opsail-usage = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Query remaining windows

```rust
use opsail_usage::{UsageOptions, read_usage};

#[tokio::main]
async fn main() {
    let report = read_usage(&UsageOptions::default()).await;
    for entry in report.providers {
        println!("{}", entry.provider.as_str());
    }
}
```

The default deadline is 15 seconds.

## Result contract

Reports use `schemaVersion: 1` and a `providers` array. Each row has
`provider` and `status` (`ready` or `unavailable`). Optional numeric fields are
omitted when unused. The library never returns raw RPC frames or auth material.

Claude adds an optional `windows` array without changing schema version 1.
Each window has `id`, `remainingPercent`, `usedPercent`, `windowDurationMins`
and optional `resetsAt` (Unix seconds). `five_hour` is 300 minutes; `seven_day`
and `seven_day_*` are 10080 minutes. Null windows are omitted. Percentages use
the endpoint's 0–100 units, not fractions. RFC3339 reset offsets are respected.
Paid extra usage is not a subscription window and is not projected.

The existing top-level fields mirror `five_hour`, falling back to `seven_day`
and then the first named weekly window when preceding windows are absent.
They do not aggregate windows: a weekly/model limit can be exhausted while
the five-hour window still has capacity. Inspect `windows` for all limits.
Codex and Grok retain their previous fields and omit `windows` entirely.

## Claude credentials

`--claude-auth PATH` (library: `claude_auth_path`) or `OPSAIL_CLAUDE_AUTH`
selects one credentials file exclusively. Otherwise macOS reads the current
user's `Claude Code-credentials` Keychain service, with Claude Code's SHA-256
directory suffix when `CLAUDE_CONFIG_DIR` is set. If Keychain is unavailable,
use `.credentials.json` under `CLAUDE_CONFIG_DIR`, or `~/.claude` by default.
Other platforms read that file directly. Config-directory strings are NFC
normalized as in Claude Code. These options accept paths, never tokens.

Only `claudeAiOauth.accessToken` is used; `expiresAt` must still be valid and
explicit scopes must include `user:profile`. Missing, expired or rejected
logins return `unavailable` with `claude auth login` for the user to run.
`planType` comes from the stored subscription type (known plan names only),
so a recent plan change may not appear until Claude Code updates its login.
API keys, cloud-provider credentials and inference-only setup tokens are
not subscription usage sources.

Credential reads are bounded to 1 MiB and HTTP responses to 2 MiB. Redirects
are disabled; credentials, response bodies and underlying transport errors
never enter diagnostics. There is no refresh, login, credential write or
persistent response cache. Each provider shares the concurrent query deadline.

Source: [Claude Code authentication documentation](https://code.claude.com/docs/en/authentication#credential-management).
Endpoint/header and Keychain naming were also checked against the installed
official Claude Code 2.1.74 client.

## Trust boundary

Callers pass file paths, not secrets. Opsail does not log in, refresh tokens, or
print credentials. A missing or unsigned-in provider becomes an unavailable row.
