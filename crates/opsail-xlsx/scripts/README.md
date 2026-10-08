# XLSX benchmark harness

`benchmark.py` measures a release binary through the public machine protocol.
`prepare` creates three deterministic synthetic workbooks in a new benchmark
directory. Real workbooks are never read by default: pass `--cases` with an
absolute JSON file to copy explicitly named inputs alongside the synthetic set.

```text
python3 scripts/benchmark.py prepare --dir /absolute/opsail-xlsx-bench \
  [--cases /absolute/cases.json]

python3 scripts/benchmark.py run --dir /absolute/opsail-xlsx-bench \
  --binary /absolute/opsail --label baseline --repetitions 3 --timeout 45
python3 scripts/benchmark.py run --dir /absolute/opsail-xlsx-bench \
  --binary /absolute/opsail --label optimized --repetitions 3 --timeout 45
python3 scripts/benchmark.py compare --dir /absolute/opsail-xlsx-bench baseline optimized
```

The synthetic cases are `tall` (5,000 x 4), `wide` (50 x 400), and `styles`
(2,000 x 10 with 512 fonts/style records), each with `inspect`, `diff`, and
`patch`. Synthetic `after.xlsx` differs only in the final cell. Each synthetic
patch selects exactly 10,000 cells. Use repeated `--case ID` to retest a subset.

A `--cases` file is a list of `{"name", "files", "cases"}` definitions. `files`
maps workbook names to absolute paths; each case has `id`, `kind`, `inputDir`
(equal to `name`) and a protocol `request` whose `operation` equals `kind` and
whose workbook fields reference only that definition's files. Every definition
is validated before any directory is created. An existing benchmark directory
is reused as is, so a different definition needs a new directory.

Every case has an unreported warm-up followed by separately started sample
processes.  `run.json` and each raw stdout are flushed after every sample, so a
timeout or malformed response is retained.  Reported figures are
median/minimum/maximum; three samples are deliberately never called p95.

`compare` requires matching stored protocol responses (`output` is removed for
patches) and compares every decompressed ZIP part payload of paired patch
candidates.  It omits a speedup when there is a failure, timeout, unequal
response, missing case, or unequal package payload.
