# Opsail Gateway development

Read `CONTRIBUTING.md` before changing code. Keep gateway logic in the sibling
`opsail-gateway` Rust crate, CLI parsing in `opsail`, and Node integration a thin
process adapter. Default tests must run offline without real credentials.

Preserve unrelated changes. Never write real API keys or passphrases to source,
fixtures, command arguments, logs, or examples. Keep stdout machine-readable and
diagnostics on stderr. Use Rust 1.97 and locked dependencies for verification.

This checkout is a contribution candidate. Commit, push, publish, and integration
into a consuming application require separate authorization.
