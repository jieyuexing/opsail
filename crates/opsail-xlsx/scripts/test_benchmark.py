#!/usr/bin/env python3
"""Sensitivity checks for benchmark comparison evidence, not performance tests."""

import contextlib
import hashlib
import importlib.util
import io
import json
import tempfile
import unittest
import zipfile
from pathlib import Path
from types import SimpleNamespace


SCRIPT = Path(__file__).with_name("benchmark.py")
SPEC = importlib.util.spec_from_file_location("opsail_xlsx_benchmark", SCRIPT)
BENCH = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(BENCH)


def write_zip(path: Path, payload: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr("xl/worksheets/sheet1.xml", payload)


class CompareSensitivity(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / "inputs").mkdir()
        manifest = {"schemaVersion": 1, "fixtures": {}, "cases": [{"id": "patch", "kind": "patch"}]}
        (self.root / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
        self.manifest_sha = hashlib.sha256((self.root / "manifest.json").read_bytes()).hexdigest()

    def tearDown(self) -> None:
        self.temp.cleanup()

    def write_run(self, label: str, response_value: str = "same", status: str = "ok", payload: bytes = b"same") -> None:
        case_dir = self.root / "results" / label / "patch"
        samples = []
        for index in range(1, 4):
            candidate = case_dir / f"candidate-{index}.xlsx"
            write_zip(candidate, payload)
            samples.append({"index": index, "status": status, "elapsedMs": 10.0, "response": {"value": response_value, "output": str(candidate)}, "candidate": str(candidate)})
        warmup = case_dir / "warmup.xlsx"
        write_zip(warmup, payload)
        report = {"schemaVersion": 1, "label": label, "repetitions": 3, "manifestSha256": self.manifest_sha,
                  "expectedCaseIds": ["patch"], "cases": [{"id": "patch", "kind": "patch",
                  "warmup": {"status": status, "elapsedMs": 10.0, "response": {"value": response_value, "output": str(warmup)}, "candidate": str(warmup)},
                  "samples": samples}]}
        (case_dir.parent / "run.json").write_text(json.dumps(report), encoding="utf-8")

    def compare(self) -> dict:
        with contextlib.redirect_stdout(io.StringIO()):
            BENCH.compare(SimpleNamespace(dir=str(self.root), first="one", second="two"))
        return json.loads((self.root / "comparison-one-vs-two.json").read_text(encoding="utf-8"))

    def test_response_difference_blocks_speedup(self) -> None:
        self.write_run("one", response_value="old")
        self.write_run("two", response_value="new")
        result = self.compare()
        self.assertFalse(result["equivalent"])
        self.assertIsNone(result["speedup"])

    def test_timeout_even_on_both_sides_blocks_speedup(self) -> None:
        self.write_run("one", status="timeout")
        self.write_run("two", status="timeout")
        result = self.compare()
        self.assertFalse(result["equivalent"])
        self.assertIsNone(result["speedup"])

    def test_candidate_payload_difference_blocks_speedup(self) -> None:
        self.write_run("one", payload=b"first")
        self.write_run("two", payload=b"second")
        result = self.compare()
        self.assertFalse(result["equivalent"])
        self.assertIsNone(result["speedup"])


class PrepareBoundary(unittest.TestCase):
    """The public suite is synthetic by default; external cases are explicit."""

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)

    def tearDown(self) -> None:
        self.temp.cleanup()

    def extra(self, **changes) -> dict:
        source = self.root / "external.xlsx"
        if not source.exists():
            BENCH.synthetic_book(source, 2, 2, "before", 1)
        definition = {"name": "external", "files": {"source.xlsx": str(source)},
                      "cases": [{"id": "external-inspect", "kind": "inspect", "inputDir": "external",
                                 "request": {"schemaVersion": 1, "operation": "inspect", "source": "source.xlsx",
                                             "ranges": ["Bench!A1:B2"], "maxCells": 4}}]}
        definition.update(changes)
        return definition

    def test_source_has_no_workspace_binding(self) -> None:
        text = SCRIPT.read_text(encoding="utf-8")
        for needle in ("parents[", ".local/", "UseCase", "Mapping List", "realInputs", "--real-source"):
            self.assertNotIn(needle, text)

    def test_prepare_has_no_default_input_files(self) -> None:
        args = vars(BENCH.parser().parse_args(["prepare", "--dir", "/absent"]))
        self.assertEqual({key: value for key, value in args.items() if key not in {"command", "func"}},
                         {"dir": "/absent", "cases": None})

    def test_default_manifest_is_synthetic_only(self) -> None:
        manifest = BENCH.build_manifest(self.root / "bench")
        self.assertEqual(list(manifest["fixtures"]), ["tall", "wide", "styles"])
        self.assertEqual(len(manifest["cases"]), 9)
        self.assertNotIn("externalInputs", manifest)

    def test_external_definition_is_copied_and_listed_first(self) -> None:
        bench = self.root / "bench"
        manifest = BENCH.build_manifest(bench, [self.extra()])
        self.assertEqual(list(manifest["fixtures"]), ["external", "tall", "wide", "styles"])
        self.assertEqual(manifest["cases"][0]["id"], "external-inspect")
        self.assertTrue((bench / "inputs" / "external" / "source.xlsx").is_file())
        BENCH.verify_manifest(bench, manifest)

    def test_bad_external_definitions_write_nothing(self) -> None:
        bad_case = self.extra()["cases"][0]
        bad = [
            "not-a-list",
            [self.extra(name="tall")],
            [self.extra(name="../escape")],
            [self.extra(files={"../source.xlsx": str(self.root / "external.xlsx")})],
            [self.extra(files={"source.xlsx": "relative.xlsx"})],
            [self.extra(files={"source.xlsx": str(self.root / "missing.xlsx")})],
            [self.extra(cases=[])],
            [self.extra(cases=[{**bad_case, "id": "tall-inspect"}])],
            [self.extra(cases=[{**bad_case, "inputDir": "tall"}])],
            [self.extra(cases=[{**bad_case, "kind": "write"}])],
            [self.extra(cases=[{**bad_case, "request": {**bad_case["request"], "source": "/abs/other.xlsx"}}])],
            [self.extra(cases=[bad_case, bad_case])],
        ]
        for extras in bad:
            bench = self.root / "bench"
            with self.subTest(extras=extras), self.assertRaises(SystemExit):
                BENCH.prepare_dir(bench, extras)
            self.assertFalse(bench.exists())

    def test_legacy_manifest_with_real_inputs_is_still_verified(self) -> None:
        bench = self.root / "legacy"
        write_zip(bench / "inputs" / "uc" / "source.xlsx", b"legacy")
        fixture = bench / "inputs" / "uc" / "source.xlsx"
        manifest = {"schemaVersion": 1, "realInputs": {"source": "/old/source.xlsx", "after": "/old/after.xlsx"},
                    "fixtures": {"uc": {"source.xlsx": hashlib.sha256(fixture.read_bytes()).hexdigest()}},
                    "cases": [{"id": "uc-inspect", "kind": "inspect"}]}
        (bench / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
        self.assertEqual(BENCH.prepare_dir(bench)["status"], "reused")
        fixture.write_bytes(b"changed")
        with self.assertRaises(SystemExit):
            BENCH.prepare_dir(bench)


if __name__ == "__main__":
    unittest.main()
