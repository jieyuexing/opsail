# Opsail Host

This is the public `jieyuexing/opsail-host` fork of `lencx/opsail`. Keep upstream
history, license, and attribution. The integration branch is `local/opsail-host`;
repository renaming does not detach the GitHub fork or rewrite branches.

Read Git status before changes and preserve existing work. Do not commit,
publish, install extensions, collect real chat data, or change account bindings
without authorization for that operation.

## Ownership

- `crates/`: native read, XLSX, browser, usage, and existing refit capabilities.
- `packages/node/`: existing native Node API and upstream distribution contract.
- `packages/host/`: portable MCP/chat/browser core. Never import a private
  workspace, company adapter, tenant URL, credential binding, or real account ID.
- Instance adapters live outside this repository and depend on this core.
  Chat and browser bindings are runtime inputs, never source fixtures.
- T3 integration follows standard MCP Apps; UI work is a later stage. No T3
  source dependency or dedicated fork panel is part of this split.

## Routes

| Trigger | Command | Contract |
| --- | --- | --- |
| Public MCP tool discovery or standalone package boundary | `node packages/host/src/mcp-server.js`; `node --test packages/host/test/*.test.js` | [packages/host/README.md](packages/host/README.md) |
| Bounded chat reads or explicitly authorized collection | `packages/host/bin/chat <command>` | [chat contract](packages/host/src/chat/README.md) |
| Browser bridge installation/binding/diagnosis | `packages/host/bin/opsail-chrome <command>` | [browser contract](packages/host/src/browser/README.md); installation is a separate authorized action |
| Native XLSX changes | `cargo test -p opsail-xlsx`; `cargo test -p opsail --test xlsx_edit_cli` | [XLSX contract](crates/opsail-xlsx/README.md) |
| Native usage changes | `cargo test -p opsail-usage` | [usage contract](crates/opsail-usage/README.md) |

Source and fixture checks do not establish real account/browser collection,
extension installation, provider rendering, or workbook visual acceptance.
