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
Grok adapter uses the CLI auth file and the CLI proxy billing endpoint, with
the grok.com gRPC-web endpoint as a fallback (see below).
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
and optional `resetsAt` (Unix seconds) and `label` (the readable model name).
`five_hour` is 300 minutes; `seven_day` and `seven_day_*` are 10080 minutes. Percentages use
the endpoint's 0–100 units, not fractions. RFC3339 reset offsets are respected.
Paid extra usage is not a subscription window and is not projected.

Claude prefers the endpoint's nonempty `limits` array: `session` maps to
`five_hour`, `weekly_all` to `seven_day`, and `weekly_scoped` with a model
display name to `seven_day_<model>`. Model IDs use lowercase ASCII letters
and digits, replacing each other character with an underscore; `label`
preserves the display name. Unknown kinds and scopes without model names
are ignored. `percent` is the used percentage; `is_active` does not filter windows.
When `limits` is absent or empty, only the legacy `five_hour`, `seven_day`
and named `seven_day_*` objects with numeric `utilization` become windows.
Missing/null utilization (including `seven_day_breakdown`) is skipped;
other nonnumeric utilization is rejected. Unrelated code-name keys are ignored.

The existing top-level fields mirror `five_hour`, falling back to `seven_day`
and then the first named weekly window when preceding windows are absent.
They do not aggregate windows: a weekly/model limit can be exhausted while
the five-hour window still has capacity. Inspect `windows` for all limits.
Codex and Grok omit `windows` entirely; Grok projects its current billing period
into the existing top-level fields.

## Grok billing

Grok first makes a read-only `GET` to
`https://cli-chat-proxy.grok.com/v1/billing?format=credits`, the route used by
Grok CLI 1.0.41. It sends Bearer authorization, `X-XAI-Token-Auth: xai-grok-cli`,
JSON Accept and Opsail's own User-Agent. The live route was verified without a
user-id header or a Grok client-version header. These are internal client
endpoints, not a stable public API contract.

`config.creditUsagePercent` is the used percentage. For the legacy JSON shape,
derive it from `used.val / monthlyLimit.val` (positive limit only); Cent values
may be numbers or numeric strings, and an empty Cent object means zero.
Proto3 may omit a zero `creditUsagePercent`: accept this only with a recognized
weekly/monthly `currentPeriod` and valid ordered start/end timestamps. Empty,
null and unrecognized configs do not imply a full allowance. Paid on-demand
usage, prepaid balances and history are not included in subscription usage.

`resetsAt` is the current period end in Unix seconds; `windowDurationMins` is
the period's actual end minus start, respecting RFC3339 offsets. The legacy
date pair is used only when `currentPeriod` is absent/null. A known subscription
tier is projected as `planType` if supplied; missing tiers are omitted.
The checked-in `tests/fixtures/grok-credits.json` preserves the live response
structure with synthetic dates, amounts and a redacted top-up method.

If the proxy is unavailable or unrecognized, use the original read-only
`GetGrokCreditsConfig` gRPC-web POST on grok.com. Both attempts share the existing
provider deadline; the primary request reserves half of it for fallback.
HTTP 401, gRPC unauthenticated and explicit bad-credentials errors request
`grok login`. HTTP 403 carrying HTML or Cloudflare markers instead reports
`Grok 计费接口被 Cloudflare 拦截（非登录问题）`; other HTTP failures remain unavailable.
An expired local credential is rejected before any network request. No login,
refresh, credential writes, billing changes or inference calls are performed.
Redirects are disabled, authorization headers are sensitive, and raw responses
never enter diagnostics. Auth reads and responses are bounded to 1 MiB and 2 MiB.

Sources: official Grok CLI [billing handler](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-shell/src/extensions/billing.rs)
and [credit display mapping](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/src/app/effects/helpers.rs),
cross-checked with the installed 1.0.41 binary and a credential-free live probe.

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
