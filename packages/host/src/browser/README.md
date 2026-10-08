# Opsail Chrome

Opsail Chrome connects explicitly bound Chrome profiles to the existing Opsail
chat readers. The extension uses fixed content scripts and Chrome Native
Messaging; the local native host exposes a private per-profile Unix socket.
The browser remains owned by its user. Requests never accept arbitrary scripts,
URLs, headers, cookies, or shell commands.

## Installation and binding

Run from this package directory:

```sh
bin/opsail-chrome install
bin/opsail-chrome doctor
bin/opsail-chrome bind --provider feishu --profile-id PROFILE_ID
bin/opsail-chrome bind --provider teams --profile-id PROFILE_ID
```

`install` registers the native host for the current macOS user and publishes a
stable **real directory** at `runtime/current/extension` under the retained-chat
root. Load that directory once through Chrome's **Load unpacked** action. An
existing load from `runtime/releases/<hash>/extension` needs this one final manual
change. Upgrades stage a complete sibling directory and exchange it atomically
(macOS `renamex_np`, Linux `renameat2`, via Python 3); unsupported atomic exchange
fails without a rename-away gap. Symlinks remain forbidden. The latest three
verified hash releases are retained; package and chat data are not pruned.

At a subsequent hello or request, a native/worker build mismatch returns
`build-changed`. The worker reloads only if its own unpacked directory now contains
the requested build, with a persisted one-attempt guard. An extension still loaded
from an old hash directory instead requires loading the stable path. The guard
clears after a successful hello. Neither installation nor fixture success proves
Chrome has picked up the build.

`doctor` inventories online profiles and stale runtime-record counts. Use
`doctor --provider feishu` or `doctor --provider teams` for an ordered one-shot
probe: native registration, bound-profile socket, handshake/build, site permission,
identity, pause, binding consistency, stale records. Each failed or
blocked check includes a next step; provider doctor exits 2 until all checks pass.
It sends only ping/diagnose and never creates, navigates, adopts or closes tabs.
An unprepared page yields an explicit `chat prepare --provider ...` hint;
there is no implicit preparation. Diagnostics contain only states, codes,
booleans, counts, exact bundle filenames and bounded field-type summaries. Normal host shutdown removes
only its own socket and metadata record; doctor reports bounded stale profile IDs and counts without deleting them.
Like `prune-runtime`, provider doctor never counts a profile bound by any provider as stale; an offline
bound profile is listed in `offlineBoundProfiles` and fails only its own provider's socket check.

Explicit stale metadata cleanup is available separately:

```sh
bin/opsail-chrome prune-runtime
bin/opsail-chrome doctor --provider feishu
```

`prune-runtime` inventories at most 128 profile records and prints `removedCount`,
each removed `runtime/<profileId>.json`, and each kept profile's reason. It removes
only private regular records whose filename/profile/socket identities agree,
whose profile is absent from **all** config bindings (including pending bindings),
and whose deterministic socket is absent or refuses connections. Live sockets,
timeouts, unsafe endpoints, damaged records and symlinks are preserved. Missing or
damaged config fails closed. The shared `runtime/config.lock` excludes install,
bind, qualification and native startup through socket readiness; locks are never
stolen. Native startup retries via the existing extension reconnect path if busy.
Cleanup never removes sockets, bindings, extension releases or chat packages.
An offline bound profile stays protected until an explicit verified rebind.
If configuration changes outside the lock or a removal fails, cleanup stops with
exit 2 and `completed: false`, retaining the list of records already removed.

No browser profile files are copied, read, or rewritten. If a site grant is
required, use the extension options page to grant the exact configured origins,
then retry bind. Pending identity or permission remains an error.

Successful `bind --provider ... --profile-id ...` verifies the candidate before a
single atomic configuration commit. The verified provider record in
`$OPSAIL_CHAT_DATA_ROOT/config.json` is marked `authoritative: true` and is the
single source of extension profile/account/tenant identity for chat, native host
and collection. Existing snapshot roots and legacy browser coordinates in
`$OPSAIL_CHAT_DATA_ROOT/bindings.json` are left byte-for-byte intact. Old records
keep their previous precedence until an explicit successful bind; no automatic
migration or account selection occurs. An explicit `OPSAIL_CHAT_BINDING_FILE`
continues to isolate all source selection and does not inherit installation data.

For an offline old profile, run:

```sh
bin/opsail-chrome bind --provider feishu --rebind
bin/opsail-chrome bind --provider teams --rebind
```

Rebind requires exactly one online replacement profile and verifies **both** its
account and tenant hashes against the stored identity, checking the online roster
again before publication. An online old profile, ambiguity, changed identity or
failed probe leaves both configuration files unchanged. The explicit binding
probe may prepare an owned background tab; it cannot send messages. Candidate
site coordinates travel only in this validated operator probe; normal read
requests cannot inject a binding. Installation and binding share
`runtime/config.lock`. After a crash, confirm no installer/binder is active before
removing that exact empty lock directory; locks are never stolen automatically.

`chrome-extension` remains an explicit transport selection. A failed operation
never falls back to AppleScript, another account, an old export, or another source.

The options page displays connection and profile state and allows pausing reads.
The extension only drives its own tabs and creates them in the background when possible.
Per the user decision on 2026-09-24, the extension may operate on its owned
Feishu/Teams tab even while the user is viewing it.
Ownership survives extension reload only when the recorded Chrome document ID
and exact source binding still match. A reused tab ID or changed document requires
a new `prepare`; matching URLs never authorize adoption of user tabs.

## Read and collect

```sh
bin/chat prepare --provider feishu --quiet
bin/chat catalog --provider feishu --mode live-dom --limit 20
bin/chat select --provider feishu --conversation-id EXACT_ID
bin/chat read --provider feishu --mode live-dom --conversation-id EXACT_ID --limit 50
```

原 `chat-sources` / `chat-browser` 已并入 `bin/chat <subcommand>`，两个包装脚本及其 DSH 镜像已删除。
`status`, `catalog`, and `read` are read-only. `live-dom` is a rendered page
window, not a historical completeness claim. Daily accumulation, Feishu page-SDK
backfill and digests are in [chat accumulation](../chat/README.md#daily-accumulation); the
earlier date collector and `browser-history` packages were retired on 2026-09-24
because no directory driver exists. Scripts are the normal path, including browser preparation. Computer
Use is only assistance for a concrete script blocker; UI text is never substituted
for an authenticated script observation.

Owned tabs live in one unfocused Opsail window (recorded in session storage and
recreated when closed). Before each page operation the target owned tab is moved
into that window if needed and made its active tab: freshly loaded Feishu and
Teams pages paint their lists only in an active tab. The user's own windows and
tabs are never activated, navigated or closed.

Extension `select` waits for two matching, scope-verified message-ID samples at
least 250 ms apart and returns `contentValidated: true`; it does not return message
bodies. Each `read` independently waits for two matching verified windows. The
whole operation uses the existing 55-second prepare budget. Only selection/pane
transitions are retried; account, tenant, document, ownership and other failures
remain terminal. An empty loading pane or a pane that never verifies fails closed.
No wait clicks, scrolls, navigates, or adopts a tab.

Feishu visible rows now return `sender_ref` when the current message model agrees
with the row ID and selected chat, and `is_self` is boolean only when message
identity or a local self marker proves it (otherwise null, including conflicts).
Sender names come from dedicated author nodes, an ID-matched chatter record, or
the selected p2p chat record whose `chatterId` equals the row's `fromId` (build
`index.8daec7ac.js` shows no author node in p2p chats). Group chats show the author
only on the first row of a run; later rows reuse that name only by exact verified
`fromId`, never by position, and an ID seen with two names gets no name. Quoted
authors are excluded. A known sender ID is the fallback when its display name is
unavailable (the operator's own rows in p2p chats have no name source). Names and IDs appear only in read content,
never diagnostics.

`time` becomes ISO date-time when a matching message's `createTime` (Unix seconds),
a semantic `time[datetime]`, or a date label establishes the day. Fixed date
separators are read in pane order before applying the result limit. Full calendar
dates and today/yesterday (including Chinese labels, relative to the page's local
calendar) are supported; month/day without a year and unrecognized labels stay
unknown. No arbitrary numeric data attribute is treated as a timestamp.
Each Feishu row includes `raw_time`, `date_status: known|unknown`,
`time_source: message-metadata|time-element|visible-time|date-separator|raw`, and
`time_zone: explicit-offset|unknown`. Date labels establish a local ISO wall time
without an invented UTC offset. Unknown dates retain raw time. These fields are
also preserved in capture message extensions, and `sender_ref` in `sender.ref`.
The real site's sender/date markup must still be rechecked after deployment;
synthetic tests do not qualify a new app build or full-day history coverage.

Feishu history beyond the visible window is paged with `chat sync --since D`
(page SDK `messagesPage` in the owned tab); storage, merge and coverage rules are
in [chat accumulation](../chat/README.md#daily-accumulation). The earlier legacy-anchored
`chat-sync-selected` publisher was retired on 2026-09-24 after its only
conversation was migrated.

## Source qualification

Transport readiness, verified account identity, and qualified history are
separate states. `extension/providers/index.js` is the source-adapter registry.
An adapter enters it only with exact read-only operations, stable account/tenant
identity, complete directory pagination, message pagination, and scope evidence.
A local flag or fixture cannot qualify an API. Credentials stay inside Chrome.

Current provider-wide browser-session API drivers are **not implemented or qualified**. The registry therefore
rejects provider-wide catalog and history pages with `browser-api-unqualified`; it does not invent an
endpoint or turn a DOM window into a complete historical package. DOM identity
extraction also requires real-page qualification: fixtures alone do not verify
the current Feishu or Teams site. Independently authenticated `live-api` remains
unverified. API qualification and real full-date acceptance must be recorded
before the v1 goal is declared complete.

The Feishu identity reader uses a filename allow-list **and** the fixed
`configurationAdapter.passport.getCurUserInfo()` behavioral contract. The original
`index.745e4057.js` implementation performs session `GET /web/user`; the returned
`user.id` must equal both `passport.userId` and `window.userId`, with a nonempty
`user.tenant.id`. DOM identity attributes cannot bypass these checks. User/tenant
IDs are hashed before Native Messaging and every bound operation checks both
stored hashes. Teams identity behavior is unchanged.

The retained-chat `config.json` schema remains version 1 with this optional field:

```json
{
  "identityBuilds": {
    "feishu": [
      {"name": "index.745e4057.js", "qualification": "built-in"}
    ]
  }
}
```

An absent field uses that original default; new installs persist it. An explicit
list replaces the default and may contain at most 64 distinct exact
`index.<6–64 lowercase hex>.js` names. Malformed configuration fails closed.
Only script resources from the existing qualified HTTPS CDN
`sf1-scmcdn-cn.feishucdn.com`, under `/static/js/`, are considered. Every observed
index bundle must be listed; no bundle, mixed unknown bundles or more than 16
observed names also fail with `unqualified-build`. Diagnostics expose at most 16
filenames, never URLs, query strings, page text or identity values. Filename
qualification does not verify bundle bytes; this version does not add SHA-256 pins.

After reviewing an exact filename reported in `diagnostics.bundleNames`, the
operator can run:

```sh
bin/opsail-chrome qualify --provider feishu --build index.OBSERVED_HEX.js
# Only when observation on the bound profile is not feasible, e.g. it is offline:
bin/opsail-chrome qualify --provider feishu --build index.OBSERVED_HEX.js --by-operator
```

Replace `index.OBSERVED_HEX.js` with the exact reported lowercase hexadecimal name.
Normal qualification uses only the **bound** profile, may explicitly prepare its
own background tab, and probes the requested bundle with the session-user
contract and both stored identity hashes. Only then does one atomic config write
append that name as `behavior-verified`. It never uses a replacement profile
implicitly. If observation is unavailable, qualification fails; it never falls
back automatically. The separate `--by-operator` action makes no browser request
and records `qualified-by-operator`, returning a clear note that the contract was
**not observed**. All subsequent bind/read operations still enforce that contract.
No build is auto-added; a duplicate is a no-op and does not change its provenance.
Qualification shares `runtime/config.lock` with install/bind and preserves all
provider bindings. This CLI command does not qualify history APIs or full-date
coverage; the existing native `qualify` history probe remains separate.

`bind`, `bind --rebind` and provider `doctor` carry bounded identity/build/tab
diagnostics through the worker, Native Messaging and CLI. Bootstrap output is
projected to counts and field-type counts before Native Messaging; unknown keys,
account/tenant IDs, raw provider errors and page-owned strings are discarded.
Feed-card IDs still require agreement between the row props and feed context.

Feishu permits one active messenger tab. Before creating a tab, the extension
queries only tab metadata for the exact bound origin. Any other tab at that
origin blocks creation with `target-tab-conflict`; it is never read, adopted,
navigated or closed. Close other Feishu messenger tabs, then retry (also close
any other tab at that exact origin if the conflict persists). A tab on
`/next/messenger/degraded` or `/messenger/degraded` returns `provider-degraded`
with the same next step. An unusable/degraded extension-created tab is closed
only when the document ownership or current-session creation record proves
ownership. A reused ID after restart is not enough to authorize deletion.
Doctor only reports these states: it never creates or closes tabs. When the old
bound profile is offline and exactly one other profile is online, it explicitly
points to `opsail-chrome bind --provider feishu --rebind` and its hash checks.

Teams identity uses the current authenticated React context's
`profile.objectId` and `profile.tenantId`, corroborated across attached contexts;
home-account and conversation tenants never substitute for the current tenant.
Both site readers corroborate selected sidebar IDs with message-row and pane
IDs in one script before returning a visible window. They reject recycled rows,
inconsistent panes, duplicate message IDs, and changing account contexts.
React-backed reads resolve membership in the currently committed component tree;
the DOM node's original Fiber can describe an earlier selection. Traversal is
bounded and cached within one script invocation. A selected title alone never
proves that the message pane has completed navigation. Retained nodes from an
earlier conversation are rejected when their stable pane/row IDs do not match.

Feishu qualification includes fixed one-item directory and selected-message
probes. They return field shapes, counts and identity/anchor checks only. A
successful probe still leaves history unqualified: folded folders, hidden and
archived branches, message page boundaries and replies require separate evidence.
The selected-message probe obtains its anchor from the observed source model;
relative DOM labels and invented positions cannot authorize a history request.

The separate Feishu `messagesPage` route reads only the already selected exact
conversation. It checks the authenticated account, tenant, selected feed, and
current DOM message anchor before a fixed `GET_CHAT_MESSAGES` request. It does
not qualify catalog or full-date collection. Local opaque continuation handles
are pinned to the account, tenant, conversation, Chrome document and installed
build, expire after 30 minutes, and derive positions only from validated prior
responses. Consumers must verify page overlap and gaps before advancing any
incremental watermark. The provider's `dataComplete` flag alone does not prove
that the entire conversation history has been collected.

An explicit `OPSAIL_CHAT_BINDING_FILE` isolates source selection from the fixed
installation record. This keeps fixture and separately configured reads from
inheriting the operator's installed Chrome accounts.

Resume also requires a newly verified `resumeAnchor` from the source driver to
match the saved source snapshot. A local checkpoint digest alone cannot establish
that the remote directory or message pagination still describes the same source.
Without that evidence, the collector preserves progress and rejects resume.

Completeness requires every supported directory surface and every discovered
subject to finish. Teams includes chats, teams, channels, old threads with new
replies, and reply pagination. A root post outside the date cannot exclude a
reply inside it. Missing branches, changed accounts, unstable IDs, relative-only
timestamps, and cursor loops remain explicit gaps. Edited/deleted content is only
reported when actually observed; absence cannot prove deletion.

## Data layout

The extension installation (`config.json`, `runtime/`) shares the single chat data
root `$OPSAIL_CHAT_DATA_ROOT/` with the snapshot bindings and accumulated
conversations described in [chat accumulation](../chat/README.md#daily-accumulation).
Directories use 0700 and files use 0600. Chat data has no TTL or automatic deletion;
the three-release retention applies only to reconstructible extension builds.
Uninstall removes the native registration while preserving all data. Existing
workbench snapshots and WeChat database custody remain at their current owner.

## Verification

```sh
node --test test/browser/*.test.mjs test/browser/native/*.test.mjs test/browser/providers/*.test.mjs
# Optional: launches isolated Chromium with synthetic pages; omit when browser launch is prohibited.
python3 -m unittest discover -s test/browser -p test_extension_browser.py -v
python3 -m unittest discover -s test/chat/tests -p 'test_*.py'
node --test test/chat/*.test.js test/index.test.js test/mcp-server.test.js
```

Chromium tests use synthetic pages and a temporary independent profile; their
copied manifest removes nativeMessaging permission and the production fixed key.
They
prove the extension execution boundary, not production source coverage. Native
messaging tests exercise framed stdio and real Unix sockets. Report code tests,
installed Chrome build identity, account/source qualification, and actual full
date coverage separately. A running MCP must reload after adapter changes.

The implementation follows Chrome's [Native Messaging contract](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging),
the official [unpacked extension loading flow](https://developer.chrome.com/docs/extensions/get-started/tutorial/hello-world),
and Playwright's [isolated extension test setup](https://playwright.dev/docs/chrome-extensions).
