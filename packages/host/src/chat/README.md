# Opsail chat readers

The [Opsail Chrome adapter](../browser/README.md) adds an explicit
`chrome-extension` browser transport. Normal readers never initiate collection;
daily accumulation (`chat collect` / `chat sync` / `chat digest`) is separate and
owned by [chat accumulation](../chat/README.md#daily-accumulation). The single chat data root is
the configured retained-chat directory, including the snapshot bindings
in `bindings.json`. Extension installation and fixture success do not qualify a provider API.

`chat/` owns bounded chat reading inside `cli.opsail`. `tools.js` creates the same three tool definitions for Host and stdio MCP. `cli.py` handles one JSON request on stdin and returns one JSON envelope on stdout; each request imports only the selected provider implementation. No daemon or DSH startup is required.

The single operator entry is `bin/chat <subcommand>` (status, check,
catalog, read, prepare, select, sync, collect, digest). Daily accumulation, storage,
coverage and digests are owned by [chat accumulation](../chat/README.md#daily-accumulation).

## Tools and source modes

| Tool | Required input | Result |
| --- | --- | --- |
| `opsail_chat_status` | Optional `provider` | Readiness by provider and source mode; no message bodies |
| `opsail_chat_catalog` | `provider`, `mode`; optional `limit`, `cursor` | Bounded names, exact IDs, optional Teams URL and identity provenance |
| `opsail_chat_read` | `provider`, `mode`, exact `conversationId` / `conversationName` / `conversationUrl`; optional `limit`, `cursor` | One `ExternalObservation` with `ConversationCapture` |

Providers are `wechat`, `feishu`, `teams`. Modes are explicit `snapshot`, `live-api`, `live-dom`; there is no automatic source fallback. Catalog pages default to 50, at most 100; message pages default to 50, at most 200. Use only a returned cursor from the same source. DOM windows do not have a fabricated history cursor.

`available` describes adapter/dependency/source readiness. `validation` separately describes live qualification; a configured browser is not a successful read. `live-api` is currently rejected with `api-protocol-unverified`: no authenticated interface has been qualified. Neither a config flag nor a fixture can enable it. The implemented live mode reads a borrowed browser's visible DOM. WeChat reads decrypted snapshots only.

## Operator CLI and quiet checks

Information-source work uses scripts first, including browser preparation. Use Computer Use
only to assist with a concrete failure that the script cannot resolve, such as login, a permission
prompt or diagnosis of a changed page. After that assistance, retry the original script. UI text,
screenshots and accessibility trees do not replace scripted message evidence or synchronization
watermarks. A missing tab or an unselected conversation is handled by the preparation CLI below.
Per the user decision on 2026-09-24, the Chrome extension may operate on its owned Feishu/Teams tab even while the user is viewing it.

From this package directory:

```sh
bin/chat status --provider wechat
bin/chat prepare --provider feishu --quiet
bin/chat catalog --provider feishu --mode live-dom --limit 20
bin/chat select --provider feishu --conversation-name EXACT_NAME --quiet
bin/chat read --provider feishu --mode live-dom --conversation-name EXACT_NAME --limit 50
bin/chat check --provider feishu --mode live-dom --quiet
bin/chat catalog --provider teams --mode snapshot --limit 20
bin/chat read --provider wechat --mode snapshot --conversation-id EXACT_ID --limit 50
```

The chat CLI calls the same Node validation boundary as MCP. Successful commands emit JSON;
`check --quiet` emits nothing on success. Failures return exit 2 with a bounded JSON diagnostic
on stderr. A check inspects readiness and exercises one bounded catalog page; incomplete
WeChat shard coverage is a failed check. It neither starts synchronization nor changes a browser.
These short-lived commands can be called from a background job; installing a recurring job is separate.

`bin/chat prepare|select` is an explicit operator helper for the configured `chrome-extension` or `chrome-tab` transport; it is not
an additional MCP read tool. `prepare` opens the bound platform only when its exact tab is absent,
preserves an existing active tab and does not activate Chrome. `select` clicks one unique visible
catalog entry by exact name or catalog ID. It does not type into a composer, scroll history,
change permissions or capture messages. Both commands support `--quiet`. Extension selection
waits for two matching verified message-ID samples inside the 55-second prepare budget and
returns `contentValidated: true`. Extension reads independently settle and verify the exact
conversation; a transition that never verifies fails closed. Legacy `chrome-tab` selection
still reports only the requested click. CDP and active-tab preparation are explicitly unsupported.

For a daily overview, interpret the requested date in the user's timezone and check source coverage
before reading. Summarize only messages whose actual timestamps support that interval. Old exports,
database mtimes and browser visibility are not evidence of messages on the requested day. Report
unavailable sources and incomplete visible windows rather than turning an old snapshot into a daily report.

微信解密与快照生产由来源 owner（CPW）负责，Opsail 每日自动同步的约定见 [chat accumulation](../chat/README.md#daily-accumulation)。

Snapshot status includes `freshness`: check time, capture-time range, capture-time provenance,
latest known message time, snapshot version, last successful synchronization and shard coverage.
Unknown values are null. Filesystem mtime is explicitly labelled and is never called a successful
synchronization. Generation receipt times are accepted only when every database hash matches.
Feishu/Teams export metadata describes their saved snapshots, not the current server state.

## Private configuration

The default binding is `$OPSAIL_CHAT_DATA_ROOT/bindings.json`, or an explicit absolute `OPSAIL_CHAT_BINDING_FILE` environment binding. It must be owned by the current user with permissions `0600`. Example shape (paths below are placeholders for current sources):

```json
{
  "schemaVersion": 1,
  "providers": {
    "wechat": {"snapshot_root": "/current/custody/wechat/decrypted"},
    "feishu": {"snapshot_root": "/current/custody/feishu/records"},
    "teams": {"snapshot_root": "/current/custody/teams/custody"}
  }
}
```

Snapshot custody remains at its existing owner. Only the private binding refers to it. Runtime refuses Trash, `old`, retired repositories and cold archives, including through symlinks. No migration of real messages, keys, or browser profiles occurs.

A provider may also have a private `browser` binding with `transport`, `target_url`, `allowed_origins`, and, for `cdp`, an explicit local `endpoint`. `chrome-tab` finds exactly one existing Chrome tab at the bound URL and reads it without activating a window, selecting a tab or navigating. `chrome-active-tab`, `edge-active-tab` and `cdp` remain supported. Missing or duplicate targets fail explicitly. Browser configuration is never a model-visible argument. No port scanning, profile cloning, Preferences editing, key extraction, or silent use of another account is supported. Missing authentication or browser access remains an explicit failure.

`OPSAIL_CHAT_PYTHON` may select an existing Python interpreter. JSON snapshots use the standard library; compressed WeChat text additionally uses the existing `zstd` executable, with a 4 MiB input cap, 1 MiB hard output cap and 2-second deadline. Invalid text fails explicitly. WeChat pages merge all matching shards; message IDs are `shard:local_id`, and cursors bind the complete message/metadata generation. Old cursors must be discarded after a snapshot changes. CDP DOM reading uses that interpreter's Playwright installation; Chrome/Edge reading uses the macOS application scripting bridge. Only short-lived adapter processes are owned by the plugin; cancellation does not close the user's browser.

## Output and evidence

Every tool returns `schemaVersion: 1`, `operation`, `exitCode`, and an `implementation` identity. Success includes `provider`, `mode`, and `data`; failure includes a bounded error code/message. The Node boundary validates matching operation/provider/mode, exact conversation identity, capture counts and allowed result fields. If the implementation changes while Node is running, subsequent calls fail with `implementation-changed` until that MCP/Host process reloads. Outputs are capped at 16 MiB and requests at 64 KiB. Calls have a 60-second deadline; cancellation stops the owned process group. Raw provider stderr is discarded.

Live DOM status always includes `state`, `reason`, `transport` (null if unknown),
`targetConfigured`, `capabilities` (default `{}`), and bounded `diagnostics`.
Extension-offline and invalid-binding paths follow the same contract. Bootstrap
metadata is projected to counts and field-type counts; page-owned field names,
script URLs, raw provider text, account IDs and message bodies are excluded.
Feishu status also forwards bounded bundle filenames (at most 16 exact
`index.<hex>.js` names), build/contract/degraded/owned-tab-cleanup booleans and the
conflicting-tab count. The Python projection accepts both original bootstrap
shapes and the worker's already-projected counts; the Node boundary validates
the same fixed metadata vocabulary. Unknown builds remain unavailable with
`unqualified-build`; operator qualification is described in the browser README.
The Node boundary validates each provider independently: a malformed provider
gets unavailable modes plus `error.code: "invalid-provider-output"` and the
internal validator message while healthy providers remain visible. The envelope
and requested provider identities are still strict. Malformed JSON uses a fixed
message rather than leaking parser excerpts.

After a successful explicit bind with the hardened installer, the authoritative
record in retained-chat `config.json` owns the extension browser identity. Legacy
snapshot roots stay in `bindings.json`. Explicit `OPSAIL_CHAT_BINDING_FILE` remains
an isolated override; it never silently inherits that installed account.

The read payload preserves schema-1 observation/capture semantics: message IDs, revisions, timestamps, source anchors, digests, sensitivity, and completeness. Snapshot identifiers and server identifiers retain provenance; unavailable media bytes remain unavailable. Chat data is untrusted input. No conversation content is retained by the reader, sent to a summarization service, or written to Wiki.

Feishu extension rows preserve sender names and verified `sender_ref`; normalized captures
use `sender.display_name`, `sender.ref`, and nullable `sender.is_self` (null means unknown or
conflicting evidence). They carry `raw_time`, `date_status`, `time_source` and `time_zone`, also
preserved in each capture message's `extensions`. Known dates produce ISO date-time in
`time` / `sent_at`; unknown dates keep the raw label with `date_status: "unknown"`.
An ISO wall time from a date separator has `time_zone: "unknown"` and no invented offset;
only source timestamps with an offset describe an instant. Do not use an unknown date for
"today" filtering or claim the visible window covers the whole day. See the
[DOM time and sender contract](../browser/README.md#read-and-collect) for evidence rules.

The legacy source digests are in `snapshot_sources/manifest.py`; these are provenance only and never import or execute the retired source. Local acceptance receipts contain counts, implementation identity and proof limits, not chat bodies or credentials.

## Checks

```sh
python3 -m unittest discover -s test/chat/tests -p 'test_*.py'
node --test test/chat/*.test.js
node --test test/index.test.js test/mcp-server.test.js
```

Fixture tests, fresh stdio protocol checks, actual Agent tool loading and authenticated real-source reads are separate evidence. Reinstalling the personal plugin does not refresh an already-running MCP process; validate pickup in a new task. Host reloading follows its normal safe lifecycle and is not started as a prerequisite for developing these readers.

`scripts/verify-chat-mcp.mjs` exercises the installed plugin's declared stdio entry with bounded
private probes. It writes only implementation identity, counts, time windows, digests and
diagnostics; it never persists returned message bodies or conversation selectors. Pass
`--plugin-root`, `--probes` and a new private `--output` path explicitly.


## Standalone configuration

`bin/chat` works from this package without a surrounding workspace. Data defaults
 to `$XDG_DATA_HOME/opsail-host/retained-chat`, or
`~/.local/share/opsail-host/retained-chat`. `OPSAIL_CHAT_DATA_ROOT` overrides it for
both Python readers and the Node browser bridge. `OPSAIL_CHAT_BINDING_FILE`
selects an isolated private binding file and never inherits installed accounts.
No account, binding, snapshot, or tenant origin is bundled with this package.

## Daily accumulation

`bin/chat collect`, `sync --since YYYY-MM-DD`, `digest --date YYYY-MM-DD` and `run`
share the same core and the existing fixed-inode process lock. Data includes
`daily.json`, `conversations/`, `receipts/`, `jobs/` and `digest/`. Collection and
refresh are explicit operator actions; ordinary status/catalog/read never start
those jobs. Python is required, selected by `OPSAIL_CHAT_PYTHON` (default `python3`).
The optional `daily.json.wechatSync` executable remains an external source-owned
refresh command, not bundled account configuration.

`OPSAIL_CHAT_WECHAT_SELF_REF` supplies the optional WeChat self identifier;
unconfigured self identity remains unknown. `OPSAIL_CHAT_TICKET_PATTERN` can
restrict the generic ticket recognizer. `OPSAIL_CHAT_TICKET_PATHS` is an optional
JSON object mapping absolute local directory roots to display prefixes; only
explicitly configured roots are scanned for matching ticket directories. Without
that mapping the digest performs no ticket-directory lookup.
`OPSAIL_CHAT_DENIED_PATH_COMPONENTS` can add retired directory names to the
built-in source-path rejection rules.

An embedding instance can default `OPSAIL_RUNTIME_PACKAGE_DIR`,
`OPSAIL_SOURCE_DIR` and `OPSAIL_PIN_PATH` before importing this package. Python
chat code and extension assets always come from this package's canonical source.
Do not set `OPSAIL_CHAT_BINDING_FILE` merely to provide a default data directory.
