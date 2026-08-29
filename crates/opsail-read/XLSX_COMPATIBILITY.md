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
| Cell formatting | Semantic for font strike and color; structural otherwise | Style index and number format remain available. Cell font strike and OOXML color (`rgb`, `theme`, `indexed`, `auto`, `tint`) are published; `resolvedRgb` is added only when direct RGB, theme, or the standard indexed palette can be resolved, with tint applied. Fills/borders are not resolved. |
| Formulas | Semantic | Expression, cached value, normal/shared/array/data-table kind, shared index and formula range; no recalculation |
| Charts | Structural | Package chart and drawing-part counts |
| Hyperlinks | Semantic | Cell reference, internal location or relationship target, display text, external flag |
| Page/printing setup | Structural | Per-sheet print-area/title defined names, page setup attributes, print options, bounded row/column break ids, and header/footer presence |
| Merged ranges | Semantic | Exact sheet and intersecting-selection merge references; selected anchor and covered cells publish merge membership without copying the anchor value |
| Conditional formatting | Structural | Bounded `sqref` list and rule count |
| Data validation | Structural | Bounded `sqref` list and rule count |
| Cell notes | Structural | Comment-part and legacy-drawing counts |
| Textboxes | Structural | Drawing presence only; textbox text is not extracted |
| Checkboxes | Structural | Worksheet control and control-property part counts |
| Sparklines | Structural | Sparkline element count |
| Images | Semantic when a bounded payload is returned; structural otherwise | Worksheet `twoCellAnchor`/`oneCellAnchor` pictures publish anchor cells/indexes, media part, MIME type, bytes, and SHA-256. Intersecting selections may add a capped `dataUri`; sheet inventories and cap-overflow entries remain metadata-only. No OCR or layout rendering. |
| Workbook themes | Structural | Theme-part count |
| Rich multi-format strings | Semantic text and font formatting | Concatenated `value`/`display` plus the compatible `richText` flag are retained. `richTextRuns[]` publishes each run's text, strike, and OOXML font color, including provable `resolvedRgb`. East Asian `rPh` pronunciation hints remain excluded from cell text. |
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
inventory, while publishing cells only from visible-sheet preview ranges. An
explicit range read may skip the contents of later `sheetData` rows, but it
always advances to worksheet EOF and parses post-`sheetData` merges,
autofilters, page/print setup, header/footer, and row/column breaks.

`features.cellDataComplete` reports whether every cell body was inspected;
`features.tailFeaturesComplete` reports whether the worksheet tail was reached;
the compatibility `features.complete` flag is true only when both are true.
`semanticBoundsComplete` follows cell-data completeness. Selection `truncated`
still means the cell cap prevented publication, never that worksheet tail
features were unread. Declared worksheet dimensions never authorize a dense
allocation.

Each selection publishes `usedBounds` from non-empty cells on its requested
rows, plus bounded left/right column overflow and any above/below row overflow
that was actually scanned. `mergedRanges` contains every merge intersecting the
requested rectangle, including merges whose anchor or far edge is outside it.
Returned cells in a merge carry `merge.range`, `merge.anchor`, and
`merge.role`. A requested covered cell may therefore be published as a blank
cell record; the anchor value is not invented on it.

For each scanned sheet, `pictures[]` inventories DrawingML worksheet pictures
without embedding pixels, bounded to 256 references per sheet/selection. Each entry publishes `sheet`, `fromCell`, optional
`toCell`, the corresponding zero-based row/column marker indexes, `mediaPart`,
`contentType`, `byteSize`, and `sha256`. Selection `images[]` contains only
pictures whose anchor intersects the requested rectangle. It adds `dataUri`
when the image fits the 2 MiB per-image and 4 MiB total raw-payload caps;
otherwise `payloadTruncated` is true and the metadata remains. The selection's
`imagesTruncated` reports image payload/reference limits independently from the
cell-only `truncated` flag. Base64 expansion stays within the Host 16 MiB
serialized-output ceiling. Picture bytes are not OCR text and are never copied
into cells. Charts, textboxes, and VML are not interpreted as pictures.

Per-sheet `print` evidence contains `printArea`, `printTitles`, structured
`pageSetup` (`orientation`, `paperSize`, fit/scale attributes), optional
`printOptions`, bounded `rowBreaks`/`columnBreaks`, and `headerFooter`. This is a
range hint, not reconstructed pagination or a rendering of margins.

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
5. Workbook, shared-string, style, theme, drawing, or media changes trigger a
   conservative full refresh.
6. `merge_markdown_mirror` replaces only Opsail generated blocks. It preserves
   agent-authored text byte-for-byte and rejects malformed/nested markers.

## Performance acceptance

Compatibility and efficiency are separate metrics.

- Compatibility rate = successful cold, revision, and targeted reads divided
  by eligible non-temporary XLSX files.
- A bounded worksheet read limits cell materialization and skips parsing later
  cell subtrees, but it still decompresses the worksheet through EOF to retain
  tail evidence. It therefore does not promise an expanded-byte saving over a
  full worksheet scan.
- Revision probes and unchanged refreshes still expand zero worksheet bytes.
- Read-time and retained-cell savings remain useful metrics, provided the
  refreshed selection matches a cold-read semantic projection.
- The agent-Markdown gate runs repeated unchanged refreshes across every valid
  workbook. The direct human-edit gate uses size-stratified real workbook
  samples, edits one visible numeric non-formula cell in a temporary copy, and
  records bounded-read timing and retained-cell counts. The
  declared repeated-Markdown plus one-human-edit cycle is reported separately.
  Concurrent throughput cannot turn a slow individual read into a pass.

Changing shared strings, styles, themes, or the workbook manifest is an
intentional negative example: it can affect many cells and therefore must take
the full-refresh path. The direct worksheet-only efficiency number must not be
generalized to those dependency-wide edits.

## Text-format protocol

Cells optionally publish `fontStrike`, `fontColor`, and `richTextRuns`. A color
preserves its OOXML source fields (`rgb`, `theme`, `indexed`, `auto`, `tint`).
`resolvedRgb` is a six-digit uppercase sRGB value only when Opsail can prove it
from direct RGB, `theme1.xml`, or the standard 0-63 indexed palette; automatic,
missing, out-of-range, or malformed colors remain unresolved rather than being
guessed. Theme and indexed colors apply OOXML tint in HSL luminance space.
Spreadsheet font theme indexes use `0=lt1`, `1=dk1`, `2=lt2`, `3=dk2`, then
the accent slots; this intentionally differs from DrawingML `clrScheme` XML
order. `sysClr.lastClr` is only a fallback, and an xf with `applyFont=false`
does not contribute its `fontId`.

Default Markdown and HTML projections add a `Text formatting` section whenever
a returned cell or rich-text run has strike-through or an explicit font color.
Conditional-format rendering is not evaluated, so it cannot supply a final
display color.

The corpus benchmark never writes the source workbooks and publishes no cell
content. Human-edit simulations operate only on temporary sanitized fixtures or
temporary workbook copies.
