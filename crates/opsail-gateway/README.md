# Opsail Gateway

Encrypted connection storage and bounded HTTP/model calls, exposed through
`opsail gateway` and the `opsail` Node package. This crate does not require a
workbench, agent framework, local model installer, or background service.

## Connections and credentials

```sh
opsail gateway init
opsail gateway connection set local --adapter openai-compatible \
  --base-url http://127.0.0.1:11434/v1 --auth none --model your-local-model
opsail gateway connection set cloud --adapter openai-compatible \
  --base-url https://provider.example/v1 --auth bearer --model your-cloud-model
opsail gateway connection list
opsail gateway models local
```

The CLI prompts for the API key and vault passphrase in the controlling terminal.
Each character displays `*`; Backspace edits, Ctrl-U clears, and Ctrl-C cancels.
Prompts and masking stay off stdout. Initialization confirms the passphrase;
`rekey` asks for the current password and confirms its replacement. No password
or API key flag is accepted. `connection set` replaces the complete named
connection, including its key. `connection model NAME [MODEL]` changes only the
default model and keeps the stored key; omit MODEL to clear it. `connection remove
NAME` deletes one connection.

By default, `vault.age` lives in the OS user's configuration directory under
`opsail/gateway`: Application Support on macOS, XDG_CONFIG_HOME (or ~/.config)
on Linux, and the user's Roaming AppData on Windows. `--data-dir DIRECTORY`
overrides this location. Gateway never infers a repository root.

The age v1 passphrase format encrypts a schemaVersion 1 JSON document containing
both metadata and credentials. Production encryption uses scrypt N=2^18, r=8,
p=1; decryption caps the work factor at 18. Use a strong, unique passphrase. There
is no stored master password, unlock daemon, plaintext export, or forgotten
password recovery. For backup, copy the encrypted vault while no writer is
active; restoring an older copy also restores older keys and connections.

Writes acquire a nonblocking process lock, reload the vault under that lock,
write ciphertext to a same-directory temporary file, sync it, then atomically
replace the vault. A busy writer returns `vault-busy`; retry explicitly. Wrong
passwords, invalid schema, damaged ciphertext, and pre-commit failures preserve
the original file. Unix files use mode 0600 and newly created directories 0700;
Windows files inherit the user configuration directory's ACL. A canceled vault
worker may finish its atomic operation; inspect the vault before repeating a
mutation. Rust secret values and decrypted buffers are zeroized on drop. Node
strings and operating-system process memory cannot provide a zeroization guarantee.

## Calls

```sh
printf '%s' '{"messages":[{"role":"user","content":"Say hello"}],"parameters":{"max_tokens":64}}' \
  | opsail gateway chat cloud
printf '%s' '{"method":"GET","path":"health","query":{"detail":"brief"}}' \
  | opsail gateway request cloud
```

`request`, `chat`, and `evaluate` read JSON from stdin or `--input FILE`; their
input omits `operation` and `connection`, which come from the CLI. `models`
requests `models` under the configured base path; an unsupported listing is an
HTTP error, not evidence that inference is unavailable. Model names are explicit
or come from the connection's optional `defaultModel`; there is no implicit model
selection or fallback.

HTTP adapters are `http`, `openai-compatible`, and `vercel-ai-gateway`; the
subscription `grok-cli` adapter is described below. HTTP authentication
is independently `none`, `bearer`, or `header` (requires `--auth-header`). Chat
uses `chat/completions`, keeps the base path such as `/v1`, forces `stream:false`,
and preserves provider parameters and the full response, including reasoning,
tool calls, and usage. It never runs returned tools. `parameters` may not replace
`model`, `messages`, or `stream`.

Generic HTTP requests support GET, POST, PUT, PATCH, DELETE, HEAD and OPTIONS,
relative paths, string query/header maps, and `body:{type:"json",value:...}` or
`body:{type:"text",value:"..."}`. Authentication and transport headers cannot be
overridden per call. HTTPS is required except for loopback addresses or an
explicit `allowHttp:true` / `--allow-http` connection. URL userinfo, queries in
base URLs, origin changes, and paths escaping the configured base are rejected.
Redirects and automatic retries are disabled. Loopback requests bypass proxies;
remote requests honor the platform/environment proxy configuration.

Requests and machine stdin are bounded to 1 MiB; UTF-8 responses to 8 MiB after
decompression. The native timeout defaults to 30 seconds and accepts 1–3600000 ms.
SSE, binary uploads, and streaming output are unsupported. Cancellation or a
timeout after dispatch cannot establish whether a remote write completed.
Credentials echoed verbatim in a provider response are redacted. No request
body, secret header, or arbitrary provider error message is logged.

## Vercel evaluation

```sh
opsail gateway connection set vercel --adapter vercel-ai-gateway \
  --base-url https://ai-gateway.vercel.sh --auth bearer --model typesafe-ai/jev
printf '%s' '{"state":"A full refund was issued.","questions":{"refunded":{"type":"boolean","instructions":"Was a refund issued?"}}}' \
  | opsail gateway evaluate vercel
```

The evaluation adapter follows `ai@7.0.107` / EvaluationModelV4: POST
`v4/ai/evaluation-model`, gateway protocol `0.0.1`, auth method `api-key`, model
specification `4`, and `ai-model-id`. No JavaScript AI SDK is needed at runtime.
State and instructions accept strings, objects, or arrays. Boolean questions
optionally supply `criteria.true`/`criteria.false`; choice uses a nonempty map of
option names to descriptions; score uses at least two ordered descriptions.
Criteria descriptions may be null. Boolean answers contain a **probability**, not
a Boolean. Answers, probabilities, rounding, usage, warnings and providerMetadata
remain intact. See `tests/fixtures/jev-*.json` for a synthetic protocol example;
these fixtures do not establish live model availability.

Contract source: the AI SDK's `@ai-sdk/gateway/src/gateway-evaluation-model.ts`
and `@ai-sdk/provider/src/evaluation-model/v4/evaluation-model-v4-question.ts` in
the `ai@7.0.107` dependency tree. Evaluation is experimental; a provider protocol
change requires a deliberate adapter and fixture update.

## Machine and Rust APIs

`opsail gateway --machine` takes one JSON object on private stdin:

```json
{
  "protocolVersion": 1,
  "request": { "operation": "models", "connection": "local" },
  "passphrase": "EXAMPLE_ONLY",
  "dataDir": "/path/to/user/gateway"
}
```

Never save real versions of this example to disk or shell history. `newPassphrase`
is supplied only for `rekey`; otherwise omit it. Supported operations are init,
list, set (connection object), set-default-model (name, optional model), remove (name), rekey, request, models, chat and
evaluate. Success is `{protocolVersion:1,ok:true,engine,result}` and exit 0;
failure is `{protocolVersion:1,ok:false,engine,error}` and exit 1. Result contains
schemaVersion, operation, data, elapsedMs, and optional connection/httpStatus.
The full response remains in data; usage is never estimated. Error includes code,
stage, message, retryable, and optional httpStatus/providerCode/elapsedMs. Machine
input is never echoed. Nonzero protocol versions and unknown fields are rejected.

Rust callers use `execute(Vault, GatewayRequest, Secret, Option<Secret>).await`;
the last argument is the new passphrase for rekey. Dropping a network future
cancels its owned HTTP request. `Vault` also exposes synchronous management methods.

Default tests are offline and use fictitious credentials. Unit vault tests use a
cheaper scrypt factor; CLI/Node cross-process tests use the production factor.
macOS, Linux, and Windows are covered by the Gateway workflow. A live provider
check must separately establish successful vault persistence, authentication,
and model output; mocked success or an HTTP account error does not satisfy it.

## Grok CLI subscription adapter

```sh
opsail gateway connection set grok --adapter grok-cli --auth none --model grok-4.7
# Optional persisted executable override:
opsail gateway connection set grok --adapter grok-cli --auth none --grok-path /absolute/path/to/grok
opsail gateway models grok
printf '%s' '{"messages":[{"role":"system","content":"Answer briefly."},{"role":"user","content":"Hello"}],"parameters":{"reasoningEffort":"low"}}' \
  | opsail gateway chat grok
```

Requires an already authenticated official Grok CLI subscription login in
`~/.grok`; Gateway never runs login, setup, update or logout. API-key authentication
is rejected, and the child does not inherit `XAI_API_KEY` / `GROK_API_KEY`.
`--auth none` prompts only for the vault passphrase. The fixed connection
`baseUrl` is `grok-cli:` (filled in by the human CLI). `grokPath` is optional and
persisted when supplied; the human CLI resolves relative overrides against its
invocation directory. Resolution is stored `--grok-path`, then `OPSAIL_GROK_PATH`,
then executable `grok` on PATH. An invalid explicit override fails with
`grok-not-found`, without trying another binary. Old connection objects retain
exactly their previous serialization when `grokPath` is absent. Encryption and
vault schema remain unchanged.

Only `models` and `chat` are supported. `request` and `evaluate` return
`unsupported-capability` before starting any child. Models returns the existing
OpenAI list shape (`data.data[]`), with `id`, `object`, `owned_by`, and a `default`
Boolean, plus `data.defaultModel` from `grok models`. Chat selects the explicit
model, then the stored default, then the CLI's own default. `data.model` reports
the actual model from the CLI ledger where available; it may differ from the
requested alias. Text, stopReason, sessionId, usage and num_turns are retained.
System messages are joined with blank lines and supplied as
`--system-prompt-override` (verified with Grok 1.0.41). User/assistant turns become
`[user]` / `[assistant]` labeled plain text, not native conversation history.
Content accepts strings or arrays consisting entirely of text blocks. Tool
messages, extra message fields and non-text blocks are rejected. Only canonical
`reasoningEffort` levels (`none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`)
are accepted; model support is decided by Grok. No generation parameter silently
falls through. System text is bounded to 32 KiB because the CLI accepts it on argv;
the conversation uses an absolute `--prompt-file` in the temporary cwd.

Every invocation creates a unique directory under `std::env::temp_dir()` and
removes it after the call. A temporary base inside the canonical HOME fails with
`unsafe-temp-directory`; there is no automatic fallback. Tests can inject a
separate base and fixture HOME without changing the process environment. macOS
and Linux are supported; other platforms fail closed. Temporary directories have
owner-only permissions. Grok may retain its ordinary native session/usage records
under `~/.grok`; deleting the temporary cwd does not erase those records.

The cwd contains a generated `.grok/sandbox.toml` profile with a random name,
`extends = "workspace"`, and kernel deny rules for every enumerated top-level
HOME entry (both the entry and its `/**` subtree), excluding only `.grok`, plus
`/Volumes/**`. Grok treats `*`, `?`, and `[` as glob syntax and does not support
backslash escapes or brace alternation. Paths with these characters, control
characters, unsupported syntax, or non-UTF-8 names are skipped, not widened into
patterns; `data.sandbox.skippedDenyPaths` reports their count. These skipped
entries, entries created after enumeration, other users' directories, system
paths and `~/.grok` are outside this deny policy. It is the explicitly bounded
HOME policy, not isolation of every filesystem path. Workspace does not restrict
network by itself.

Tool access is separately removed using internal IDs, `Agent`, a minimal
allowlist followed by the denylist, `--no-subagents`, `--disable-web-search`,
`--permission-mode dontAsk`, and deny-all plus Read/Grep/Bash/Edit/Write/WebFetch/
WebSearch/MCPTool rules. `--max-turns 1` bounds inference. The adapter internally
consumes bounded native events to reject any tool attempt, including a preamble
whose final summary misleadingly reports `end_turn` and one turn. Incomplete
responses return `grok-incomplete-response`; observed tool events return
`grok-tool-attempt`. External Gateway output remains one non-streaming result.
Any sandbox failure, missing profile or profile warning discards stdout and
returns `sandbox-unavailable`, even if the process exits 0. A warning is not a
successful sandbox qualification.

Calls keep the existing 30 s default / 3600000 ms maximum timeout and 8 MiB stdout
limit (including event framing); stderr is capped at 64 KiB and never echoed into
Gateway errors. Unix children run in a new process group, terminated on timeout,
output failure, cancellation and parent exit. No retry, session resume or orphan
agent is intended. Requalify tool IDs, events, sandbox behavior and login with
real Grok after changing its version; fake scripts only establish adapter logic.
