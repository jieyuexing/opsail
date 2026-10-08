# Opsail Host core

This package owns the portable Node/Python layer of
[Opsail Host](https://github.com/jieyuexing/opsail-host): public MCP tool definitions,
bounded chat reads, chat collection, and the browser bridge. The native CLI and
its Rust crates remain at the repository root; `packages/node` retains the
existing native Node API.

It contains no company adapters, account bindings, credentials, or retained chat
data. Instance integrations compose these tools in their own repository. The
dependency direction is **instance → public core → native CLI**.

## Run from source

Use Node.js 22 or newer and Python 3.10 or newer. No Node dependencies are needed
for the MCP server. Browser-backed providers retain their documented platform
requirements; see [chat](src/chat/README.md) and [browser bridge](src/browser/README.md).

From the repository root:

```sh
cargo build --release --locked
node packages/host/src/mcp-server.js
packages/host/bin/chat status
```

The native binary is resolved from `OPSAIL_BINARY_PATH`, this checkout's
`target/release` or `target/debug`, then `PATH`. Its version must match `pin.json`.
When copying this package without the native source tree, provide an explicit
native binary. Missing native/browser dependencies fail when the corresponding
tool is called; listing tools does not start a browser or authenticate an account.

The standalone MCP server advertises only:

- `opsail_read`
- `opsail_xlsx_inspect`, `opsail_xlsx_patch`, `opsail_xlsx_diff`
- `opsail_usage`
- `opsail_chat_status`, `opsail_chat_catalog`, `opsail_chat_read`

The chat MCP tools read bounded data. Operator commands `collect`, `sync`,
`digest`, `run`, `prepare`, and `select` remain explicit CLI operations with their
existing write/navigation boundaries. Importing this package does not schedule
or run collection.

## Instance configuration

Private data defaults to `$XDG_DATA_HOME/opsail-host/retained-chat`, or
`~/.local/share/opsail-host/retained-chat` when XDG is unset. An instance can supply
`OPSAIL_CHAT_DATA_ROOT` and `OPSAIL_CHAT_BINDING_FILE` explicitly. Keep private
bindings and account identifiers outside the public source tree.

The enclosing workspace is not discovered or imported. `OPSAIL_SOURCE_DIR`,
`OPSAIL_PIN_PATH`, and `OPSAIL_RUNTIME_PACKAGE_DIR` let an existing installation
retain its native source, version policy, and wrapper identity. These are
per-process settings, not account or global configuration changes.

## T3 and MCP Apps

The selected integration route is an independent MCP App served by Opsail Host,
using standard MCP resources and UI metadata. T3 remains a consumer; this package
does not import T3 source or require a dedicated T3 fork panel.

The current delivery establishes the public core and standalone MCP boundary.
The first UI card and provider-specific rendering acceptance will be decided in
the next stage. No UI resource, T3 registration, or installed extension update is
claimed by this split.

## Verification

```sh
# TMPDIR/TMP/TEMP should point to a task-owned temporary directory.
node --test packages/host/test/*.test.js
```

The standalone test copies this package and restricts Node filesystem access to
that copy before initializing MCP and listing tools. Instance compatibility and
real provider/browser acceptance are separate checks.

This source package is marked `private` to prevent accidental npm publication.
The repository name does not rename the upstream `opsail` binary or claim
ownership of the upstream npm/crates.io package names.
