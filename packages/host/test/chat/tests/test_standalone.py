"""Standalone extraction checks: synthetic sources, no browser or installed account."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

PACKAGE = Path(__file__).resolve().parents[3]


class StandaloneTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="standalone-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.package = self.root / "independent checkout/packages/host"
        for name in ("chat", "browser"):
            shutil.copytree(PACKAGE / "src" / name, self.package / "src" / name,
                            ignore=shutil.ignore_patterns("__pycache__"))
        for name in ("layout.js", "paths.py"):
            shutil.copyfile(PACKAGE / "src" / name, self.package / "src" / name)
        (self.package / "bin").mkdir()
        for name in ("chat", "opsail-chrome"):
            shutil.copy2(PACKAGE / "bin" / name, self.package / "bin" / name)
        (self.package / "package.json").write_text('{"type":"module"}')
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith("OPSAIL_") and key not in ("PYTHONPATH", "XDG_DATA_HOME")}
        self.env.update(HOME=str(self.root / "home"), PYTHONDONTWRITEBYTECODE="1",
                        JIEYUEXING_UNIVERSE_ROOT=str(self.root / "nonexistent-workspace"))

    def run_command(self, argv, *, input=None):
        result = subprocess.run(argv, cwd=self.package, env=self.env, input=input,
                                capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def python(self, code):
        return self.run_command([sys.executable, "-B", "-c",
                                 "import sys; sys.path.insert(0, 'src'); " + code])

    def test_default_paths_ignore_workspace_markers_and_private_modules(self):
        observed = json.loads(self.run_command(["node", "--input-type=module", "-e",
            "import {resolveOpsailLayout} from './src/layout.js'; console.log(JSON.stringify(resolveOpsailLayout()))"]))
        self.assertEqual(observed["root"], str(self.package))
        self.assertEqual(observed["sourceDir"], str(self.package.parent.parent))
        expected = str(self.root / "home/.local/share/opsail-host/retained-chat")
        self.assertEqual(observed["retainedChatDataDir"], expected)
        self.assertEqual(self.python("from paths import CHAT_DATA_ROOT; print(CHAT_DATA_ROOT)").strip(), expected)
        status = json.loads(self.run_command([str(self.package / "bin/chat"), "status"]))
        self.assertEqual(status["exitCode"], 0)
        self.assertTrue(all(not mode["configured"] for row in status["data"]["providers"]
                            for mode in row["modes"].values()))

    def test_explicit_paths_and_xdg_agree_between_python_and_node(self):
        self.env["XDG_DATA_HOME"] = str(self.root / "xdg data")
        expected = str(self.root / "xdg data/opsail-host/retained-chat")
        self.assertEqual(self.python("from paths import CHAT_DATA_ROOT; print(CHAT_DATA_ROOT)").strip(), expected)
        self.env.update(OPSAIL_RUNTIME_PACKAGE_DIR=str(self.root / "runtime package"),
                        OPSAIL_SOURCE_DIR=str(self.root / "native source"),
                        OPSAIL_PIN_PATH=str(self.root / "explicit pin.json"),
                        OPSAIL_CHAT_DATA_ROOT=str(self.root / "configured data"))
        node = json.loads(self.run_command(["node", "--input-type=module", "-e",
            "import {OPSAIL_PACKAGE_DIR,resolveOpsailLayout} from './src/layout.js'; console.log(JSON.stringify({packageDir:OPSAIL_PACKAGE_DIR,...resolveOpsailLayout()}))"]))
        python = json.loads(self.python("import json; from paths import RUNTIME_PACKAGE_DIR,SOURCE_DIR,PIN_PATH,CHAT_DATA_ROOT; print(json.dumps(list(map(str,[RUNTIME_PACKAGE_DIR,SOURCE_DIR,PIN_PATH,CHAT_DATA_ROOT]))))"))
        self.assertEqual(python, [node[key] for key in ("root", "sourceDir", "pinPath", "retainedChatDataDir")])
        self.assertEqual(node["packageDir"], node["root"])

    def test_node_passes_data_root_to_actual_python_reader(self):
        data = self.root / "data with spaces"
        capture = data / "records/fixture/captures/one.json"
        capture.parent.mkdir(parents=True)
        capture.write_text(json.dumps({"schema_version": 1, "kind": "chat_export_capture", "provider": "feishu",
            "capture_date": "2026-01-01", "chat": {"id": "fixture", "name": "Fixture"}, "messages": []}))
        binding = data / "bindings.json"
        binding.write_text(json.dumps({"schemaVersion": 1, "providers": {"feishu": {"snapshot_root": str(data / "records")}}}))
        binding.chmod(0o600)
        self.env["OPSAIL_CHAT_DATA_ROOT"] = str(data)
        self.env["OPSAIL_CHAT_PYTHON"] = sys.executable
        result = json.loads(self.run_command([str(self.package / "bin/chat"), "read", "--provider", "feishu",
            "--mode", "snapshot", "--conversation-id", "fixture"]))
        self.assertEqual(result["exitCode"], 0)
        self.assertEqual(result["data"]["content"]["capture"]["extensions"]["source"]["conversation_id"], "fixture")

    def test_self_identity_and_ticket_scan_require_explicit_configuration(self):
        self.assertEqual(self.python("from chat import daily; from unittest.mock import patch; "
            "guard=patch.object(daily.Path,'rglob',side_effect=AssertionError('unexpected scan')); guard.start(); "
            "assert daily._wiki_tickets()=={}; assert daily.WECHAT_SELF_REF is None; print('unconfigured')").strip(), "unconfigured")
        tickets = self.root / "ticket records"
        (tickets / "DEMO-42").mkdir(parents=True)
        (tickets / "OTHER-1").mkdir()
        self.env.update(OPSAIL_CHAT_WECHAT_SELF_REF="fixture-self", OPSAIL_CHAT_TICKET_PATTERN=r"DEMO-\d+",
                        OPSAIL_CHAT_TICKET_PATHS=json.dumps({str(tickets): "ticket-links"}))
        self.assertEqual(json.loads(self.python("import json; from chat import daily; assert daily.WECHAT_SELF_REF=='fixture-self'; print(json.dumps(daily._wiki_tickets()))")),
                         {"DEMO-42": "ticket-links/DEMO-42"})

    def test_digest_refuses_changes_to_executed_core_and_route_files(self):
        for relative in ("src/chat/config.py", "src/browser/ipc.py", "src/paths.py", "src/layout.js", "bin/chat"):
            with self.subTest(path=relative):
                path = self.package / relative
                original = path.read_bytes()
                try:
                    result = self.run_command(["node", "--input-type=module", "-e", r"""
import {appendFileSync} from 'node:fs';
import {runChat} from './src/chat/tools.js';
const before = await runChat('status', {});
if (before.exitCode !== 0 || before.implementation.adapterSha256.length !== 64) throw Error('initial reader failed');
appendFileSync(process.argv[1], '\n');
const after = await runChat('status', {});
if (after.error?.code !== 'implementation-changed') throw Error('drift accepted');
console.log(after.error.code);
""", relative])
                    self.assertEqual(result.strip(), "implementation-changed")
                finally:
                    path.write_bytes(original)


if __name__ == "__main__":
    unittest.main()
