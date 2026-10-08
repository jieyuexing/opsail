import contextlib
import io
import json
import os
from pathlib import Path
import sys
import subprocess
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str((Path(__file__).resolve().parent / '../../../src/chat').resolve()))
from cli import execute, validate_request
from chat.config import ChatError, active_path, load_bindings


class EntryTests(unittest.TestCase):
    def test_unknown_parameters_and_unbounded_request_rejected(self):
        for args in [
            {"provider": "wechat", "mode": "snapshot", "limit": True},
            {"provider": "teams", "mode": "snapshot", "limit": 201},
            {"provider": "feishu", "mode": "snapshot", "conversationName": "one", "cookie": "secret"},
        ]:
            with self.assertRaises(ChatError):
                validate_request({"operation": "read", "args": args})

    def test_status_without_bindings_does_not_import_providers(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {"OPSAIL_CHAT_BINDING_FILE": directory + "/absent.json"}):
            before = set(sys.modules)
            result = execute({"operation": "status", "args": {}})
            self.assertEqual(len(result["data"]["providers"]), 3)
            self.assertFalse({"chat.snapshots", "chat.live", "playwright.sync_api"} & (set(sys.modules) - before))

    def test_json_snapshot_does_not_load_wechat_or_browser_dependencies(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            capture = root / "records" / "fixture" / "captures" / "one.json"
            capture.parent.mkdir(parents=True)
            capture.write_text(json.dumps({"schema_version": 1, "kind": "chat_export_capture", "provider": "feishu", "capture_date": "2026-09-07", "chat": {"id": "fixture", "name": "Fixture"}, "messages": []}))
            binding = root / "bindings.json"
            binding.write_text(json.dumps({"schemaVersion": 1, "providers": {"feishu": {"snapshot_root": str(root / "records")}}}))
            binding.chmod(0o600)
            plugin = str((Path(__file__).resolve().parent / '../../../src').resolve())
            script = f"import sys; sys.path.insert(0, {plugin!r}); from chat.cli import execute; execute({{'operation':'read','args':{{'provider':'feishu','mode':'snapshot','conversationId':'fixture'}}}}); assert not {{'sqlite3','chat.live','playwright.sync_api'}} & set(sys.modules)"
            result = subprocess.run([sys.executable, "-c", script], capture_output=True, env={**os.environ, "OPSAIL_CHAT_BINDING_FILE": str(binding)}, timeout=10)
            self.assertEqual(result.returncode, 0)

    def test_private_mode_and_symlink_retirement_guard(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binding = root / "binding.json"
            binding.write_text(json.dumps({"schemaVersion": 1, "providers": {}}))
            binding.chmod(0o644)
            with patch.dict(os.environ, {"OPSAIL_CHAT_BINDING_FILE": str(binding)}):
                with self.assertRaises(ChatError): load_bindings()
                binding.chmod(0o600)
                self.assertEqual(load_bindings(), {})
            retired = root / ".Trash" / "source"
            retired.mkdir(parents=True)
            alias = root / "current"
            alias.symlink_to(retired)
            with self.assertRaises(ChatError): active_path(str(alias))

    def test_explicit_binding_does_not_inherit_installed_private_accounts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binding = root / "binding.json"
            binding.write_text(json.dumps({"schemaVersion": 1, "providers": {}}))
            binding.chmod(0o600)
            installed = root / "config.json"
            installed.write_text("not the selected binding")
            installed.chmod(0o600)
            with patch.dict(os.environ, {"OPSAIL_CHAT_BINDING_FILE": str(binding)}), patch("chat.config.DEFAULT_EXTENSION_CONFIG_FILE", installed):
                self.assertEqual(load_bindings(), {})


if __name__ == "__main__":
    unittest.main()
