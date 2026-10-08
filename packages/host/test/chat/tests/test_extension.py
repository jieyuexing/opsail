"""Extension transport tests use a local Unix socket and no browser/profile."""
from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str((Path(__file__).resolve().parent / '../../../src').resolve()))
from chat import extension, live  # noqa: E402
from chat.config import ChatError, validate_binding  # noqa: E402


def binding() -> dict:
    return {"browser": {"transport": "chrome-extension", "target_url": "https://feishu.cn/next/messenger", "allowed_origins": ["https://feishu.cn"], "profile_id": "profile-1", "account_hash": "a" * 64, "tenant_hash": "b" * 64, "build_id": "build-1"}}


class ExtensionSocketTest(unittest.TestCase):
    def setUp(self):
        # The socket is synthetic; its address must not depend on an installed account.
        self.enterContext(patch("chat.extension.DEFAULT_DATA_ROOT", Path("fixture-data")))

    class FakeSocket:
        def __init__(self, response): self.response, self.sent = response, b""
        def __enter__(self): return self
        def __exit__(self, *args): return False
        def settimeout(self, value): pass
        def connect(self, path): self.path = path
        def sendall(self, value): self.sent += value
        def recv(self, size):
            value, self.response = self.response, b""
            return value

    def test_request_uses_fixed_protocol_and_returns_only_data(self):
        fake = self.FakeSocket(b"")
        def send(value):
            fake.sent += value
            packet = json.loads(value)
            fake.response = (json.dumps({"type": "response", "protocolVersion": 1, "requestId": packet["requestId"], "ok": True, "data": {"entries": []}}) + "\n").encode()
        fake.sendall = send
        with patch("chat.extension._safe_socket"), patch("chat.extension.socket.socket", return_value=fake):
            self.assertEqual(extension.request({**binding(), "_provider": "feishu"}, "catalog", {"limit": 1}), {"entries": []})
        packet = json.loads(fake.sent)
        self.assertEqual(set(packet), {"protocolVersion", "requestId", "operation", "provider", "args"})
        self.assertEqual(packet["operation"], "catalog")
        self.assertEqual(packet["provider"], "feishu")

    def test_rejects_extra_native_frame_and_arbitrary_arguments(self):
        with self.assertRaises(ChatError):
            extension.request({**binding(), "_provider": "feishu"}, "read", {"script": "alert(1)"})
        fake = self.FakeSocket(b"")
        def send(value):
            packet = json.loads(value)
            fake.response = (json.dumps({"type": "response", "protocolVersion": 1, "requestId": packet["requestId"], "ok": True, "data": {}}) + "\nextra\n").encode()
        fake.sendall = send
        with patch("chat.extension._safe_socket"), patch("chat.extension.socket.socket", return_value=fake):
            with self.assertRaises(ChatError) as raised:
                extension.request({**binding(), "_provider": "feishu"}, "status", {})
            self.assertEqual(raised.exception.code, "extension-invalid-response")

    def test_extension_read_rechecks_identity_and_normalizes_capture(self):
        raw = {"conversationId": "oc_1", "conversationName": "Work", "pageUrl": "https://feishu.cn/next/messenger", "messages": [{"id": "m1", "sender": "Ada", "time": "2026-09-15T08:00:00Z", "text": "hello", "is_self": False, "media": []}]}
        with patch("chat.extension.request", return_value=raw):
            result = live.read("feishu", binding(), {"mode": "live-dom", "conversationId": "oc_1", "limit": 1})
        self.assertEqual(result["content"]["conversation_id"], "oc_1")
        self.assertEqual(result["content"]["capture"]["extensions"]["source_mode"], "chrome-extension")
        raw["conversationId"] = "other"
        with patch("chat.extension.request", return_value=raw):
            with self.assertRaises(ChatError):
                live.read("feishu", binding(), {"mode": "live-dom", "conversationId": "oc_1", "limit": 1})

    def test_feishu_time_evidence_and_sender_identity_survive_normalization(self):
        messages = [
            {"id": "m1", "sender": "Ada", "sender_ref": "other", "time": "2026-09-24T09:55:00", "raw_time": "09:55", "date_status": "known", "time_source": "date-separator", "time_zone": "unknown", "text": "hello", "is_self": False, "media": []},
            {"id": "m2", "sender": "Operator", "sender_ref": "self", "time": "2026-09-24T10:16:00Z", "raw_time": "10:16", "date_status": "known", "time_source": "message-metadata", "time_zone": "explicit-offset", "text": "reply", "is_self": True, "media": []},
            {"id": "m3", "sender": "", "sender_ref": None, "time": "10:17", "raw_time": "10:17", "date_status": "unknown", "time_source": "raw", "time_zone": "unknown", "text": "unknown", "is_self": None, "media": []},
        ]
        packet = {"conversationId": "oc_1", "conversationName": "Work", "pageUrl": "https://feishu.cn/next/messenger", "messages": messages}
        with patch("chat.extension.request", return_value=packet):
            result = live.read("feishu", binding(), {"mode": "live-dom", "conversationId": "oc_1", "limit": 3})
        self.assertEqual(result["content"]["messages"], messages)
        capture = result["content"]["capture"]["messages"]
        for raw, normalized in zip(messages, capture):
            self.assertEqual(normalized["sent_at"], raw["time"])
            self.assertEqual(normalized["sender"], {"display_name": raw["sender"], "ref": raw["sender_ref"], "is_self": raw["is_self"]})
            for key in ("raw_time", "date_status", "time_source", "time_zone"):
                self.assertEqual(normalized["extensions"][key], raw[key])

    def test_extension_rejects_malformed_date_evidence_and_sender_types(self):
        base = {"id": "m1", "sender": "Ada", "sender_ref": "other", "time": "2026-09-24T09:55:00", "raw_time": "09:55", "date_status": "known", "time_source": "date-separator", "time_zone": "unknown", "text": "hello", "is_self": False, "media": []}
        for change in ({"time": "09:55"}, {"time": "2026-02-30T09:55:00"}, {"time_zone": "explicit-offset"}, {"date_status": "guessed"}, {"date_status": "unknown"}, {"time_source": "private-data"}, {"sender_ref": {}}, {"is_self": "false"}, {"raw_time": 955}):
            with self.subTest(change=change):
                packet = {"conversationId": "oc_1", "conversationName": "Work", "pageUrl": "https://feishu.cn/next/messenger", "messages": [{**base, **change}]}
                with patch("chat.extension.request", return_value=packet), self.assertRaises(ChatError):
                    live.read("feishu", binding(), {"mode": "live-dom", "conversationId": "oc_1", "limit": 1})

    def test_status_does_not_call_identity_only_extension_ready(self):
        status = {"ready": True, "reason": "identity-verified", "profileId": "profile-1", "accountHash": "a" * 64, "tenantHash": "b" * 64, "adapterVersion": "fixture", "diagnostics": {}, "capabilities": {"catalog": False, "history": False}}
        with patch("chat.extension.request", return_value=status):
            result = live.status("feishu", binding())["liveDom"]
        self.assertEqual(result["state"], "unavailable")
        self.assertEqual(result["reason"], "browser-api-unqualified")

    def test_pending_extension_config_does_not_override_an_existing_binding(self):
        with tempfile.TemporaryDirectory() as root:
            config = Path(root) / "config.json"
            config.write_text(json.dumps({"schemaVersion": 1, "dataRoot": root, "extensionId": "extension-1", "buildId": "build-1", "nodePath": "/usr/bin/node", "providers": {"feishu": {"profileId": "profile-1", "pending": True, "targetUrl": "https://feishu.cn/next/messenger", "allowedOrigins": ["https://feishu.cn"]}}})); config.chmod(0o600)
            # The fixed root check is part of the install contract; this fixture
            # patches it so no real user configuration is read.
            with patch("chat.config.DEFAULT_DATA_ROOT", Path(root)), patch("chat.config.DEFAULT_EXTENSION_CONFIG_FILE", config), patch("chat.config.DEFAULT_BINDING_FILE", Path(root) / "bindings.json"):
                from chat.config import load_bindings
                self.assertEqual(load_bindings(), {})

    def test_authoritative_binding_overrides_only_legacy_browser_and_explicit_file_stays_isolated(self):
        import os
        from chat.config import load_bindings
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            legacy = root / "bindings.json"
            installed = root / "config.json"
            old = binding()["browser"]
            legacy_value = {"schemaVersion": 1, "providers": {"feishu": {"snapshot_root": "/fixture/snapshot", "browser": old}, "wechat": {"snapshot_root": "/fixture/wechat"}}}
            verified = {"profileId": "bb", "pending": False, "authoritative": True, "targetUrl": old["target_url"], "allowedOrigins": old["allowed_origins"], "accountHash": "a" * 64, "tenantHash": "b" * 64}
            config = {"schemaVersion": 1, "dataRoot": str(root), "extensionId": "extension-1", "buildId": "build-2", "nodePath": "/usr/bin/node", "providers": {"feishu": verified}}
            legacy.write_text(json.dumps(legacy_value)); legacy.chmod(0o600)
            installed.write_text(json.dumps(config)); installed.chmod(0o600)
            with patch.dict(os.environ, {}, clear=True), patch("chat.config.DEFAULT_BINDING_FILE", legacy), patch("chat.config.DEFAULT_EXTENSION_CONFIG_FILE", installed), patch("chat.config.DEFAULT_DATA_ROOT", root):
                merged = load_bindings()
                self.assertEqual(merged["feishu"]["browser"]["profile_id"], "bb")
                self.assertEqual(merged["feishu"]["browser"]["build_id"], "build-2")
                self.assertEqual(merged["feishu"]["snapshot_root"], "/fixture/snapshot")
                self.assertEqual(merged["wechat"], {"snapshot_root": "/fixture/wechat"})
                validate_binding("feishu", merged["feishu"], "live-dom")
                with patch.dict(os.environ, {"OPSAIL_CHAT_BINDING_FILE": str(legacy)}):
                    self.assertEqual(load_bindings()["feishu"]["browser"], old)
            self.assertEqual(json.loads(legacy.read_text()), legacy_value)

    def test_identity_build_allow_list_keeps_the_installed_authority_readable(self):
        import os
        from chat.config import load_bindings
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            legacy, installed = root / "bindings.json", root / "config.json"
            old = binding()["browser"]
            legacy.write_text(json.dumps({"schemaVersion": 1, "providers": {"feishu": {"snapshot_root": "/fixture/snapshot"}}})); legacy.chmod(0o600)
            verified = {"profileId": "bb", "pending": False, "authoritative": True, "targetUrl": old["target_url"], "allowedOrigins": old["allowed_origins"], "accountHash": "a" * 64, "tenantHash": "b" * 64}
            config = {"schemaVersion": 1, "dataRoot": str(root), "extensionId": "extension-1", "buildId": "build-2", "nodePath": "/usr/bin/node", "providers": {"feishu": verified},
                      "identityBuilds": {"feishu": [{"name": "index.745e4057.js", "qualification": "default"}, {"name": "index.8daec7ac.js", "qualification": "qualified-by-operator"}]}}
            installed.write_text(json.dumps(config)); installed.chmod(0o600)
            with patch.dict(os.environ, {}, clear=True), patch("chat.config.DEFAULT_BINDING_FILE", legacy), patch("chat.config.DEFAULT_EXTENSION_CONFIG_FILE", installed), patch("chat.config.DEFAULT_DATA_ROOT", root):
                self.assertEqual(load_bindings()["feishu"]["browser"]["profile_id"], "bb")
                installed.write_text(json.dumps({**config, "identityBuilds": ["index.8daec7ac.js"]})); installed.chmod(0o600)
                self.assertEqual(load_bindings()["feishu"], {"snapshot_root": "/fixture/snapshot"})

    def test_unreadable_or_missing_installed_authority_never_falls_back_to_legacy_browser(self):
        import os
        from chat.config import load_bindings
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            legacy, config = root / "bindings.json", root / "config.json"
            legacy.write_text(json.dumps({"schemaVersion": 1, "providers": {"feishu": {"snapshot_root": "/fixture/snapshot", "browser": {**binding()["browser"], "transport": "chrome-tab"}}}})); legacy.chmod(0o600)
            config.write_text("broken"); config.chmod(0o600)
            with patch.dict(os.environ, {}, clear=True), patch("chat.config.DEFAULT_BINDING_FILE", legacy), patch("chat.config.DEFAULT_EXTENSION_CONFIG_FILE", config):
                self.assertEqual(load_bindings()["feishu"], {"snapshot_root": "/fixture/snapshot"})
                config.unlink()
                (root / "runtime/current/extension").mkdir(parents=True)
                self.assertEqual(load_bindings()["feishu"], {"snapshot_root": "/fixture/snapshot"})

    def test_validator_accepts_completed_extension_binding(self):
        self.assertEqual(validate_binding("feishu", binding(), "live-dom")["browser"]["transport"], "chrome-extension")

    def test_damaged_extension_config_does_not_hide_legacy_snapshot_binding(self):
        with tempfile.TemporaryDirectory() as root:
            root_path = Path(root)
            legacy = root_path / "bindings.json"
            extension_config = root_path / "config.json"
            legacy.write_text(json.dumps({"schemaVersion": 1, "providers": {"wechat": {"snapshot_root": "/fixture"}}})); legacy.chmod(0o600)
            extension_config.write_text("not json"); extension_config.chmod(0o600)
            with patch("chat.config.DEFAULT_BINDING_FILE", legacy), patch("chat.config.DEFAULT_EXTENSION_CONFIG_FILE", extension_config):
                from chat.config import load_bindings
                self.assertEqual(load_bindings()["wechat"], {"snapshot_root": "/fixture"})


if __name__ == "__main__":
    unittest.main()
