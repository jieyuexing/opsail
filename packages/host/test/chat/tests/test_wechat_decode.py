import hashlib
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str((Path(__file__).resolve().parent / '../../../src').resolve()))
from chat.snapshots import SnapshotError, read


class WechatDecodeTests(unittest.TestCase):
    def make_root(self):
        holder = tempfile.TemporaryDirectory()
        self.addCleanup(holder.cleanup)
        root = Path(holder.name)
        (root / "message").mkdir(); (root / "session").mkdir()
        return root

    def add_conversation(self, root, ident="wxid_fixture"):
        with sqlite3.connect(root / "session" / "session.db") as db:
            db.execute("CREATE TABLE SessionTable (username TEXT)")
            db.execute("INSERT INTO SessionTable VALUES (?)", (ident,))
        return ident, "Msg_" + hashlib.md5(ident.encode()).hexdigest()

    def add_shard(self, root, number, table, rows, sender_names=()):
        with sqlite3.connect(root / "message" / f"message_{number}.db") as db:
            db.execute(f"CREATE TABLE [{table}] (local_id INTEGER, local_type INTEGER, create_time INTEGER, real_sender_id INTEGER, message_content BLOB)")
            db.execute("CREATE TABLE Name2Id (user_name TEXT)")
            db.executemany(f"INSERT INTO [{table}] VALUES (?, ?, ?, ?, ?)", rows)
            db.executemany("INSERT INTO Name2Id(rowid, user_name) VALUES (?, ?)", sender_names)

    def messages(self, root, ident, limit, cursor=None):
        args = {"conversationId": ident, "limit": limit}
        if cursor: args["cursor"] = cursor
        return read("wechat", {"snapshot_root": str(root)}, args)

    def test_cross_shard_merge_tie_and_pagination_has_no_omissions(self):
        root = self.make_root(); ident, table = self.add_conversation(root)
        self.add_shard(root, 0, table, [(7, 1, 100, 1, b"old"), (9, 1, 200, 1, b"same-time-low-shard")])
        self.add_shard(root, 1, table, [(9, 1, 200, 1, b"same-time-high-shard"), (1, 1, 300, 1, b"new")])
        # A nonmatching database must neither fail the read nor become part of the cursor identity.
        self.add_shard(root, 2, "Msg_unrelated", [(1, 1, 999, 1, b"ignored")])
        first = self.messages(root, ident, 2)
        self.assertEqual([m["message_id"] for m in first["content"]["capture"]["messages"]], ["1:9", "1:1"])
        cursor = first["content"]["capture"]["extensions"]["pagination"]["next_cursor"]
        second = self.messages(root, ident, 2, cursor)
        self.assertEqual([m["message_id"] for m in second["content"]["capture"]["messages"]], ["0:7", "0:9"])

    def test_sender_rowid_is_scoped_to_its_message_shard(self):
        root = self.make_root(); ident, table = self.add_conversation(root)
        self.add_shard(root, 0, table, [(1, 1, 10, 1, b"from zero")], [(1, "sender-zero")])
        self.add_shard(root, 1, table, [(1, 1, 20, 1, b"from one")], [(1, "sender-one")])
        messages = self.messages(root, ident, 2)["content"]["capture"]["messages"]
        self.assertEqual([(message["message_id"], message["sender"]["ref"]) for message in messages], [("0:1", "sender-zero"), ("1:1", "sender-one")])

    def test_replaced_matching_shard_invalidates_existing_cursor(self):
        root = self.make_root(); ident, table = self.add_conversation(root)
        self.add_shard(root, 0, table, [(1, 1, 10, 1, b"old"), (2, 1, 20, 1, b"new")])
        first = self.messages(root, ident, 1)
        cursor = first["content"]["capture"]["extensions"]["pagination"]["next_cursor"]
        with sqlite3.connect(root / "message" / "message_0.db") as db:
            db.execute(f"UPDATE [{table}] SET message_content=? WHERE local_id=1", (b"replaced",))
        with self.assertRaises(SnapshotError) as raised:
            self.messages(root, ident, 1, cursor)
        self.assertEqual(raised.exception.code, "invalid-cursor")

    def test_complete_message_database_set_invalidates_cursor(self):
        root = self.make_root(); ident, table = self.add_conversation(root)
        self.add_shard(root, 0, table, [(1, 1, 10, 1, b"old"), (2, 1, 20, 1, b"new")])
        self.add_shard(root, 4, "Msg_unrelated", [(1, 1, 1, 1, b"unrelated")])
        first = self.messages(root, ident, 1)
        cursor = first["content"]["capture"]["extensions"]["pagination"]["next_cursor"]
        with sqlite3.connect(root / "message" / "message_4.db") as db:
            db.execute("UPDATE [Msg_unrelated] SET message_content=?", (b"changed",))
        with self.assertRaises(SnapshotError) as raised: self.messages(root, ident, 1, cursor)
        self.assertEqual(raised.exception.code, "invalid-cursor")

    def test_related_metadata_database_is_part_of_generation(self):
        root = self.make_root(); ident, table = self.add_conversation(root)
        self.add_shard(root, 0, table, [(1, 1, 10, 1, b"old"), (2, 1, 20, 1, b"new")])
        (root / "contact").mkdir()
        with sqlite3.connect(root / "contact" / "contact.db") as db:
            db.execute("CREATE TABLE receipt_marker (value TEXT)"); db.execute("INSERT INTO receipt_marker VALUES ('one')")
        cursor = self.messages(root, ident, 1)["content"]["capture"]["extensions"]["pagination"]["next_cursor"]
        with sqlite3.connect(root / "contact" / "contact.db") as db:
            db.execute("UPDATE receipt_marker SET value='two'")
        with self.assertRaises(SnapshotError) as raised: self.messages(root, ident, 1, cursor)
        self.assertEqual(raised.exception.code, "invalid-cursor")
        # A newly published numeric shard changes generation identity even when it
        # does not contain this conversation's table.
        self.add_shard(root, 5, "Msg_other", [(1, 1, 1, 1, b"other")])
        with self.assertRaises(SnapshotError) as raised: self.messages(root, ident, 1, cursor)
        self.assertEqual(raised.exception.code, "invalid-cursor")

    def test_zstd_text_is_decoded_and_bad_or_large_streams_fail_typed(self):
        if not Path("/opt/homebrew/bin/zstd").exists(): self.skipTest("zstd executable unavailable")
        root = self.make_root(); ident, table = self.add_conversation(root)
        compressed = subprocess.run(["zstd", "--quiet", "--compress", "--stdout"], input="hello compressed".encode(), capture_output=True, check=True).stdout
        self.add_shard(root, 0, table, [(1, 1, 1, 1, compressed)])
        self.assertEqual(self.messages(root, ident, 1)["content"]["capture"]["messages"][0]["text"], "hello compressed")
        with sqlite3.connect(root / "message" / "message_0.db") as db:
            db.execute(f"UPDATE [{table}] SET message_content=?", (b"\x28\xb5\x2f\xfdnot-zstd",))
        with self.assertRaises(SnapshotError) as raised: self.messages(root, ident, 1)
        self.assertEqual(raised.exception.code, "message-decode-failed")
        too_large = subprocess.run(["zstd", "--quiet", "--compress", "--stdout"], input=b"x" * (1024 * 1024 + 1), capture_output=True, check=True).stdout
        with sqlite3.connect(root / "message" / "message_0.db") as db:
            db.execute(f"UPDATE [{table}] SET message_content=?", (too_large,))
        with self.assertRaises(SnapshotError) as raised: self.messages(root, ident, 1)
        self.assertEqual(raised.exception.code, "message-decode-failed")


if __name__ == "__main__":
    unittest.main()
