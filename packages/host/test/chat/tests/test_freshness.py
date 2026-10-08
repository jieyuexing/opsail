"""Freshness metadata is bounded, sanitized, and never inferred from mtime alone."""

from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import sys
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str((Path(__file__).resolve().parent / '../../../src').resolve()))

from chat.config import ChatError  # noqa: E402
from chat.freshness import snapshot_freshness  # noqa: E402


def _write_json(path: Path, value: dict, *, private: bool = False) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value), encoding="utf-8")
    if private:
        path.chmod(0o600)


def _sqlite(path: Path, *, timestamp: int | None = None) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(path) as database:
        if timestamp is not None:
            database.execute("CREATE TABLE Msg_0123456789abcdef (create_time INTEGER)")
            database.execute("INSERT INTO Msg_0123456789abcdef VALUES (?)", (timestamp,))
        else:
            database.execute("CREATE TABLE fixture (value TEXT)")


def _digest(rows: list[dict[str, str]]) -> str:
    value = hashlib.sha256()
    for row in rows:
        value.update(row["relative"].encode())
        value.update(b"\0")
        value.update(row["sha256"].encode())
    return value.hexdigest()


class SnapshotFreshnessTests(unittest.TestCase):
    def _legacy_wechat(self, root: Path, timestamp: int = 1_789_000_000) -> Path:
        decrypted = root / "decrypted"
        _sqlite(decrypted / "message/message_0.db", timestamp=timestamp)
        for relative in ("contact/contact.db", "session/session.db", "head_image/head_image.db"):
            _sqlite(decrypted / relative)
        return decrypted

    def _generation_wechat(self, custody: Path) -> tuple[Path, list[dict[str, str]], str]:
        pending = custody / "sync/generations/pending"
        decrypted = self._legacy_wechat(pending)
        rows = [{"relative": path.relative_to(decrypted).as_posix(), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()} for path in sorted(decrypted.rglob("*.db"))]
        digest = _digest(rows)
        generation = pending.parent / digest
        pending.rename(generation)
        return generation / "decrypted", rows, digest

    def test_legacy_wechat_uses_message_time_not_database_mtime(self):
        with tempfile.TemporaryDirectory() as directory:
            decrypted = self._legacy_wechat(Path(directory), timestamp=1_700_000_000)
            future = 1_790_000_000
            for path in decrypted.rglob("*.db"):
                os.utime(path, (future, future))
            result = snapshot_freshness("wechat", {"snapshot_root": str(decrypted)})
        self.assertEqual(result["latestMessageAt"], datetime.fromtimestamp(1_700_000_000, timezone.utc).isoformat().replace("+00:00", "Z"))
        self.assertEqual(result["snapshotTimeKind"], "filesystem-mtime")
        self.assertNotEqual(result["latestMessageAt"], result["snapshotCapturedAt"])

    def test_private_source_binding_marks_five_expected_one_present_incomplete(self):
        with tempfile.TemporaryDirectory() as directory:
            custody = Path(directory) / "custody"
            decrypted = self._legacy_wechat(custody)
            source = Path(directory) / "source"
            for index in range(5):
                (source / "message").mkdir(parents=True, exist_ok=True)
                (source / f"message/message_{index}.db").touch()
            _write_json(custody / "sync/binding.json", {"schema_version": 1, "source_root": str(source)}, private=True)
            result = snapshot_freshness("wechat", {"snapshot_root": str(decrypted)})
        self.assertEqual(result["coverage"], {"expectedMessageShards": 5, "readableMessageShards": 1, "databaseCount": 4, "complete": False})

    def test_receipt_uses_database_stats_and_binds_every_database_digest(self):
        with tempfile.TemporaryDirectory() as directory:
            custody = Path(directory) / "custody"
            decrypted, rows, version = self._generation_wechat(custody)
            receipt = {"schema_version": 1, "database_stats": rows, "snapshot_digest": version, "snapshot_version": version, "checked_at": "2026-09-15T00:00:00Z", "last_successful_sync_at": "2026-09-15T00:00:00Z"}
            _write_json(decrypted.parent / "receipt.json", receipt)
            result = snapshot_freshness("wechat", {"snapshot_root": str(decrypted)})
            self.assertEqual(result["snapshotVersion"], version)
            self.assertEqual(result["snapshotTimeKind"], "verified-sync-receipt")
            self.assertEqual(result["lastSuccessfulSyncAt"], "2026-09-15T00:00:00Z")

            receipt["database_stats"] = rows[:-1]
            _write_json(decrypted.parent / "receipt.json", receipt)
            with self.assertRaisesRegex(ChatError, "freshness could not be verified"):
                snapshot_freshness("wechat", {"snapshot_root": str(decrypted)})

            receipt["database_stats"] = rows + [{"relative": "message/extra.db", "sha256": "b" * 64}]
            _write_json(decrypted.parent / "receipt.json", receipt)
            with self.assertRaises(ChatError):
                snapshot_freshness("wechat", {"snapshot_root": str(decrypted)})

            receipt["database_stats"] = rows + ["extra statistic"]
            _write_json(decrypted.parent / "receipt.json", receipt)
            with self.assertRaises(ChatError):
                snapshot_freshness("wechat", {"snapshot_root": str(decrypted)})

            receipt["database_stats"] = rows
            receipt["snapshot_digest"] = "c" * 64
            _write_json(decrypted.parent / "receipt.json", receipt)
            with self.assertRaises(ChatError):
                snapshot_freshness("wechat", {"snapshot_root": str(decrypted)})

    def test_failed_attempt_cannot_advance_last_successful_sync(self):
        with tempfile.TemporaryDirectory() as directory:
            custody = Path(directory) / "custody"
            decrypted, rows, version = self._generation_wechat(custody)
            _write_json(decrypted.parent / "receipt.json", {"schema_version": 1, "database_stats": rows, "snapshot_digest": version, "snapshot_version": version, "checked_at": "2026-09-15T00:00:00Z", "last_successful_sync_at": "2026-09-15T00:00:00Z"})
            _write_json(custody / "sync/receipts/latest-attempt.json", {"result": "failed", "code": "decrypt-failed", "checked_at": "2026-09-16T00:00:00Z", "last_successful_sync_at": "2099-01-01T00:00:00Z", "snapshot_version": version})
            result = snapshot_freshness("wechat", {"snapshot_root": str(decrypted)})
        self.assertEqual(result["lastSuccessfulSyncAt"], "2026-09-15T00:00:00Z")
        self.assertEqual(result["lastSyncResult"], {"checkedAt": "2026-09-16T00:00:00Z", "result": "failed", "code": "decrypt-failed"})

    def test_generation_snapshot_requires_its_verified_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            decrypted, _rows, _version = self._generation_wechat(Path(directory) / "custody")
            with self.assertRaisesRegex(ChatError, "freshness could not be verified"):
                snapshot_freshness("wechat", {"snapshot_root": str(decrypted)})

    def test_feishu_and_teams_metadata_are_sanitized_and_unknown_timestamps_remain_null(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _write_json(root / "private-conversation/state.json", {"last_export_at": "not-a-time", "seen_max_time": "also-not-a-time", "conversation_name": "do-not-return", "absolute_path": "/private/source"})
            for provider in ("feishu", "teams"):
                result = snapshot_freshness(provider, {"snapshot_root": str(root)})
                serialized = json.dumps(result)
                self.assertNotIn("do-not-return", serialized)
                self.assertNotIn("/private/source", serialized)
                self.assertIsNone(result["snapshotCapturedAt"])
                self.assertIsNone(result["latestMessageAt"])
                self.assertEqual(result["snapshotTimeKind"], "unknown")

    def test_malformed_provider_state_is_unavailable(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _write_json(root / "chat/state.json", ["not", "an", "object"])
            with self.assertRaisesRegex(ChatError, "freshness could not be verified"):
                snapshot_freshness("feishu", {"snapshot_root": str(root)})


if __name__ == "__main__":
    unittest.main()
