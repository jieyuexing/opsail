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


if __name__ == "__main__":
    unittest.main()
