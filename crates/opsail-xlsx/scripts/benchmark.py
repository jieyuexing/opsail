#!/usr/bin/env python3
"""Reproducible process-level benchmarks for ``opsail xlsx --machine``.

The harness deliberately measures the shipped CLI as a fresh process.  It does
not benchmark fixture construction, JSON preparation, or cache warm-up as part
of a reported sample.  It also retains every response, including failures.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import time
import zipfile
from pathlib import Path
from statistics import median
from typing import Any


SCHEMA = 1
ROOT = Path(__file__).resolve().parents[6]
REAL_SOURCE = ROOT / ".local/opsail-xlsx/20260908-native-acceptance/source.xlsx"
REAL_AFTER = ROOT / ".local/opsail-xlsx/20260908-native-acceptance/candidate.xlsx"
MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
CT = "http://schemas.openxmlformats.org/package/2006/content-types"
PKGREL = "http://schemas.openxmlformats.org/package/2006/relationships"
LABEL = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$")


def die(message: str) -> None:
    raise SystemExit(f"benchmark: {message}")


def absolute(value: str) -> Path:
    path = Path(value)
    if not path.is_absolute():
        die(f"path must be absolute: {value}")
    return path


def digest(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def json_write(path: Path, value: Any) -> None:
    temporary = path.with_name(f".{path.name}.tmp-{os.getpid()}")
    with temporary.open("w", encoding="utf-8") as out:
        json.dump(value, out, ensure_ascii=False, indent=2, sort_keys=True)
        out.write("\n")
        out.flush()
        os.fsync(out.fileno())
    os.replace(temporary, path)


def zip_entry(zip_file: zipfile.ZipFile, name: str, payload: bytes) -> None:
    info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
    info.compress_type = zipfile.ZIP_DEFLATED
    info.external_attr = 0o600 << 16
    zip_file.writestr(info, payload, compresslevel=6)


def col_name(number: int) -> str:
    text = ""
    while number:
        number, remainder = divmod(number - 1, 26)
        text = chr(65 + remainder) + text
    return text


def cell_xml(column: int, row: int, value: str, style: int = 0) -> str:
    # Values are deterministic test fixtures and contain no XML metacharacters.
    return f'<c r="{col_name(column)}{row}" s="{style}" t="inlineStr"><is><t>{value}</t></is></c>'


def styles_xml(font_count: int) -> bytes:
    fonts = "".join(
        f'<font><name val="Bench{index}"/><sz val="{10 + index % 8}"/>'
        f'<color rgb="FF{index:06X}"/></font>'
        for index in range(font_count)
    )
    xfs = "".join(
        f'<xf numFmtId="0" fontId="{index}" fillId="0" borderId="0" '
        f'applyFont="1"><alignment horizontal="left" vertical="top"/></xf>'
        for index in range(font_count)
    )
    return (
        f'<styleSheet xmlns="{MAIN}"><fonts count="{font_count}">{fonts}</fonts>'
        '<fills count="2"><fill><patternFill patternType="none"/></fill>'
        '<fill><patternFill patternType="gray125"/></fill></fills>'
        '<borders count="1"><border><left/><right/><top/><bottom/><diagonal/>'
        '</border></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" '
        'fillId="0" borderId="0"/></cellStyleXfs>'
        f'<cellXfs count="{font_count}">{xfs}</cellXfs></styleSheet>'
    ).encode()


def worksheet_xml(rows: int, columns: int, last_value: str, styles: int) -> bytes:
    row_xml = []
    for row in range(1, rows + 1):
        cells = []
        for column in range(1, columns + 1):
            value = "after" if row == rows and column == columns else f"R{row}C{column}"
            if row == rows and column == columns:
                value = last_value
            cells.append(cell_xml(column, row, value, (row * columns + column) % styles))
        row_xml.append(f'<row r="{row}">{"".join(cells)}</row>')
    return (
        f'<worksheet xmlns="{MAIN}"><sheetViews><sheetView workbookViewId="0"/>'
        '</sheetViews><sheetFormatPr defaultRowHeight="15"/><sheetData>'
        f'{"".join(row_xml)}</sheetData></worksheet>'
    ).encode()


def synthetic_book(path: Path, rows: int, columns: int, last_value: str, fonts: int) -> None:
    workbook = (
        f'<workbook xmlns="{MAIN}" xmlns:r="{REL}"><sheets>'
        '<sheet name="Bench" sheetId="1" r:id="rId1"/></sheets></workbook>'
    ).encode()
    rels = (
        f'<Relationships xmlns="{PKGREL}"><Relationship Id="rId1" Type="{REL}/worksheet" '
        'Target="worksheets/sheet1.xml"/><Relationship Id="rId2" '
        f'Type="{REL}/styles" Target="styles.xml"/></Relationships>'
    ).encode()
    content = (
        f'<Types xmlns="{CT}"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
        '<Default Extension="xml" ContentType="application/xml"/>'
        '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
        '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
        '<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>'
        '</Types>'
    ).encode()
    root_rels = (
        f'<Relationships xmlns="{PKGREL}"><Relationship Id="rId1" '
        f'Type="{REL}/officeDocument" Target="xl/workbook.xml"/></Relationships>'
    ).encode()
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as z:
        for name, data in [
            ("[Content_Types].xml", content),
            ("_rels/.rels", root_rels),
            ("xl/workbook.xml", workbook),
            ("xl/_rels/workbook.xml.rels", rels),
            ("xl/worksheets/sheet1.xml", worksheet_xml(rows, columns, last_value, fonts)),
            ("xl/styles.xml", styles_xml(fonts)),
        ]:
            zip_entry(z, name, data)


def base_request(operation: str) -> dict[str, Any]:
    return {"schemaVersion": SCHEMA, "operation": operation}


def source_request(case: dict[str, Any], inputs: Path, output: Path | None = None) -> dict[str, Any]:
    request = copy.deepcopy(case["request"])
    for field in ("source", "before", "after"):
        if field in request:
            request[field] = str((inputs / case["inputDir"] / request[field]).resolve())
    if output is not None:
        request["output"] = str(output)
        request["expectedSha256"] = digest(Path(request["source"]))
    return request


def fixture_cases(name: str, rows: int, columns: int) -> list[dict[str, Any]]:
    end = f"{col_name(columns)}{rows}"
    inspect_start = max(1, rows - max(1, 200 // columns) + 1)
    inspect_end_col = min(columns, 200)
    inspect_range = f"Bench!A{inspect_start}:{col_name(inspect_end_col)}{rows}"
    target_rows = 10000 // columns
    return [
        {"id": f"{name}-inspect", "kind": "inspect", "inputDir": name,
         "request": {**base_request("inspect"), "source": "source.xlsx", "ranges": [inspect_range], "maxCells": 200}},
        {"id": f"{name}-diff", "kind": "diff", "inputDir": name,
         "request": {**base_request("diff"), "before": "source.xlsx", "after": "after.xlsx", "maxCells": 20}},
        {"id": f"{name}-patch", "kind": "patch", "inputDir": name,
         "request": {**base_request("patch"), "source": "source.xlsx", "output": "__BENCH_OUTPUT__",
                     "operations": [{"op": "setStyle", "sheet": "Bench", "range": f"A1:{col_name(columns)}{target_rows}",
                                     "style": {"bold": True, "wrapText": True, "horizontal": "left", "vertical": "top"}}]}},
    ]


def build_manifest(bench: Path, real_source: Path, real_after: Path) -> dict[str, Any]:
    inputs = bench / "inputs"
    inputs.mkdir(parents=True, exist_ok=False)
    real = inputs / "uc"
    real.mkdir()
    shutil.copyfile(real_source, real / "source.xlsx")
    shutil.copyfile(real_after, real / "after.xlsx")
    cases: list[dict[str, Any]] = [
        {"id": "uc-inspect", "kind": "inspect", "inputDir": "uc", "request": {**base_request("inspect"), "source": "source.xlsx", "ranges": ["UseCase!E12:F14"], "maxCells": 200}},
        {"id": "uc-diff", "kind": "diff", "inputDir": "uc", "request": {**base_request("diff"), "before": "source.xlsx", "after": "after.xlsx", "maxCells": 20}},
        {"id": "uc-patch", "kind": "patch", "inputDir": "uc", "request": {**base_request("patch"), "source": "source.xlsx", "output": "__BENCH_OUTPUT__", "operations": [
            {"op": "setText", "sheet": "UseCase", "cell": "E12", "expectedText": "HHT requires request.UUID and:", "value": "HHT requires request.UUID and: [Opsail verification copy]"},
            {"op": "setStyle", "sheet": "UseCase", "range": "F13", "style": {"fontColor": "0000FF", "wrapText": True, "vertical": "top", "horizontal": "left"}},
            {"op": "copyStyle", "sheet": "UseCase", "range": "F14", "fromCell": "F13", "components": ["alignment"]},
            {"op": "rowHeight", "sheet": "UseCase", "row": 14, "height": 32},
            {"op": "columnWidth", "sheet": "Mapping List", "column": "N", "width": 20},
            {"op": "rowVisibility", "sheet": "UseCase", "row": 15, "hidden": False},
            {"op": "setStyle", "sheet": "gwhCtet1Rpspl01Prt(api)", "range": "AT41", "style": {"wrapText": True, "vertical": "top"}},
        ]}},
    ]
    definitions = [("tall", 5000, 4, 2), ("wide", 50, 400, 2), ("styles", 2000, 10, 512)]
    for name, rows, columns, fonts in definitions:
        target = inputs / name
        target.mkdir()
        synthetic_book(target / "source.xlsx", rows, columns, "before", fonts)
        synthetic_book(target / "after.xlsx", rows, columns, "after", fonts)
        cases.extend(fixture_cases(name, rows, columns))
    manifest = {
        "schemaVersion": 1,
        "createdBy": "opsail-xlsx benchmark.py",
        "realInputs": {"source": str(real_source), "after": str(real_after)},
        "fixtures": {name: {file.name: digest(file) for file in sorted((inputs / name).glob("*.xlsx"))} for name in ("uc", "tall", "wide", "styles")},
        "cases": cases,
    }
    json_write(bench / "manifest.json", manifest)
    return manifest


def verify_manifest(bench: Path, manifest: dict[str, Any]) -> None:
    if manifest.get("schemaVersion") != 1:
        die("unsupported manifest schemaVersion")
    for name, expected in manifest.get("fixtures", {}).items():
        for filename, expected_hash in expected.items():
            path = bench / "inputs" / name / filename
            if not path.is_file() or digest(path) != expected_hash:
                die(f"fixture changed or missing: {path}")


def prepare(args: argparse.Namespace) -> None:
    bench = absolute(args.dir)
    source = absolute(args.real_source)
    after = absolute(args.real_after)
    if not source.is_file() or not after.is_file():
        die("--real-source and --real-after must be readable files")
    manifest_file = bench / "manifest.json"
    if manifest_file.exists():
        manifest = json.loads(manifest_file.read_text(encoding="utf-8"))
        verify_manifest(bench, manifest)
        print(json.dumps({"status": "reused", "manifest": str(manifest_file), "cases": len(manifest["cases"])}, ensure_ascii=False))
        return
    if bench.exists() and any(bench.iterdir()):
        die(f"new benchmark directory must be empty: {bench}")
    bench.mkdir(parents=True, exist_ok=True)
    manifest = build_manifest(bench, source, after)
    print(json.dumps({"status": "prepared", "manifest": str(manifest_file), "cases": len(manifest["cases"])}, ensure_ascii=False))


def run_one(binary: Path, request: dict[str, Any], timeout: float) -> dict[str, Any]:
    started = time.time()
    tic = time.perf_counter_ns()
    try:
        completed = subprocess.run([str(binary), "xlsx", "--machine"], input=json.dumps(request), text=True,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout, check=False)
        elapsed = (time.perf_counter_ns() - tic) / 1_000_000
        try:
            response = json.loads(completed.stdout) if completed.stdout.strip() else None
        except json.JSONDecodeError as error:
            response = None
            parse_error = str(error)
        else:
            parse_error = None
        return {"startedAt": started, "elapsedMs": elapsed, "status": "ok" if completed.returncode == 0 and response is not None else "error",
                "exitCode": completed.returncode, "stdout": completed.stdout, "stderr": completed.stderr,
                "response": response, "parseError": parse_error}
    except subprocess.TimeoutExpired as error:
        elapsed = (time.perf_counter_ns() - tic) / 1_000_000
        stdout = error.stdout.decode() if isinstance(error.stdout, bytes) else (error.stdout or "")
        stderr = error.stderr.decode() if isinstance(error.stderr, bytes) else (error.stderr or "")
        return {"startedAt": started, "elapsedMs": elapsed, "status": "timeout", "censored": True,
                "exitCode": None, "stdout": stdout, "stderr": stderr, "response": None, "parseError": None}


def selected(cases: list[dict[str, Any]], requested: list[str] | None) -> list[dict[str, Any]]:
    if not requested:
        return cases
    wanted = set(requested)
    unknown = wanted - {case["id"] for case in cases}
    if unknown:
        die(f"unknown --case: {', '.join(sorted(unknown))}")
    return [case for case in cases if case["id"] in wanted]


def run(args: argparse.Namespace) -> None:
    bench = absolute(args.dir)
    binary = absolute(args.binary)
    if not binary.is_file() or not os.access(binary, os.X_OK):
        die(f"--binary must be an executable file: {binary}")
    if not LABEL.fullmatch(args.label):
        die("label may contain only letters, digits, dot, underscore and hyphen")
    manifest = json.loads((bench / "manifest.json").read_text(encoding="utf-8"))
    verify_manifest(bench, manifest)
    if args.repetitions < 1:
        die("--repetitions must be at least one")
    result_dir = bench / "results" / args.label
    if result_dir.exists():
        die(f"result label already exists: {result_dir}")
    result_dir.mkdir(parents=True)
    chosen = selected(manifest["cases"], args.case)
    report: dict[str, Any] = {"schemaVersion": 1, "label": args.label, "binary": str(binary), "binarySha256": digest(binary),
                              "platform": {"system": platform.system(), "release": platform.release(), "machine": platform.machine(), "python": sys.version},
                              "timeoutSeconds": args.timeout, "repetitions": args.repetitions,
                              "manifestSha256": digest(bench / "manifest.json"),
                              "expectedCaseIds": [case["id"] for case in chosen], "cases": []}
    json_write(result_dir / "run.json", report)
    for case in chosen:
        print(f"benchmark: {args.label} {case['id']} warmup", flush=True)
        case_dir = result_dir / case["id"]
        case_dir.mkdir()
        request = source_request(case, bench / "inputs", case_dir / "warmup.xlsx" if case["kind"] == "patch" else None)
        warmup = run_one(binary, request, args.timeout)
        if case["kind"] == "patch":
            warmup["candidate"] = str(case_dir / "warmup.xlsx")
        (case_dir / "warmup.stdout.json").write_text(warmup["stdout"], encoding="utf-8")
        samples = []
        for index in range(1, args.repetitions + 1):
            print(f"benchmark: {args.label} {case['id']} sample {index}/{args.repetitions}", flush=True)
            output = case_dir / f"candidate-{index}.xlsx" if case["kind"] == "patch" else None
            sample = run_one(binary, source_request(case, bench / "inputs", output), args.timeout)
            sample["index"] = index
            if output is not None:
                sample["candidate"] = str(output)
            (case_dir / f"sample-{index}.stdout.json").write_text(sample["stdout"], encoding="utf-8")
            samples.append(sample)
            case_record = {"id": case["id"], "kind": case["kind"], "warmup": warmup, "samples": samples}
            report["cases"] = [r for r in report["cases"] if r["id"] != case["id"]] + [case_record]
            json_write(result_dir / "run.json", report)
    print(json.dumps({"status": "complete", "run": str(result_dir / "run.json"), "cases": len(chosen)}, ensure_ascii=False))


def response_for_compare(sample: dict[str, Any], patch: bool) -> Any:
    value = copy.deepcopy(sample.get("response"))
    if patch and isinstance(value, dict):
        value.pop("output", None)
    return value


def zip_payloads(path: Path) -> dict[str, str]:
    result: dict[str, str] = {}
    with zipfile.ZipFile(path) as archive:
        names = archive.namelist()
        if len(names) != len(set(names)):
            raise ValueError("duplicate ZIP entry")
        for name in names:
            result[name] = hashlib.sha256(archive.read(name)).hexdigest()
    return result


def comparison_samples(left: dict[str, Any], right: dict[str, Any], patch: bool) -> dict[str, Any]:
    comparison = {"bothOk": left.get("status") == right.get("status") == "ok",
                  "sameStatus": left.get("status") == right.get("status"),
                  "sameResponse": response_for_compare(left, patch) == response_for_compare(right, patch)}
    if patch and left.get("status") == right.get("status") == "ok":
        try:
            comparison["sameCandidatePayloads"] = zip_payloads(Path(left["candidate"])) == zip_payloads(Path(right["candidate"]))
        except (KeyError, OSError, zipfile.BadZipFile, ValueError) as error:
            comparison["sameCandidatePayloads"] = False
            comparison["candidateCompareError"] = str(error)
    return comparison


def candidate_for(record: dict[str, Any], case_dir: Path, warmup: bool) -> dict[str, Any]:
    """Resolve a candidate without changing a legacy result record.

    Earlier runs did not record the warm-up candidate path although their stable
    output location was already part of the public harness layout.  The compare
    report says when this fallback was used rather than manufacturing a field in
    the saved run evidence.
    """
    if record.get("candidate"):
        return record
    fallback = case_dir / ("warmup.xlsx" if warmup else f"candidate-{record.get('index')}.xlsx")
    result = dict(record)
    result["candidate"] = str(fallback)
    result["candidatePathSource"] = "derivedStableLayout"
    return result


def timings(case: dict[str, Any]) -> dict[str, Any]:
    values = [sample["elapsedMs"] for sample in case["samples"] if sample["status"] == "ok"]
    if len(values) != len(case["samples"]):
        return {"okSamples": len(values), "totalSamples": len(case["samples"]), "medianMs": None, "minMs": None, "maxMs": None}
    return {"okSamples": len(values), "totalSamples": len(case["samples"]), "medianMs": median(values), "minMs": min(values), "maxMs": max(values)}


def validate_run(report: dict[str, Any], label: str, manifest_sha: str, manifest_ids: set[str]) -> dict[str, Any]:
    """Return validation facts; never amend a completed run record in place."""
    cases = report.get("cases")
    repetitions = report.get("repetitions")
    observed = [case.get("id") for case in cases] if isinstance(cases, list) else []
    expected = report.get("expectedCaseIds")
    legacy = expected is None and report.get("manifestSha256") is None
    if expected is None:
        expected = observed
    valid_ids = (isinstance(expected, list) and len(expected) > 0 and len(expected) == len(set(expected))
                 and set(expected) == set(observed) and set(expected).issubset(manifest_ids))
    complete = isinstance(repetitions, int) and repetitions >= 1 and valid_ids
    if complete:
        for case in cases:
            complete = isinstance(case.get("samples"), list) and len(case["samples"]) == repetitions and "warmup" in case
            if not complete:
                break
    attested = report.get("manifestSha256") == manifest_sha
    return {"label": label, "complete": complete, "expectedCaseIds": expected,
            "manifestSha256": report.get("manifestSha256"), "manifestAttested": attested,
            "legacyManifestEvidence": legacy}


def compare(args: argparse.Namespace) -> None:
    bench = absolute(args.dir)
    manifest_path = bench / "manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    verify_manifest(bench, manifest)
    manifest_sha = digest(manifest_path)
    manifest_ids = {case["id"] for case in manifest["cases"]}
    first = json.loads((bench / "results" / args.first / "run.json").read_text(encoding="utf-8"))
    second = json.loads((bench / "results" / args.second / "run.json").read_text(encoding="utf-8"))
    first_validation = validate_run(first, args.first, manifest_sha, manifest_ids)
    second_validation = validate_run(second, args.second, manifest_sha, manifest_ids)
    one = {case["id"]: case for case in first["cases"]}
    two = {case["id"]: case for case in second["cases"]}
    report: dict[str, Any] = {"schemaVersion": 1, "first": args.first, "second": args.second,
                              "manifestSha256": manifest_sha, "runValidation": [first_validation, second_validation],
                              "cases": [], "speedup": None}
    all_clean = first_validation["complete"] and second_validation["complete"]
    ratios = []
    for identifier in sorted(set(one) | set(two)):
        left, right = one.get(identifier), two.get(identifier)
        if left is None or right is None:
            report["cases"].append({"id": identifier, "missing": args.first if left is None else args.second})
            all_clean = False
            continue
        patch = left["kind"] == "patch"
        paired = [comparison_samples(
            candidate_for(a, bench / "results" / args.first / identifier, False) if patch else a,
            candidate_for(b, bench / "results" / args.second / identifier, False) if patch else b,
            patch) for a, b in zip(left["samples"], right["samples"])]
        warmup = comparison_samples(
            candidate_for(left["warmup"], bench / "results" / args.first / identifier, True) if patch else left["warmup"],
            candidate_for(right["warmup"], bench / "results" / args.second / identifier, True) if patch else right["warmup"],
            patch)
        equal = (len(left["samples"]) == len(right["samples"]) and warmup["bothOk"] and warmup["sameResponse"]
                 and warmup.get("sameCandidatePayloads", True)
                 and all(item["bothOk"] and item["sameResponse"] and item.get("sameCandidatePayloads", True) for item in paired))
        first_timing, second_timing = timings(left), timings(right)
        entry = {"id": identifier, "kind": left["kind"], "equivalent": equal, "warmup": warmup, "samples": paired,
                 args.first: first_timing, args.second: second_timing}
        if first_timing["medianMs"] is not None and second_timing["medianMs"] is not None and first_timing["medianMs"] > 0 and equal:
            entry["medianRatio"] = second_timing["medianMs"] / first_timing["medianMs"]
            ratios.append(entry["medianRatio"])
        else:
            entry["medianRatio"] = None
        report["cases"].append(entry)
        all_clean = all_clean and equal
    report["equivalent"] = all_clean
    # A speedup claim is omitted whenever a failure, timeout, mismatch, or missing
    # case makes the paired comparison unreliable.
    if all_clean and ratios:
        report["speedup"] = {"medianOfCaseRatios": median(ratios), "interpretation": "second / first; below 1 is faster"}
    output = bench / f"comparison-{args.first}-vs-{args.second}.json"
    json_write(output, report)
    print(json.dumps({"status": "complete", "comparison": str(output), "equivalent": all_clean, "speedup": report["speedup"]}, ensure_ascii=False))


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__)
    commands = p.add_subparsers(dest="command", required=True)
    prepare_parser = commands.add_parser("prepare", help="create or hash-verify fixed benchmark inputs")
    prepare_parser.add_argument("--dir", required=True)
    prepare_parser.add_argument("--real-source", default=str(REAL_SOURCE))
    prepare_parser.add_argument("--real-after", default=str(REAL_AFTER))
    prepare_parser.set_defaults(func=prepare)
    run_parser = commands.add_parser("run", help="measure one binary with fresh processes")
    run_parser.add_argument("--dir", required=True)
    run_parser.add_argument("--binary", required=True)
    run_parser.add_argument("--label", required=True)
    run_parser.add_argument("--repetitions", type=int, default=3)
    run_parser.add_argument("--timeout", type=float, default=45)
    run_parser.add_argument("--case", action="append")
    run_parser.set_defaults(func=run)
    compare_parser = commands.add_parser("compare", help="compare two completed labels")
    compare_parser.add_argument("--dir", required=True)
    compare_parser.add_argument("first")
    compare_parser.add_argument("second")
    compare_parser.set_defaults(func=compare)
    return p


if __name__ == "__main__":
    arguments = parser().parse_args()
    arguments.func(arguments)
