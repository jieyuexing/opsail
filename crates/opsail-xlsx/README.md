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
| setStyle | range, style with fontColor/fillColor/bold/strike/wrapText/horizontal/vertical/indent |
| copyStyle | range, fromCell, components drawn from font/fill/border/alignment/numberFormat |
| rowHeight | row, height in points (0–409) |
| columnWidth | column, width in Excel character units (0–255) |
| rowVisibility | row, hidden |

Plain text, style and row targets must exist. Edits preserve unrequested cell
metadata, alignment attributes and column-span properties. Shared style records
are appended/deduplicated, never rewritten; text edits create an inline string
rather than altering a shared string used elsewhere. A merged text target must
be its anchor; a style range cannot partially intersect a merge.

The package is read once into a bounded snapshot. Publication validates expanded
size and all modified XML, raw-copies unchanged ZIP entries, checks the original
SHA again, and uses a same-directory 0600 temporary file with `persist_noclobber`.
Validation failures remove temporary files. Source and existing outputs are never
written. The source is not locked against a live Office session; use a stable saved
copy for a reviewed batch.

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
inherited style application and incompatible donor base styles require a native
application. Insert/delete, merge changes and formula editing are not operations.

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
child positions. They are valid because v1 does not insert/delete/reorder rows
or cells. Style append caches are local to a mutable style document and must be
rebuilt if future operations change existing records. Inspection/diff style
resolution caches are never shared across workbook files. Candidate package
validation and source drift checks remain part of timed patch operations.
