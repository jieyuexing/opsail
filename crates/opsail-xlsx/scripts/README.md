# XLSX benchmark harness

`benchmark.py` measures a release binary through the public machine protocol.
It makes no changes to a UC workbook: `prepare` copies the explicitly supplied
saved UC source and candidate into a new benchmark directory, then creates
three deterministic synthetic workbooks.

```text
python3 scripts/benchmark.py prepare \
  --dir /absolute/opsail-xlsx-bench \
  --real-source /absolute/source.xlsx \
  --real-after /absolute/candidate.xlsx

python3 scripts/benchmark.py run --dir /absolute/opsail-xlsx-bench \
  --binary /absolute/opsail --label baseline --repetitions 3 --timeout 45
python3 scripts/benchmark.py run --dir /absolute/opsail-xlsx-bench \
  --binary /absolute/opsail --label optimized --repetitions 3 --timeout 45
python3 scripts/benchmark.py compare --dir /absolute/opsail-xlsx-bench baseline optimized
```

The fixed cases are the copied UC `inspect`, `diff`, and seven-operation patch;
plus `tall` (5,000 x 4), `wide` (50 x 400), and `styles` (2,000 x 10 with 512
fonts/style records).  Synthetic `after.xlsx` differs only in the final cell.
Each synthetic patch selects exactly 10,000 cells.  Use repeated `--case ID` to
retest a subset.

Every case has an unreported warm-up followed by separately started sample
processes.  `run.json` and each raw stdout are flushed after every sample, so a
timeout or malformed response is retained.  Reported figures are
median/minimum/maximum; three samples are deliberately never called p95.

`compare` requires matching stored protocol responses (`output` is removed for
patches) and compares every decompressed ZIP part payload of paired patch
candidates.  It omits a speedup when there is a failure, timeout, unequal
response, missing case, or unequal package payload.
