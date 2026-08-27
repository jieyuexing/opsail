# opsail-read

`opsail-read` is the Rust library behind
[`opsail read`](https://github.com/lencx/opsail#read-html). It acquires static
HTML or delegates rendered DOM capture to `opsail-chrome`, and it sparsely reads
bounded regions from local XLSX workbooks. Both paths return versioned artifacts
suitable for agents and other programmatic callers.

The extraction pipeline is browser-independent. `opsail-read` owns source
validation, non-browser acquisition, extraction, sanitization, and result
provenance. Browser executable discovery, owned process lifecycle, CDP target
management, waits, and DOM capture belong to `opsail-chrome`. Callers that
already have rendered HTML should provide it directly instead of using either
browser path.

## Capabilities

- Acquire HTML from HTTP(S), regular files, caller-provided stdin bytes, or an
  already-decoded captured document.
- Connect to caller-managed Chrome through an HTTP(S) discovery endpoint or
  browser/page WebSocket, optionally navigate, wait, and capture the current DOM.
- Launch a local Chrome or Chromium process with an isolated temporary profile,
  capture one page, and clean up the owned process and profile.
- Resolve relative links and assets against a validated HTTP(S) base URL.
- Extract readable Markdown and sanitized HTML with structured metadata.
- Read local XLSX sheet manifests and repeated `Sheet!A1:D20` ranges without
  allocating from declared worksheet dimensions.
- Parse shared and inline strings, stored values, formulas and cached results,
  supported date/time number formats, merged ranges, hidden state, and defined
  names from OOXML.
- Inventory hyperlinks, filters, tables, conditional formats, validations,
  page setup, outlines, drawings, charts, images, comments, controls,
  sparklines, themes, and VBA-project presence without executing active content.
- Compare ZIP-part revisions without expanding OOXML, refresh changed worksheet
  selections through `WorkbookSession`, and merge generated Markdown blocks
  without overwriting agent-authored text.
- Report source, extraction method, quality signals, and warnings through one
  stable result model.
- Enforce byte, DOM element, nesting-depth, redirect, and timeout limits.
- Reject active content, unsafe resource URLs, embedded URL credentials, and
  high-confidence full-page browser verification interstitials instead of
  publishing them as content.

## Installation

```toml
[dependencies]
opsail-read = "0.2"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Acquire and read a URL

```rust
use opsail_read::{ReadOptions, ReadSource, read};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = ReadSource::Url("https://example.com/article".parse()?);
    let result = read(source, &ReadOptions::default()).await?;

    println!("{}", result.metadata.title);
    println!("{}", result.content);
    Ok(())
}
```

`ReadOptions` controls the base URL, request and connection timeouts, maximum
input size, `User-Agent`, and `Accept-Language` header. For direct HTTP
acquisition, leaving `user_agent` as `None` sends `opsail/<version>`; WeChat
article URLs retain their browser-compatible automatic HTTP profile with an
`opsail/<version>` product token. An explicit value always wins.

## Read bounded XLSX ranges

Use `read_artifact` for auto-detection. A local file ending in `.xlsx` returns
`ReadArtifact::Workbook`; other inputs preserve the existing
`ReadArtifact::Document` path:

```rust
use opsail_read::{ReadArtifact, ReadOptions, ReadSource, read_artifact};

async fn inspect(path: std::path::PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let mut options = ReadOptions::default();
    options.spreadsheet.ranges = vec![
        "Summary!A1:H30".to_owned(),
        "'API Sheet'!B6:AY40".to_owned(),
    ];
    let artifact = read_artifact(ReadSource::File(path), &options).await?;
    if let ReadArtifact::Workbook(result) = artifact {
        println!("{}", serde_json::to_string_pretty(&result)?);
    }
    Ok(())
}
```

Repeated ranges are planned before extraction. The archive, workbook metadata,
shared strings, and styles are read once, and each selected worksheet XML part
is scanned once. Cells are stored sparsely by reference; a declared dimension
such as `A1:XFD1048576` never causes rectangular allocation.

With no explicit ranges, visible sheets receive a bounded preview controlled by
`preview_rows` and `preview_columns`; hidden sheets remain in the manifest.
`max_cells` bounds published non-empty cells and marks affected selections as
truncated. `max_expanded_bytes` limits cumulative uncompressed OOXML read from
the ZIP package. The normal `max_bytes` limit still applies to the compressed
file itself. Stdout adapters can call `WorkbookReadResult::truncate_published_cells`
and rerender a stable prefix when their serialized transport has a stricter byte
budget than the cell-count limit.

Formula expressions and cached values are reported separately. Opsail does not
recalculate formulas, so callers must not describe the cached value as current.
Legacy `.xls`, encrypted workbooks, macros, charts, images, conditional-format
rendering, and Excel-accurate visual layout are outside this reader.

The read-side feature levels, completeness labels, human/agent mirror contract,
and 80% efficiency gate are specified in
[`XLSX_COMPATIBILITY.md`](XLSX_COMPATIBILITY.md).

## Refresh a human-owned workbook without losing agent Markdown

`WorkbookSession` is the in-process collaboration API. Keep the XLSX as the
human-owned source and keep agent analysis outside the generated marker blocks:

```rust
use opsail_read::{ReadOptions, WorkbookSession, merge_markdown_mirror};

fn refresh(
    path: std::path::PathBuf,
    mut mirror: String,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut session = WorkbookSession::open(path, ReadOptions::default())?;
    if mirror.is_empty() {
        mirror = session.result().content.clone();
    }

    // The agent can append or edit text outside opsail:xlsx-generated blocks.
    mirror.push_str("\n\n## Agent analysis\n\nPending human confirmation.\n");
    let refresh = session.refresh()?;
    Ok(merge_markdown_mirror(&mirror, refresh.result)?)
}
```

An unchanged file uses a metadata fast path and expands zero OOXML bytes. A
worksheet-only change reuses cached shared strings/styles, parses only the
previous selections on changed sheets, and incrementally replaces generated
Markdown/HTML blocks. Workbook, shared-string, style, and theme changes take a
conservative full-refresh path. A full refresh is a correctness fallback, not
an efficiency failure hidden as a partial read.

Run the collaboration benchmark against a workbook tree with:

```sh
cargo +1.97.0 run --release -p opsail-read \
  --example xlsx_collaboration_benchmark -- \
  --md-rounds 4 --edit-samples 100 /path/to/workbooks
```

The benchmark never changes source files. It edits numeric, non-formula cells
only in temporary copies, compares every incremental selection with a cold
read, and verifies that agent-authored Markdown survives.

## Process caller-captured HTML

Browser hosts should capture the rendered HTML and final page URL themselves,
then provide both to `opsail-read`:

```rust
use opsail_read::{CapturedDocument, ReadOptions, ReadSource, read};

async fn process(html: String) -> Result<(), Box<dyn std::error::Error>> {
    let document = CapturedDocument::new(
        html,
        Some("https://example.com/final-article-url".parse()?),
    );
    let result = read(ReadSource::Html(document), &ReadOptions::default()).await?;
    println!("{}", result.content);
    Ok(())
}
```

`CapturedDocument` accepts an already-decoded Rust `String`. Its bytes are
treated as UTF-8; a legacy `<meta charset>` inside the document does not
reinterpret the Unicode text supplied by the caller.

For synchronous extraction with the default input-size limit, use
`extract_html(html, base_url)` instead.

## Capture through `opsail-chrome`

`ReadSource::Chrome` is the owned mode. It discovers or uses an explicitly
configured executable, starts headless Chrome with an isolated temporary
profile and a dynamically assigned loopback debugging port, captures one URL,
then stops the process and removes the profile:

```rust
use opsail_read::{ChromeSource, ReadOptions, ReadSource, read};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let chrome = ChromeSource::new("https://example.com/app".parse()?);
    let result = read(ReadSource::Chrome(chrome), &ReadOptions::default()).await?;
    println!("{}", result.content);
    Ok(())
}
```

Executable resolution supports macOS, Linux, and Windows in this order: the
`ChromeSource::executable_path` value, `OPSAIL_CHROME_PATH`, then supported
platform locations and `PATH`. Owned launch never reuses the user's Chrome
profile and does not add `--no-sandbox` automatically.

With no explicit `ReadOptions::user_agent`, owned launch derives the actual
User-Agent from the selected Chrome process and changes only its
`HeadlessChrome/<version>` product token to `Chrome/<version>`. It does not
hard-code a Chrome version. An explicit User-Agent is applied unchanged and
always takes precedence.

`ReadSource::Cdp` is the borrowed mode. The caller starts Chrome, exposes its
debugging endpoint, and owns the browser lifecycle. Opsail connects as a
short-lived client; it does not run an adapter server or background daemon.
When a navigation URL is supplied without a target ID, Opsail creates a
temporary `about:blank` target inside that browser, applies any explicit
User-Agent and language before navigation, captures the rendered DOM, and
closes only that temporary target:

```rust
use opsail_read::{CdpSource, CdpWaitUntil, ReadOptions, ReadSource, read};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut chrome = CdpSource::new("http://127.0.0.1:9222");
    chrome.url = Some("https://example.com/app".parse()?);
    chrome.wait_until = CdpWaitUntil::NetworkIdle;

    let result = read(ReadSource::Cdp(chrome), &ReadOptions::default()).await?;
    println!("{}", result.content);
    Ok(())
}
```

CDP capture first uses one `Runtime.evaluate` call to obtain HTML and the final
URL atomically. If `Runtime.evaluate` fails, it falls back to
`DOM.getOuterHTML` plus page navigation history. The current DOM does not expose
closed shadow roots, canvas pixels, or inaccessible cross-origin frame
documents.

When a browser endpoint is used without a navigation URL or `target_id`, Opsail
attaches only if exactly one eligible page target exists. Multiple pages return
an error instead of selecting an arbitrary page; callers must set `target_id`
explicitly. `direct_page` is valid only for a page-scoped WebSocket endpoint and
cannot be combined with `target_id`; the final URL of any existing page must use
HTTP(S). Captured HTML is limited by `ReadOptions::max_bytes` and an absolute 16
MiB CDP capture ceiling.

When `ReadOptions::user_agent` is `None`, borrowed CDP preserves the
caller-managed browser's User-Agent. An explicit value is applied unchanged
before navigation. Opsail deliberately does not normalize the identity of a
browser it does not own.

Both paths return the same captured-page shape to `opsail-read`, but provenance
remains explicit: owned launch produces `SourceKind::Chrome`, while borrowed
CDP produces `SourceKind::Cdp`. Cleanup of borrowed attachments and temporary
targets is guaranteed on normal completion and attempted on bounded failures;
if the operation is abruptly cancelled or the process is terminated, cleanup
is best-effort and the borrowed browser remains the caller's responsibility.

## Browser verification

Before extraction, `opsail-read` rejects high-confidence, full-page browser
verification interstitials with `ReadError::VerificationRequired`. The detector
uses structured and conjunctive evidence rather than regexes or generic page
wording:

- Cloudflare and AWS WAF use their published top-level response contracts:
  `cf-mitigated: challenge`, or the documented status plus
  `x-amzn-waf-action` combinations.
- WeChat, Cloudflare fallback pages, Google `/sorry/`, and top-level DataDome
  pages require multiple matching facts from the parsed DOM, trusted resource
  or form URLs, final-page URL constraints where applicable, and the absence of
  a substantive semantic content surface.

For Chrome/CDP sources, those DOM profiles also require a stable, visible live
marker from `opsail-chrome`'s privacy-bounded rendered observer. It measures
computed visibility, viewport intersection, paint-hit ownership, and animation-
frame stability. The observation is retained only when the root frame, loader,
and final URL remain the same. Missing, timed-out, or inconsistent evidence is
never treated as a positive. Direct HTTP and supplied HTML use conservative
static profiles because no live layout is available.

An embedded reCAPTCHA, hCaptcha, Turnstile, HUMAN/PerimeterX, or Arkose widget
is not sufficient evidence, and ordinary login pages are outside this
classification. Without an authoritative response contract or provider-owned
top-level route, rendered visibility and page-takeover evidence is required;
ambiguous static markup remains unclassified. The vendor set is conservative
rather than exhaustive. Opsail reports that verification is required; it does
not solve CAPTCHAs, complete third-party authentication, or bypass access
controls.

For Chrome sources, the response detector consumes only the optional
privacy-bounded main-document metadata exposed by `opsail-chrome`: status plus
normalized indicators derived only from `cf-mitigated` and
`x-amzn-waf-action`. Raw header values, cookies, authorization data, and
arbitrary response headers never enter this detection path. Frame, loader, and
response URL must also match the captured final main document.

## Result contract

`read` preserves the existing HTML-only `ReadResult`. `read_artifact` returns
the untagged `ReadArtifact` union: existing documents keep their version 1
shape, while workbooks carry `artifactKind: "workbook"` and their own version 1
shape. CLI and Node callers use the latter auto-detected contract.

An HTML `ReadResult` contains:

- `schema_version`: version of the serialized result contract.
- `content` and `content_html`: readable Markdown and sanitized HTML.
- `metadata`: title, author, dates, canonical URL, language, and related fields.
- `source`: input kind, requested and resolved locations, charset, media type,
  and byte count.
- `extraction`: selected extraction method and duration.
- `quality`: readability and content-size signals.
- `warnings`: non-fatal conditions such as unusually short extracted content.

Serialized fields use camel case, including `schemaVersion` and `contentHtml`.
Callers should branch on structured fields rather than warning or error text.

A `WorkbookReadResult` contains common `content`, `contentHtml`, `metadata`,
`source`, and `warnings` fields plus:

- `artifactKind: "workbook"`.
- `extraction.method: "ooxml-sparse"`.
- `workbook.sheets`: visibility, declared dimensions, scanned semantic bounds,
  merge and hidden-row/column metadata.
- `workbook.definedNames`: scope, reference, and explicit `#REF!` validity.
- `workbook.selections`: requested/resolved bounds, sparse cell records, and
  truncation.
- `workbook.statistics`: archive, expanded-byte, scan, style-only, and returned
  cell counts.

## Trust boundary

Treat source HTML and all extracted metadata as untrusted input. `opsail-read`
sanitizes its published HTML and filters unsafe URLs, but callers remain
responsible for safely rendering Markdown, escaping terminal output, and
applying any application-specific URL or content policy.

A borrowed CDP endpoint grants control over its Chrome session and may expose
cookies or authenticated pages. Accept it only from trusted caller
configuration. Endpoint URLs and query parameters are intentionally excluded
from `ReadResult` and public acquisition errors. Owned launch uses a fresh
temporary profile and therefore does not inherit the user's authenticated
browser state.

## License

Apache-2.0
