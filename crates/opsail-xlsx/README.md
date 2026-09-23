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
| setText | cell, expectedText, value |
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
`setText` and `appendText` accept plain strings or blanks, not numeric cells.
`appendText` writes inline rich runs: the old and appended text each receive a
copy of the stored cell font; only the appended run receives the requested
color/bold/strike overrides. Empty old text produces one run. The cell style ID
does not change. Existing rich strings, formulas, OOXML escapes, ambiguous
disabled font application and unsupported font properties are refused.

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
"copyStyleAdoptBase", "validateOnly", "compactInspect", "setFormula", "insertRows"]`; schemaVersion remains 1.

The package is read once into a bounded snapshot. Publication validates expanded
size and all modified XML, reloads the candidate with this crate's workbook
loader, raw-copies unchanged ZIP entries, checks the original
SHA again, and uses a same-directory 0600 temporary file with `persist_noclobber`.
Validation failures remove temporary files. Source and existing outputs are never
written. The source is not locked against a live Office session; use a stable saved
copy for a reviewed batch.

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
sheets, signed packages, rich-text text/font edits, OOXML text escapes, ambiguous
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
alignment preservation and removal, column-span properties, merge/rich-text
rejection, size limits, conflicting candidate writers, cleanup, and XML
namespace/reference/newline handling. Real-workbook acceptance additionally uses
an independent XML parser and compares unmodified compressed ZIP payloads.

## Performance verification

Use [scripts/benchmark.py](scripts/benchmark.py) to prepare immutable inputs,
measure saved baseline and candidate binaries, and compare their results. See
[the benchmark protocol](scripts/README.md) for exact commands. The fixed suite
covers a real UC copy and 20,000-cell tall, wide and 512-style workbooks; patch
stress cases select 10,000 targets. Each case records a separate warm-up and
three fresh-process samples, input/binary SHA-256, failures and timeouts. Compare
requires identical inspect/diff responses and all candidate ZIP payloads, not
merely matching timings. Use `python3 scripts/test_benchmark.py` to check that
response differences, candidate differences and timeouts invalidate a speedup.

Row/cell indexes are local to each loaded worksheet and refer to physical XML
child positions. Creating missing rows/cells shifts the affected indexes before
the next edit or donor lookup. Style append caches are local to a mutable style document and must be
rebuilt if future operations change existing records. Inspection/diff style
resolution caches are never shared across workbook files. Candidate package
validation and source drift checks remain part of timed patch operations.
