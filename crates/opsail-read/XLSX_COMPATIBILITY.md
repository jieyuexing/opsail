# XLSX read-side compatibility and efficiency contract

This contract translates the persisted workbook features listed by
`rust_xlsxwriter` 0.99 into read-side observability. `rust_xlsxwriter` itself is
write-only; Opsail does not copy its API or claim that a structural inventory
is an Excel renderer.

## Capability levels

- **Semantic**: Opsail publishes the value or relationship an agent needs.
- **Structural**: Opsail reports bounded presence, count, range, or package-part
  evidence, but does not reconstruct the feature visually.
- **Unsupported**: Opsail cannot currently establish the feature from the
  workbook. Unsupported or partial behavior must remain explicit.

| rust_xlsxwriter persisted feature | Opsail read level | Current evidence |
| --- | --- | --- |
| Basic Excel data types | Semantic | Strings, numbers, booleans, errors, blanks, and stored dates |
| Cell formatting | Structural | Style index, number format, format counts; fonts/fills/borders are not resolved yet |
| Formulas | Semantic | Expression, cached value, normal/shared/array/data-table kind, shared index and formula range; no recalculation |
| Charts | Structural | Package chart and drawing-part counts |
| Hyperlinks | Semantic | Cell reference, internal location or relationship target, display text, external flag |
| Page/printing setup | Structural | Page-setup/header-footer presence plus print-area/title defined names |
| Merged ranges | Semantic when the worksheet scan is complete | Exact merge references |
| Conditional formatting | Structural | Bounded `sqref` list and rule count |
| Data validation | Structural | Bounded `sqref` list and rule count |
| Cell notes | Structural | Comment-part and legacy-drawing counts |
| Textboxes | Structural | Drawing presence only; textbox text is not extracted |
| Checkboxes | Structural | Worksheet control and control-property part counts |
| Sparklines | Structural | Sparkline element count |
| Images | Structural | Media and drawing-part counts; pixels and anchors are not rendered |
| Workbook themes | Structural | Theme-part count |
| Rich multi-format strings | Semantic text, structural runs | Concatenated base text plus a `richText` flag; East Asian `rPh` pronunciation hints are excluded from cell text and per-run formatting is not published |
| Outline groupings | Semantic | Outlined row/column counts and maximum levels |
| Defined names | Semantic | Name, scope, reference, and invalid `#REF!` signal |
| Autofilters | Semantic | Exact worksheet filter range |
| Worksheet tables | Structural | Workbook and worksheet table-part counts |
| Macros | Structural safety signal | VBA project part count; code is never executed or published |

Serde serialization and constant-memory writing are writer implementation
mechanisms rather than persisted workbook semantics, so they are not read-side
capability rows.

## Completeness labels

Default preview scans every worksheet to build a complete workbook feature
inventory, while publishing cells only from visible-sheet preview ranges.
Explicit range reads may stop after the last requested row. They set
`semanticBoundsComplete=false`, `features.complete=false`, and emit a warning
when the tail was not scanned. Declared worksheet dimensions never authorize a
dense allocation.

## Human/agent collaboration loop

The XLSX remains the human-owned source. Markdown is an agent-owned mirror with
replaceable generated blocks and unrestricted text outside those blocks.

1. `WorkbookSession::open` performs one bounded cold read and retains the
   generated selections, decoded shared strings, and styles.
2. An unchanged file-metadata stamp returns the cached result without opening
   worksheet XML. When the stamp changes, a central-directory revision compares
   each part's CRC32 and expanded size without expanding OOXML. Compressed size
   is a metric, not content identity, because Excel may repack unchanged parts.
3. No XLSX content change returns the cached result with zero expanded bytes.
4. A worksheet-only change uses the already-open package, refreshes the prior
   selections for changed sheets, reuses unchanged selections, and replaces
   only the corresponding generated Markdown/HTML blocks.
5. Workbook, shared-string, style, or theme changes trigger a conservative full
   refresh.
6. `merge_markdown_mirror` replaces only Opsail generated blocks. It preserves
   agent-authored text byte-for-byte and rejects malformed/nested markers.

## Performance acceptance

Compatibility and efficiency are separate metrics.

- Compatibility rate = successful cold, revision, and targeted reads divided
  by eligible non-temporary XLSX files.
- Expanded-byte saving = `1 - incremental expanded bytes / cold expanded bytes`.
- Read-time saving = `1 - incremental refresh wall time / cold-read wall time`.
- A collaboration read is **high efficiency** only when both savings are at
  least 80% and its refreshed selection matches a cold-read semantic projection.
- The agent-Markdown gate runs repeated unchanged refreshes across every valid
  workbook. The direct human-edit gate uses size-stratified real workbook
  samples, edits one visible numeric non-formula cell in a temporary copy, and
  requires at least 80% of samples to meet the high-efficiency definition. The
  declared repeated-Markdown plus one-human-edit cycle is reported separately.
  Concurrent throughput cannot turn a slow individual read into a pass.

Changing shared strings, styles, themes, or the workbook manifest is an
intentional negative example: it can affect many cells and therefore must take
the full-refresh path. The direct worksheet-only efficiency number must not be
generalized to those dependency-wide edits.

The corpus benchmark never writes the source workbooks and publishes no cell
content. Human-edit simulations operate only on temporary sanitized fixtures or
temporary workbook copies.
