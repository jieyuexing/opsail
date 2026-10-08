# opsail-xlsx

Native, bounded inspection and editing of saved XLSX files. CLI transport is
`opsail xlsx inspect`, `patch`, `diff`, or `opsail xlsx --machine` for one JSON
request on stdin. The Host/MCP adapter projects this API and does not edit XML.

## Version 1 contract

Every request has `schemaVersion: 1` and `operation: inspect | patch | diff`.
Inspect takes an absolute `source` and 1–32 `ranges` such as `UseCase!A1:B4`.
Diff takes absolute `before` and `after` paths. Patch takes absolute `source`,
new `output`, `expectedSha256` from inspect, and 1–256 operations. Unknown fields
and fields belonging to another operation are rejected.

Supported patch operations:

| op | Fields besides op and sheet |
| --- | --- |
| setText | cell, expectedText, value, optional replaceRichText (default false) |
| setRichText | cell, expectedText, runs (1–256 nonempty text runs, optional fontColor/bold/strike per run) |
| setFormula | cell, expectedText, formula (plain expression, optional leading =) |
| setNumber | cell, expectedText, finite JSON number value |
| appendText | cell, expectedText, nonempty value, optional fontColor/bold/strike |
| setStyle | range, style with fontColor/fillColor/bold/strike/wrapText/horizontal/vertical/indent |
| copyStyle | range, fromCell, components drawn from font/fill/border/alignment/numberFormat |
| rowHeight | row, height in points (0–409) |
| columnWidth | column, width in Excel character units (0–255) |
| rowVisibility | row, hidden |

Missing cell and row targets are created in ascending OOXML order. New cells
inherit a custom row style, otherwise the covering column style, otherwise no
explicit style. Creation extends an existing dimension and leaves row spans
alone. New cells count toward the 200,000 stored-cell limit and all selected
targets count toward the 10,000 target budget. Covered merged cells cannot be
created; select the anchor. Untouched rows retain their original XML bytes,
including compact/pretty formatting, attribute order and entity spelling.
Edits preserve unrequested cell
metadata, alignment attributes and column-span properties. Shared style records
are appended/deduplicated, never rewritten; text edits create an inline string
rather than altering a shared string used elsewhere. A merged text target must
be its anchor; a style range cannot partially intersect a merge.

`setNumber` accepts plain strings, numeric cells and blanks. `expectedText`
matches the current plain text, the exact raw numeric `<v>` text, or `""` for
blank/missing cells. It writes the shortest round-trip f64 representation
(integers have no `.0`), removes the string type/content, and retains cell style.
`setText` by default accepts plain strings or blanks, not numeric cells.
`appendText` also accepts rich strings as described below.
`appendText` writes inline rich runs: the old and appended text each receive a
copy of the stored cell font; only the appended run receives the requested
color/bold/strike overrides. Empty old text produces one run. The cell style ID
does not change. `appendText` also accepts an existing rich string (shared or
inline): `expectedText` is the concatenated run text, every existing run and any
leading plain `<t>` is copied unchanged (shared-string elements take the
worksheet's prefix), and one run is added whose font copies the last run's
`rPr` in its stored order, with only the requested overrides replaced in place
or inserted in CT_RPrElt order; a last run without `rPr` uses the cell font.
The shared `<si>` is never edited, so other references keep it. Phonetic
annotations (`rPh`/`phoneticPr`), foreign XML inside the string, formulas, OOXML
escapes, ambiguous disabled font application and unsupported font/run
properties are refused. `setNumber` and `setFormula` still refuse rich strings.
`setText` refuses them unless `replaceRichText:true` is supplied. Its refusal
retains the original prefix and suggests the two replacement operations.

`setFormula` accepts a plain expression with or without a leading `=`; the
stored expression must contain 1–8192 UTF-16 units, legal XML characters and no
OOXML escapes. `expectedText` matches `=` plus existing formula text first,
otherwise plain string text, raw numeric/boolean `<v>` text, or `""` for a blank
or missing cell. Plain formulas may be replaced; shared/array/dataTable formula
targets, OOXML formula records and legacy `{=...}` values require native
application. Rich strings and escaped target text remain refused. Writing
removes `t`, `<is>` and cached `<v>`, retains style/metadata, and puts `<f>` before
`extLst`. The workbook receives one `calcPr fullCalcOnLoad="1"` request without
changing sheets/definedNames order or other calculation attributes. Formula
text participates in semantic diff; `xl/workbook.xml` is a changed part when the
calculation request changes. No result is calculated by this crate.

For `copyStyle`, **all five components** means **adopt the donor cell style
entirely**: its `xfId`, every apply flag, protection, alignment and extensions.
Records still use append/deduplication. Partial copies with different `xfId`
values fail with both target and donor references in the error.

Patch accepts optional `validateOnly: true`. `expectedSha256` is still required;
`output` is optional and ignored and no candidate or temporary publication file
is written. Operations run sequentially; a failing operation rolls back its
entire state (including style append caches) and validation continues. The exit-0
report contains `validateOnly`, `sourceSha256`, `violations`, `operationsChecked`,
`targetsProcessed` and `wouldChangeParts`. Targets count only successful
operations. Each violation has a **zero-based** `operationIndex`, `op`, `sheet`,
`target` and full human `message`. Normal patch stops at the first failure with
exit 2 and the same fields in `error`. Request/package errors before applying
operations have only a message.

Inspect accepts `detail: "full" | "compact"` (default `full`) and `includeParts`
(default true for full, false for compact). Compact reports stored style IDs,
font/fill/border/alignment/number-format summaries, row height/column width,
boolean formula/richText markers, cell text/value and merges. Null, false and
default fields are omitted; every cell always includes `sheet`, `cell`, `kind`
and `styleId`, with text (including empty text) or raw numeric value when present.
The compact/full serialized byte ratio is measured on 53 styled cells; a tenfold
reduction is a goal, not an acceptance gate. It omits
`styleContext`; full inspection retains the original detailed contract. Both
retain sheet summaries, source SHA, total/truncated counts and proof boundary.
Every successful inspect/patch/validation/diff response advertises
`protocolFeatures: ["createCells", "setNumber", "appendText",
"copyStyleAdoptBase", "validateOnly", "compactInspect", "setFormula", "insertRows",
"appendRichText", "semanticDiff", "setRichText"]`; schemaVersion remains 1.
The Host adapter advertises its separate `implementation.protocolVersion: 3`.

The package is read once into a bounded snapshot. Publication validates expanded
size and all modified XML, reloads the candidate with this crate's workbook
loader, raw-copies unchanged ZIP entries, checks the original
SHA again, and uses a same-directory 0600 temporary file with `persist_noclobber`.
Validation failures remove temporary files. Source and existing outputs are never
written. The source is not locked against a live Office session; use a stable saved
copy for a reviewed batch.

## Replace complete rich-text cells

`setText` with `replaceRichText:true` checks `expectedText` against the concatenated
visible text of the old shared or inline string and writes one plain inline `<t>`.
The **cell font still applies**, including `strike=1`: replacing the text of a
struck cell makes the entire replacement struck. Use `setRichText` with explicit
`strike:false` on the appropriate new runs to avoid that result.

```json
{"op":"setRichText","sheet":"UseCase","cell":"J5",
 "expectedText":"Old rule\nNew rule",
 "runs":[{"text":"Old rule","strike":true},
         {"text":"\nRevised rule","fontColor":"0000FF","strike":false}]}
```

Each new run copies the target's stored cell font through the existing run
constructor, then applies only that run's optional `fontColor`, `bold`, `strike`.
This replaces the complete contents; it does not keep any unlisted original run.
The style ID stays unchanged and shared `<si>` records remain untouched.
Blank/missing cells, plain strings and rich strings are accepted; numeric and
formula targets are refused. There must be 1–256 runs, each with nonempty text,
and at most 32,767 UTF-16 units across the complete replacement. Illegal XML
characters and OOXML `_xHHHH_` escapes in old or new text (including across new
run boundaries), phonetic annotations and foreign string XML are refused.
Merge-anchor checks, fixed SHA, candidate publication and `validateOnly` rollback
are the same as other text operations.

## Opt-in semantic user-edit diff

```text
opsail xlsx diff /absolute/last-output.xlsx /absolute/user-saved.xlsx --semantic
opsail xlsx diff /absolute/last-output.xlsx /absolute/user-saved.xlsx --semantic --align-rows
```

Machine/MCP requests add `semantic:true`; add `alignRows:true` for row insertion,
deletion or movement. `semantic` is valid only for diff. Presence of `alignRows`
(including false) requires `semantic:true`; when omitted alignment defaults false.
All existing `cellChanges`, `sheetStructureChanges`, `changedParts`, fingerprints
and other legacy diff fields keep their old comparison and meaning. They can
still contain WPS reserialization noise. Only the new `semantic` object uses
the following normalization:

1. Values have `kind` blank/number/string/formula/boolean/error/date and `text`.
   Numbers parse as finite f64 and use shortest round-trip strings (`1.10` equals
   `1.1`); malformed numeric text is retained conservatively. String text is the
   concatenated visible text, independent of shared/inline storage or `si`/`is`.
   Booleans normalize 0/false and 1/true. Formulas compare expression text; a
   shared follower without text becomes `shared:<si>`. Its `text` is `=<formula>`;
   stored results are separately `{kind,text}` in `cachedValue` (null if absent).
   No date-number interpretation, displayed number formatting or calculation runs.
2. Runs contain `{text,name?,size?,bold?,italic?,strike?,underline?,color?,
   vertAlign?,scheme?}`. A present `rPr` is self-contained: omitted flags are false
   and omitted other properties are absent, with **no cell-font inheritance**.
   An empty boolean element is true, `val="0"`/`"false"` is false. Runs without
   `rPr` and leading `<t>` use the cell font. `charset`/`family` are ignored.
   Empty runs are dropped and adjacent equal formats merge. A sole run whose
   format equals the cell font becomes `runs:null`, just like plain text.
3. RGB becomes uppercase 8-digit ARGB (6 digits gain FF). Theme colors retain
   `{theme,tint?}`, with zero tint omitted; indexed colors retain `{indexed}`;
   auto remains `{auto:true}`. **Theme/indexed colors are not resolved to RGB**;
   different representations remain semantic changes, even if they might render
   identically. Additional font properties such as outline/shadow remain visible.
4. Styles resolve the cellXfs component records: normalized font; pattern fill
   (`none` is no fill, solid ignores bgColor, gradients remain canonical XML);
   border side style/color (`none`/absent is no border) and diagonal flags;
   alignment (horizontal general, vertical bottom, false booleans, zero indent,
   textRotation and readingOrder defaults); existing built-in/custom number-format
   resolution; protection (locked true, hidden false); and quotePrefix false.
   Style indexes, xfId and apply* flags are ignored **only by semantic diff**.
   Missing cells use the same custom-row/column/default `style_id()` inheritance
   as stored cells. Omitted s and explicit s=0 are equivalent when their resolved
   styles match; an inherited strike font must not be confused with a different
   explicit default font. Unknown cell/style metadata is retained conservatively.
5. Cell `changes` tags are `type`, `text`, `formula`, `cachedValue`, `runs` (only
   when text is unchanged), `font`, `fill`, `border`, `alignment`, `numberFormat`,
   `protection`, `quotePrefix`, plus `metadata` for retained unknown fields.
   String `"1.2"` becoming number `1.2` always has `type`, even with equal text.
   Aligned inserted/deleted nonempty cells also have `inserted`/`deleted`.
6. `equivalentOnly` counts cells whose legacy value and/or resolved-style
   comparison differs but normalized values and styles match completely. Its
   byReason value/style counts may overlap; sample contains at most 20 Sheet!Cell
   references. Any normalized difference is kept in semantic cellChanges.
7. Rows compare height, customHeight, hidden, outlineLevel and normalized style
   only for customFormat rows. Height equal to defaultRowHeight without
   customHeight is omitted (null). Spans and XML attribute order do not matter.
   Explicit nondefault ht is compared even without customHeight.
8. Column spans expand to individual columns for width/hidden/outlineLevel/style
   comparison, then adjacent columns with equal before/after values coalesce.
   Width equal to defaultColWidth is null. customWidth/bestFit are ignored.
9. Merges compare ref sets. Print_Area/Print_Titles defined names map localSheetId
   using workbook sheet order, not alphabetical order; other names retain name,
   scope (sheet name or null), before/after text. Sheet order/name/visibility
   changes are in workbookChanges.sheets. Theme-part hash changes set themeChanged.
   sheetViews/bookViews go only to viewOnly; dimension is ignored. Sheet default
   heights/widths are compared numerically; other worksheet child elements retain
   canonical comparisons in otherStructure.
10. With alignment, each row signature hashes its nonempty cells' (column, kind,
    normalized text/formula), excluding style and formula cache. Row sequences
    include gaps through the two sheets' last stored row/merge endpoint. Myers
    matching uses linear memory; unmatched blocks with at most 200 rows on each
    side use a small DP maximizing equal-cell similarity, then number of pairs.
    Larger blocks pair their first min(before,after) rows positionally. Remaining
    rows are inserted/deleted. Cells have beforeCell (null for inserts); row-height
    details have beforeRow. Merge endpoints use the same map. Content-identical
    repeated rows can align to the wrong occurrence, **but differences are not
    hidden**: paired cells and row layouts still undergo the complete comparison.

The `semantic` object contains:

| Field | Meaning |
| --- | --- |
| cellChanges | total, bySheet, truncated, details [{sheet,cell,beforeCell?,changes,before,after}]; before/after include kind,text,formula,cachedValue,runs,style |
| equivalentOnly | total, bySheet, byReason:{value,style}, sample (max 20); exclude from the user-edit list |
| rowAlignment | [{sheet,inserted:[{afterRow,count}],deleted:[{beforeRow,count}],shifted:[{beforeRows,afterRows,offset}]}]; empty unless alignRows |
| layoutChanges | rows/columns each have total,truncated,details; merges, printAreas, sheetDefaults, otherStructure are arrays with counts in totals |
| workbookChanges | sheets:{before,after} or null; definedNames with definedNamesTotal/truncated; themeChanged |
| viewOnly | sheets and workbook; informational saved view state |
| summary | semanticTotal (cells + row records + column intervals + added/removed merge refs + print/name/default/other-structure entries + sheet-list/theme flags), equivalentTotal |
| normalization | One-line rule reminder; the detailed rules above are the contract |

Every detail/layout/alignment list is capped by maxCells (default 200, maximum
2,000) while totals span the whole bounded workbook. Nested added/removed merge,
structure-element and alignment lists also cap at maxCells and add `<field>Total>`
and `truncated` on overflow. rowAlignmentTotal counts sheets. semantic.truncated
covers its capped lists; layoutChanges.truncated covers layout lists. The 8 MiB
response budget retains semantic cell details first, then semantic layout/name/
alignment lists, then legacy cell details; affected truncated flags and
outputTruncated are set and totals remain intact. Legacy-only prefix behavior is
unchanged. Metadata too large even without details returns an explicit error.

When a user saved the previous output in WPS, compare **last output → user save**
with semantic:true, enumerate every semantic cell/layout/workbook edit and plan
how each is preserved or replayed. Compare **user save → new candidate** before
replacement and explain every remaining semantic difference; no user edit may
be reverted. Inspect equivalentOnly counts rather than consuming its samples as
edits. Any truncated/outputTruncated flag requires a larger maxCells or an
independently bounded batch comparison before a complete conclusion. Diff has no
sheet/range selector; do not pretend a partial response is a complete checklist.
Markdown from the reader is not a user-edit diff (grid-like sheets can be omitted).

This is stored-semantics evidence, not WPS rendering, print or business acceptance.
Unmodeled package parts remain available in the unchanged legacy report.

## insertRows

`{"op":"insertRows","sheet":"UseCase","before":10,"count":2,"styleFrom":"above"}`
inserts rows above `before` (1–1048576), with `count` 1–500. `styleFrom` defaults
to `above`: copy the immediately preceding row's attributes except `r`/`hidden`,
and empty cells for its explicit nonzero styles, without contents/formulas.
`none` or a missing preceding row creates plain rows. Row spans remain intact;
existing dimensions extend. Untouched rows above the insertion retain raw XML
bytes; a row whose formula changes is touched. Subsequent edits use new coordinates.
Patch responses include `rowsInserted: [{sheet, before, count}]` in operation
order (successful simulated inserts for `validateOnly`). Only inserted rows
consume the target budget; created style cells also consume the stored-cell limit.

Ranges in merges, CF/DV, hyperlinks, filters/sorts, protected ranges and sheet
views shift/grow. A1 row tokens shift in worksheet formulas, defined names
(including print areas/titles) and chart series, preserving other formula bytes.
Quoted sheet names, `$`, cell/whole-row ranges are supported. Other-sheet and
external-workbook references, whole-column ranges, structured labels and string
literals (including INDIRECT/OFFSET strings) stay unchanged. Unqualified refs
shift only in the target sheet or its locally scoped names, never global names.
Related DrawingML from/to markers, VML Anchor/Row, comments/threadedComments and
calcChain refs shift too; calcChain uses inherited 1-based sheet indexes (initial
1). Spanning anchors extend their end; absolute anchors, offsets and `editAs` stay.
[ECMA/ISO rowBreaks](https://learn.microsoft.com/en-us/dotnet/api/documentformat.openxml.spreadsheet.rowbreaks)
uses `brk.id=24` before B25: IDs `>= before - 1` increase by `count`, including
a break immediately before the insertion. Column breaks remain unchanged.

Located `requires native application` refusals cover shared/array/dataTable
cells or formula ranges touching displaced rows; any shared formula referencing
the target (even above); touching tables/pivot sources; spanning 3D references;
row/reference/anchor overflow; protected sheets; missing/malformed related parts.
Unresolved 3D spans and dynamic/unresolved named pivot sources are conservatively
refused because their affected range cannot be proved. Existing signed-package
refusals remain. All modified parts are staged atomically and re-parsed;
`validateOnly` collects refusals without partial changes. Publication reloads
the candidate. Recalculation, rendering and business acceptance remain native checks.

## Limits and proof

Default compressed/expanded limits are 64/256 MiB, hard limits 512 MiB each;
10,000 ZIP parts; 200,000 stored cells; 1 MiB requests; 10,000 selected targets.
`maxCells` defaults to 200 and is at most 2,000. Diff counts span the entire
supported workbook, while details are capped. Large cell details are trimmed to
an 8 MiB response budget and marked `outputTruncated`; callers must inspect
truncation flags. Metadata that alone exceeds this budget returns an explicit
error instead of emitting an oversized response. Sheet metadata is fingerprinted, while selected cell inspection
returns row, column and merge attributes.

This version supports UTF-8 transitional SpreadsheetML with conventional
`xl/workbook.xml`, worksheet sheets, a styles relationship and one stable main
namespace prefix per XML part. Unsupported forms fail explicitly. Protected
sheets, signed packages, edits that preserve existing runs while restyling only
selected runs, phonetic strings, OOXML text escapes, ambiguous
inherited style application and partial copies across incompatible donor base
styles require a native application. Column insertion, row/column deletion, direct merge changes and non-plain formula editing are not operations.

Diff resolves style records instead of comparing indexes, including inherited
bases and application flags. Stored formatting is not rendered formatting:
conditional formatting, native objects, pagination, text clipping, formula
recalculation and business acceptance remain outside this crate. Changed package
parts and unmodeled parts remain visible. Color conventions belong to the caller's
product documents, not this library.

## Verification

```text
cargo +1.97.0 test -p opsail-xlsx
cargo +1.97.0 clippy -p opsail-xlsx --all-targets -- -D warnings
cargo +1.97.0 test -p opsail --test xlsx_edit_cli
```

Regression fixtures cover shared strings, formula changes, style renumbering,
alignment preservation and removal, column-span properties, rich-text append
and rich-text replacement, merge-anchor/annotation rejection, size limits, conflicting candidate writers, cleanup, and XML
namespace/reference/newline handling. Real-workbook acceptance additionally uses
an independent XML parser and compares unmodified compressed ZIP payloads.

## Performance verification

Use [scripts/benchmark.py](scripts/benchmark.py) to prepare immutable inputs,
measure saved baseline and candidate binaries, and compare their results. See
[the benchmark protocol](scripts/README.md) for exact commands. The default suite
is synthetic only: 20,000-cell tall, wide and 512-style workbooks; patch stress
cases select 10,000 targets. `prepare --cases /absolute/cases.json` (or the
`prepare_dir` function) adds explicitly supplied workbooks and case definitions,
copied into the benchmark directory; there is no default real input, and
existing manifests and results keep working with `run` and `compare`. Each case
records a separate warm-up and
three fresh-process samples, input/binary SHA-256, failures and timeouts. Compare
requires identical inspect/diff responses and all candidate ZIP payloads, not
merely matching timings. Use `python3 scripts/test_benchmark.py` to check that
response differences, candidate differences and timeouts invalidate a speedup,
and that invalid external case definitions are rejected before anything is copied.

Row/cell indexes are local to each loaded worksheet and refer to physical XML
child positions. Creating missing rows/cells shifts the affected indexes before
the next edit or donor lookup. Style append caches are local to a mutable style document and must be
rebuilt if future operations change existing records. Inspection/diff style
resolution caches are never shared across workbook files. Candidate package
validation and source drift checks remain part of timed patch operations.
