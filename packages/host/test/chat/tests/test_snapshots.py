import hashlib
import json
import base64
import shutil
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str((Path(__file__).resolve().parent / '../../../src').resolve()))
from chat.snapshots import SnapshotError, catalog, read


class SnapshotTests(unittest.TestCase):
    def make_root(self):
        holder = tempfile.TemporaryDirectory()
        self.addCleanup(holder.cleanup)
        return Path(holder.name)

    def test_generation_receipt_and_atomic_current_switch_bind_bounded_read(self):
        custody = self.make_root()
        pending = custody / "sync/generations/pending"
        root = pending / "decrypted"
        for directory in ("message", "session", "contact", "head_image"):
            (root / directory).mkdir(parents=True)
        ident = "wxid_fixture"
        table = "Msg_" + hashlib.md5(ident.encode()).hexdigest()
        with sqlite3.connect(root / "message/message_0.db") as db:
            db.execute(f"CREATE TABLE [{table}] (local_id INTEGER, local_type INTEGER, create_time INTEGER, real_sender_id INTEGER, message_content TEXT)")
            db.executemany(f"INSERT INTO [{table}] VALUES (?, 1, ?, NULL, ?)", [(1, 10, "old"), (2, 20, "new")])
        with sqlite3.connect(root / "session/session.db") as db:
            db.execute("CREATE TABLE SessionTable (username TEXT)")
            db.execute("INSERT INTO SessionTable VALUES (?)", (ident,))
        with sqlite3.connect(root / "contact/contact.db") as db:
            db.execute("CREATE TABLE contact (username TEXT, remark TEXT, nick_name TEXT)")
        with sqlite3.connect(root / "head_image/head_image.db") as db:
            db.execute("CREATE TABLE head_image (username TEXT, image_buffer BLOB)")

        def publish(candidate, checked_at):
            decrypted = candidate / "decrypted"
            inventory = [{"relative": path.relative_to(decrypted).as_posix(), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()} for path in sorted(decrypted.rglob("*.db"))]
            digest = hashlib.sha256()
            for row in inventory:
                digest.update(row["relative"].encode() + b"\0" + row["sha256"].encode())
            version = digest.hexdigest()
            (candidate / "receipt.json").write_text(json.dumps({"schema_version": 1, "provider_id": "wechat", "result": "published", "published": True, "database_stats": inventory, "snapshot_version": version, "snapshot_digest": version, "checked_at": checked_at, "last_successful_sync_at": checked_at}))
            target = candidate.parent / version
            candidate.rename(target)
            (custody / "next-current").symlink_to(target / "decrypted")
            (custody / "next-current").replace(custody / "current")
            return target

        first = publish(pending, "2026-09-15T01:00:00Z")
        binding = {"snapshot_root": str(custody / "current")}
        capture = read("wechat", binding, {"conversationId": ident, "limit": 1})["content"]["capture"]
        self.assertEqual(capture["extensions"]["source"]["snapshot_observed_at_kind"], "verified-sync-receipt")
        self.assertEqual(capture["extensions"]["source"]["snapshot_observed_at"], "2026-09-15T01:00:00Z")
        cursor = capture["extensions"]["pagination"]["next_cursor"]
        shutil.copytree(first, pending)
        with sqlite3.connect(pending / "decrypted/message/message_0.db") as db:
            db.execute(f"INSERT INTO [{table}] VALUES (3, 1, 30, NULL, 'next')")
        publish(pending, "2026-09-15T02:00:00Z")
        with self.assertRaises(SnapshotError):
            read("wechat", binding, {"conversationId": ident, "limit": 1, "cursor": cursor})
        latest = read("wechat", binding, {"conversationId": ident, "limit": 1})
        self.assertEqual(latest["content"]["capture"]["messages"][0]["message_id"], "0:3")

    def test_feishu_catalog_and_bounded_read(self):
        root = self.make_root(); capture = root / "one" / "captures"; capture.mkdir(parents=True)
        capture.joinpath("2026-01-01.json").write_text(json.dumps({"schema_version": 1, "kind": "chat_export_capture", "provider": "feishu", "capture_date": "2026-01-01", "chat": {"id": "oc_1", "name": "Work"}, "messages": [{"id": "m1", "sender": "A", "time_label": "昨天 23:00", "content": "one"}, {"id": "m2", "sender": "B", "time_label": "00:10", "content": "two"}]}), encoding="utf-8")
        binding = {"snapshot_root": str(root)}
        self.assertEqual(catalog("feishu", binding, {})["entries"], [{"conversationId": "oc_1", "conversationName": "Work", "identityKind": "snapshot-chat-id"}])
        result = read("feishu", binding, {"conversationId": "oc_1", "limit": 1})
        self.assertEqual(result["item_count"], 1)
        self.assertEqual(result["content"]["capture"]["messages"][0]["message_id"], "m2")
        self.assertEqual(result["content"]["capture"]["messages"][0]["extensions"]["id_origin"], "snapshot-provider-id")
        self.assertEqual(result["content"]["capture"]["messages"][0]["sent_at"], "2026-01-01T00:10:00+08:00")
        source = result["content"]["capture"]["extensions"]["source"]
        self.assertEqual(source["mode"], "snapshot"); self.assertEqual(source["snapshot_observed_at"], "2026-01-01")
        self.assertIn("/snapshot-conversation/", result["subject_ref"])

    def test_teams_catalog_and_rejects_cursor(self):
        root = self.make_root(); conversation = root / "c1"; conversation.mkdir()
        url = "https://teams.microsoftonline.cn/l/chat/19%3Aabc/conversations?context=x"
        conversation.joinpath("state.json").write_text(json.dumps({"chat_url": url, "chat_name": "Team", "total_count": 1, "increment_complete": True, "hit_known_boundary": True, "last_export_at": "2026-01-01T00:00:00Z"}), encoding="utf-8")
        conversation.joinpath("messages.jsonl").write_text(json.dumps({"mid": "1", "author": "A", "time_iso": "2026-01-01T00:00:00Z", "body": "hello"}) + "\n", encoding="utf-8")
        binding = {"snapshot_root": str(root)}
        entry = catalog("teams", binding, {})["entries"][0]
        self.assertEqual(entry["conversationId"], "19:abc")
        self.assertEqual(read("teams", binding, {"conversationUrl": url})["completeness"], "bounded-window")
        with self.assertRaises(SnapshotError): read("teams", binding, {"conversationId": "19:abc", "cursor": "x"})

    def test_wechat_cursor_is_bound_and_pages_backward(self):
        root = self.make_root(); (root / "message").mkdir(); (root / "session").mkdir()
        conversation_id = "wxid_alice"; table = "Msg_" + hashlib.md5(conversation_id.encode()).hexdigest()
        with sqlite3.connect(root / "session" / "session.db") as db:
            db.execute("CREATE TABLE SessionTable (username TEXT)"); db.execute("INSERT INTO SessionTable VALUES (?)", (conversation_id,))
        with sqlite3.connect(root / "message" / "message_0.db") as db:
            db.execute(f"CREATE TABLE [{table}] (local_id INTEGER, local_type INTEGER, create_time INTEGER, real_sender_id INTEGER, message_content TEXT)")
            db.execute("CREATE TABLE Name2Id (user_name TEXT)")
            db.executemany(f"INSERT INTO [{table}] VALUES (?, 1, ?, 1, ?)", [(1, 10, "old"), (2, 20, "middle"), (3, 30, "new")])
        binding = {"snapshot_root": str(root)}
        self.assertEqual(catalog("wechat", binding, {})["entries"][0]["conversationId"], conversation_id)
        first = read("wechat", binding, {"conversationId": conversation_id, "limit": 2})
        messages = first["content"]["capture"]["messages"]
        self.assertEqual([item["message_id"] for item in messages], ["0:2", "0:3"])
        cursor = first["content"]["capture"]["extensions"]["pagination"]["next_cursor"]
        next_page = read("wechat", binding, {"conversationId": conversation_id, "limit": 2, "cursor": cursor})
        self.assertEqual([item["message_id"] for item in next_page["content"]["capture"]["messages"]], ["0:1"])
        with self.assertRaises(SnapshotError): read("wechat", binding, {"conversationId": "wxid_other", "cursor": cursor})
        decoded = json.loads(base64.urlsafe_b64decode(cursor + "=" * (-len(cursor) % 4)))
        decoded["b"] = [999, 999]
        forged = base64.urlsafe_b64encode(json.dumps(decoded, separators=(",", ":")).encode()).decode().rstrip("=")
        with self.assertRaises(SnapshotError): read("wechat", binding, {"conversationId": conversation_id, "cursor": forged})

    def test_rejects_snapshot_symlink_escape(self):
        root = self.make_root(); outside = self.make_root(); (outside / "message").mkdir(); (root / "message").symlink_to(outside / "message")
        with self.assertRaises(SnapshotError): catalog("wechat", {"snapshot_root": str(root)}, {})

    def test_feishu_malformed_and_same_name_are_typed_failures(self):
        root = self.make_root()
        for index in range(2):
            capture = root / str(index) / "captures"; capture.mkdir(parents=True)
            capture.joinpath("x.json").write_text(json.dumps({"schema_version": 1, "kind": "chat_export_capture", "provider": "feishu", "capture_date": "2026-01-01", "chat": {"id": f"oc_{index}", "name": "same"}, "messages": []}), encoding="utf-8")
        with self.assertRaises(SnapshotError) as raised: read("feishu", {"snapshot_root": str(root)}, {"conversationName": "same"})
        self.assertEqual(raised.exception.code, "ambiguous-conversation")
        (root / "bad" / "captures").mkdir(parents=True)
        (root / "bad" / "captures" / "bad.json").write_text("{}", encoding="utf-8")
        with self.assertRaises(SnapshotError) as raised: catalog("feishu", {"snapshot_root": str(root)}, {})
        self.assertEqual(raised.exception.code, "invalid-snapshot")

    def test_rejects_unsupported_or_conflicting_identity(self):
        root = self.make_root(); capture = root / "x" / "captures"; capture.mkdir(parents=True)
        capture.joinpath("x.json").write_text(json.dumps({"schema_version": 1, "kind": "chat_export_capture", "provider": "feishu", "capture_date": "2026-01-01", "chat": {"id": "oc", "name": "n"}, "messages": [{"id": "m"}]}), encoding="utf-8")
        with self.assertRaises(SnapshotError) as raised: read("feishu", {"snapshot_root": str(root)}, {"conversationId": "oc", "conversationUrl": "https://feishu.cn/x"})
        self.assertEqual(raised.exception.code, "invalid-request")

    def test_feishu_merged_captures_sort_before_truncation(self):
        root = self.make_root(); captures = root / "x" / "captures"; captures.mkdir(parents=True)
        base = {"schema_version": 1, "kind": "chat_export_capture", "provider": "feishu", "chat": {"id": "oc", "name": "n"}}
        captures.joinpath("a.json").write_text(json.dumps({**base, "capture_date": "2026-01-02", "updated_at": "2026-01-02T01:00:00", "messages": [{"id": "new", "time_label": "2026-01-02 10:00", "content": "new"}]}), encoding="utf-8")
        captures.joinpath("b.json").write_text(json.dumps({**base, "capture_date": "2026-01-03", "updated_at": "2026-01-03T01:00:00", "messages": [{"id": "old", "time_label": "2026-01-01 10:00", "content": "old"}]}), encoding="utf-8")
        result = read("feishu", {"snapshot_root": str(root)}, {"conversationId": "oc", "limit": 1})
        self.assertEqual(result["content"]["capture"]["messages"][0]["message_id"], "new")

    def test_teams_duplicate_message_id_fails_closed(self):
        root = self.make_root(); directory = root / "x"; directory.mkdir()
        url = "https://teams.microsoftonline.cn/l/chat/19%3Aabc/conversations"
        directory.joinpath("state.json").write_text(json.dumps({"chat_url": url, "chat_name": "n", "total_count": 2, "increment_complete": True, "hit_known_boundary": True, "last_export_at": "2026-01-01T00:00:00Z"}), encoding="utf-8")
        directory.joinpath("messages.jsonl").write_text("\n".join(json.dumps({"mid": "1", "body": body}) for body in ("one", "two")) + "\n", encoding="utf-8")
        with self.assertRaises(SnapshotError) as raised: read("teams", {"snapshot_root": str(root)}, {"conversationUrl": url})
        self.assertEqual(raised.exception.code, "invalid-snapshot")

    def test_teams_media_is_projected_to_public_shape(self):
        root = self.make_root(); directory = root / "x"; directory.mkdir()
        url = "https://teams.microsoftonline.cn/l/chat/19%3Amedia/conversations"
        directory.joinpath("state.json").write_text(json.dumps({"chat_url": url, "chat_name": "n", "total_count": 1, "increment_complete": True, "hit_known_boundary": True, "last_export_at": "2026-01-01T00:00:00Z"}), encoding="utf-8")
        directory.joinpath("messages.jsonl").write_text(json.dumps({"mid": "1", "body": "", "has_only_nontext": True, "media": [{"type": "image", "locator": "teams://fixture/image", "alt_text": "diagram", "private": "discard"}]}) + "\n", encoding="utf-8")
        result = read("teams", {"snapshot_root": str(root)}, {"conversationUrl": url})
        raw = result["content"]["messages"][0]["media"]
        self.assertEqual(raw, [{"type": "image", "locator": "teams://fixture/image", "alt_text": "diagram"}])
        self.assertEqual(result["content"]["capture"]["messages"][0]["media_refs"].__len__(), 1)

    def test_catalog_pages_and_changed_wechat_cursor_fails_closed(self):
        root = self.make_root()
        for index in range(101):
            capture = root / str(index) / "captures"; capture.mkdir(parents=True)
            capture.joinpath("x.json").write_text(json.dumps({"schema_version": 1, "kind": "chat_export_capture", "provider": "feishu", "capture_date": "2026-01-01", "chat": {"id": f"oc_{index}", "name": f"name-{index}"}, "messages": []}), encoding="utf-8")
        first = catalog("feishu", {"snapshot_root": str(root)}, {"limit": 100})
        self.assertFalse(first["complete"]); self.assertIsNotNone(first["nextCursor"])
        self.assertEqual(len(catalog("feishu", {"snapshot_root": str(root)}, {"limit": 100, "cursor": first["nextCursor"]})["entries"]), 1)


if __name__ == "__main__":
    unittest.main()
