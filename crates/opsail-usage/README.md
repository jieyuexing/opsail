# opsail-usage

`opsail-usage` is the Rust library behind
[`opsail usage`](https://github.com/lencx/opsail#query-remaining-usage). It
queries remaining-usage windows from supported CLI providers and returns a
versioned, credential-free report. Codex and Grok are currently supported.

This crate does not attach to ChatGPT.app, inject renderer UI, or share code
with `opsail-refit-codex`. Sidebar display remains a refit concern.

## Capabilities

- Query all current providers or one named provider.
- Resolve and query each provider through its own adapter.
- Return ready and unavailable rows without blocking other providers.
- Keep credentials and raw provider responses out of reports and diagnostics.

The current Codex adapter uses a short-lived `codex app-server`. The current
Grok adapter uses the CLI auth file and the official grok.com billing endpoint.

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

## Trust boundary

Callers pass file paths, not secrets. Opsail does not log in, refresh tokens, or
print credentials. A missing or unsigned-in provider becomes an unavailable row.
