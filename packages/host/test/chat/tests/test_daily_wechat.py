"""Snapshot-only daily integration; every reader/browser call is isolated."""
import copy
import json
import os
import sys
from datetime import date, datetime
from pathlib import Path

import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str((Path(__file__).resolve().parent / '../../../src').resolve()))
from chat import daily


ZONE = daily.ZoneInfo("Asia/Shanghai")
NOW = datetime(2026, 9, 29, 12, tzinfo=ZONE)
CAPTURED = "2026-08-04T12:09:38Z"
SELF = "fixture-self"
CID = "fixture@chatroom"


def message(mid="0:144", time="2026-08-03T16:05:47Z", sender_ref=SELF):
    return {"id": mid, "time": time, "date_status": None, "sender": "fixture sender",
            "sender_ref": sender_ref, "is_self": None, "text": "fixture only", "media": None, "state": None}


class SnapshotRunner:
    def __init__(self):
        self.calls = []
        self.freshness = {"snapshotCapturedAt": CAPTURED, "latestMessageAt": "2026-08-04T12:09:24Z",
                          "snapshotTimeKind": "filesystem-mtime", "snapshotVersion": "a" * 64,
                          "coverage": {"expectedMessageShards": 5, "readableMessageShards": 1, "complete": False}}
        self.catalogs = {None: {"entries": [{"conversationId": CID, "conversationName": "fixture group"}], "complete": True}}
        self.pages = {(CID, None): ([message("0:1", "2026-08-02T02:00:00Z", "wxid_other"), message()], {"has_more": False})}
        self.failure = None
        self.available = True

    def __call__(self, args):
        self.calls.append(args)
        assert args[0] == "chat" and args[1] in ("status", "catalog", "read")
        assert args[args.index("--provider") + 1] == "wechat"
        command = args[1]
        if command != "status":
            assert args[args.index("--mode") + 1] == "snapshot"
        if self.failure == command:
            return 2, {"error": {"code": f"fixture-{command}-failed"}}
        if command == "status":
            return 0, {"data": {"providers": [{"provider": "wechat", "modes": {"snapshot": {
                "available": self.available, "diagnostic": "snapshot-coverage-incomplete", "freshness": copy.deepcopy(self.freshness)}}}]}}
        cursor = args[args.index("--cursor") + 1] if "--cursor" in args else None
        if command == "catalog":
            assert args[args.index("--limit") + 1] == "100"
            return 0, {"data": copy.deepcopy(self.catalogs[cursor])}
        assert args[args.index("--limit") + 1] == "200"
        cid = args[args.index("--conversation-id") + 1]
        rows, pagination = self.pages[cid, cursor]
        return 0, {"data": {"content": {"messages": copy.deepcopy(rows),
                    "capture": {"extensions": {"pagination": copy.deepcopy(pagination)}}}}}


def collect(root, runner, *, now=NOW):
    return daily.collect(root, ["wechat"], runner=runner, now=lambda: now)["providers"][0]


def observations(root):
    return json.loads((daily.conversation_dir(root, "wechat", CID) / "coverage.json").read_text())["observations"]


class WechatDailyTests(unittest.TestCase):
    def setUp(self):
        base = Path(os.environ.get("OPSAIL_CHAT_DATA_ROOT", tempfile.gettempdir()))
        base.mkdir(parents=True, exist_ok=True)
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory(dir=base)))
        self.enterContext(patch.dict(os.environ, {"OPSAIL_CHAT_DATA_ROOT": str(self.root)}))
        def forbidden(*args, **kwargs):
            raise AssertionError("Test attempted a real subprocess/browser/snapshot call")
        self.enterContext(patch.object(daily.subprocess, "run", forbidden))
        self.enterContext(patch.object(daily, "chrome_render_flag", forbidden))
        self.enterContext(patch.object(daily, "_wiki_tickets", lambda: {}))
        self.enterContext(patch.object(daily, "WECHAT_SELF_REF", SELF))

    def test_null_date_status_is_known_and_utc_crosses_into_local_day(self):
        isolated = self.root
        runner = SnapshotRunner()
        result = collect(isolated, runner)
        assert "blocked" not in result
        rows = daily.load_messages(daily.conversation_dir(isolated, "wechat", CID))
        assert rows["0:144"]["date_status"] == "known"
        assert rows["0:144"]["time"] == "2026-08-04T00:05:47+08:00"
        assert rows["0:144"]["source"] == "snapshot"
        index, export = daily.digest(isolated, date(2026, 8, 4))
        assert index["conversations"][0]["count"] == 1
        assert export["conversations"][0]["messages"][0]["time"] == "00:05"
        assert daily.build_index(isolated, date(2026, 8, 3))[1]["conversations"] == []
        # Existing browser normalization remains conservative.
        assert daily.normalize(message(), ZONE)["date_status"] == "unknown"
        assert daily.normalize(message(time="10:05"), ZONE, source="snapshot", me=SELF)["date_status"] == "unknown"

    def test_digest_indexes_all_three_providers_without_changing_me(self):
        isolated = self.root
        daily._write_json(isolated / "daily.json", {"timezone": "Asia/Shanghai", "me": ["示例用户", "FixtureUser"]})
        before = (isolated / "daily.json").read_bytes()
        for provider in ("feishu", "teams"):
            folder = daily.conversation_dir(isolated, provider, provider + "-fixture")
            daily._write_json(folder / "conversation.json", {"conversation_id": provider + "-fixture", "name": provider})
            rows = [daily.normalize({**message(), "date_status": "known"}, ZONE)]
            daily.merge_messages(folder, rows, source="live-dom", observed_at=NOW.isoformat())
            daily.add_observation(folder, daily.observation(rows, ZONE, source="live-dom", observed_at=NOW.isoformat()))
        collect(isolated, SnapshotRunner())
        index, export = daily.digest(isolated, date(2026, 8, 4))
        assert [c["provider"] for c in index["coverage"]] == ["feishu", "teams", "wechat"]
        assert [c["provider"] for c in export["conversations"]] == ["feishu", "teams", "wechat"]
        assert (isolated / "daily.json").read_bytes() == before

    def test_self_uses_confirmed_account_ref(self):
        isolated = self.root
        for ref, expected in [(SELF, True), ("wxid_other", False), (None, None)]:
            with self.subTest(ref=ref, expected=expected):
                with tempfile.TemporaryDirectory(dir=self.root) as case_root:
                    isolated = Path(case_root)
                    runner = SnapshotRunner()
                    runner.pages[CID, None] = ([message(sender_ref=ref)], {"has_more": False})
                    collect(isolated, runner)
                    rows = daily.load_messages(daily.conversation_dir(isolated, "wechat", CID))
                    assert rows["0:144"]["is_self"] is expected
                    if expected:
                        assert daily.build_index(isolated, date(2026, 8, 4))[1]["conversations"][0]["messages"][0]["sender"] == "我"

    def test_snapshot_cutoff_and_incomplete_shards_are_visible_in_export(self):
        isolated = self.root
        runner = SnapshotRunner()
        collect(isolated, runner)
        obs = observations(isolated)[0]
        assert obs["from"] == "2026-08-02T10:00:00+08:00"
        assert obs["to"] == "2026-08-04T20:09:38+08:00"
        assert obs["to"] != NOW.isoformat() and obs["tail"] is False
        assert obs["observed_at"] == NOW.isoformat()
        assert daily.day_status([obs], date(2026, 8, 3), ZONE) == "partial"
        assert daily.day_status([obs], date(2026, 8, 5), ZONE) == "not-observed"
        assert daily.day_status([obs], date(2026, 9, 28), ZONE) == "not-observed"
        index, export = daily.digest(isolated, date(2026, 8, 4))
        cov = index["coverage"][0]
        assert cov == export["coverage"][0]
        assert cov["source"] == "snapshot"
        assert cov["snapshot"]["coverage"]["readableMessageShards"] == 1
        assert cov["snapshot"]["coverage"]["expectedMessageShards"] == 5
        assert cov["snapshot"]["snapshotCapturedAt"] == CAPTURED
        assert cov["snapshot"]["snapshotVersion"] == "a" * 64
        assert cov["snapshot"]["read"]["exhausted"] is True
        assert cov["snapshot"]["complete"] is False

    def test_shard_zero_only_is_valid_and_repeated_collection_is_idempotent(self):
        isolated = self.root
        runner = SnapshotRunner()
        assert collect(isolated, runner)["conversations"][0]["added"] == 2
        assert collect(isolated, runner)["conversations"][0]["added"] == 0
        folder = daily.conversation_dir(isolated, "wechat", CID)
        assert folder.name == "fixture%40chatroom"
        assert set(daily.load_messages(folder)) == {"0:1", "0:144"}
        assert len((folder / "messages.jsonl").read_text().splitlines()) == 2
        assert not (isolated / "daily.json").exists()

    def test_catalog_and_read_cursors_page_to_exhaustion_without_id_collisions(self):
        isolated = self.root
        runner = SnapshotRunner()
        runner.catalogs[None].update(nextCursor="catalog-2", complete=False)
        runner.catalogs["catalog-2"] = {"entries": [{"conversationId": "second", "conversationName": "second"}], "complete": True}
        runner.pages[CID, None] = ([message()], {"has_more": True, "next_cursor": "read-2"})
        runner.pages[CID, "read-2"] = ([message(), message("1:144")], {"has_more": False})
        runner.pages["second", None] = ([message("0:9")], {"has_more": False})
        # These August pages are inside this collection's seven-day window.
        result = collect(isolated, runner, now=datetime(2026, 8, 5, 12, tzinfo=ZONE))
        assert len(result["conversations"]) == 2
        assert result["conversations"][0]["pages"] == 2
        assert set(daily.load_messages(daily.conversation_dir(isolated, "wechat", CID))) == {"0:144", "1:144"}
        assert [c[1] for c in runner.calls] == ["status", "catalog", "catalog", "read", "read", "read"]

    def test_missing_snapshot_time_falls_back_to_latest_message(self):
        isolated = self.root
        for captured in [None, "undated"]:
            with self.subTest(captured=captured):
                with tempfile.TemporaryDirectory(dir=self.root) as case_root:
                    isolated = Path(case_root)
                    runner = SnapshotRunner()
                    runner.freshness["snapshotCapturedAt"] = captured
                    collect(isolated, runner)
                    obs = observations(isolated)[0]
                    assert obs["to"] == "2026-08-04T20:09:24+08:00"
                    assert obs["snapshot"]["cutoffSource"] == "latestMessageAt"
                    assert daily.day_status([obs], date(2026, 8, 5), ZONE) == "not-observed"

    def test_no_freshness_time_blocks_without_inventing_now(self):
        isolated = self.root
        runner = SnapshotRunner()
        runner.freshness.update(snapshotCapturedAt=None, latestMessageAt=None)
        assert collect(isolated, runner)["blocked"] == "snapshot-time-unknown"
        assert [c[1] for c in runner.calls] == ["status"]
        assert not (isolated / "conversations").exists()

    def test_no_cursor_keeps_a_bounded_window_and_never_claims_completeness(self):
        isolated = self.root
        for pagination in [{}, {"cursor_supported": False, "truncated": True}, {"has_more": True}]:
            with self.subTest(pagination=pagination):
                with tempfile.TemporaryDirectory(dir=self.root) as case_root:
                    isolated = Path(case_root)
                    runner = SnapshotRunner()
                    runner.freshness["coverage"] = {"expectedMessageShards": 1, "readableMessageShards": 1, "complete": True}
                    rows, _ = runner.pages[CID, None]
                    runner.pages[CID, None] = rows, pagination
                    result = collect(isolated, runner, now=datetime(2026, 8, 5, 12, tzinfo=ZONE))
                    assert result["conversations"][0]["pages"] == 1
                    obs = observations(isolated)[0]
                    assert obs["snapshot"]["read"]["exhausted"] is False
                    assert obs["snapshot"]["read"]["stop"] is None
                    assert obs["snapshot"]["complete"] is False
                    assert daily.day_status([obs], date(2026, 8, 3), ZONE) == "partial"

    def test_all_shards_and_exhausted_pages_only_prove_days_inside_snapshot(self):
        isolated = self.root
        runner = SnapshotRunner()
        runner.freshness["coverage"] = {"expectedMessageShards": 5, "readableMessageShards": 5, "complete": True}
        collect(isolated, runner)
        assert daily.day_status(observations(isolated), date(2026, 8, 3), ZONE) == "complete"
        assert daily.day_status(observations(isolated), date(2026, 8, 5), ZONE) == "not-observed"

    def test_reader_failure_stops_source_and_preserves_error_code(self):
        isolated = self.root
        for command in ["status", "catalog", "read"]:
            with self.subTest(command=command):
                with tempfile.TemporaryDirectory(dir=self.root) as case_root:
                    isolated = Path(case_root)
                    runner = SnapshotRunner()
                    runner.failure = command
                    result = collect(isolated, runner)
                    assert result["blocked"] == f"fixture-{command}-failed"
                    assert "exit 2" in result["detail"]
                    assert runner.calls[-1][1] == command
                    assert not (isolated / "conversations").exists()

    def test_repeated_cursor_stops_without_creating_coverage(self):
        isolated = self.root
        for command in ["catalog", "read"]:
            with self.subTest(command=command):
                with tempfile.TemporaryDirectory(dir=self.root) as case_root:
                    isolated = Path(case_root)
                    runner = SnapshotRunner()
                    if command == "catalog":
                        runner.catalogs[None].update(nextCursor="loop", complete=False)
                        runner.catalogs["loop"] = runner.catalogs[None]
                    else:
                        runner.pages[CID, None] = ([message()], {"has_more": True, "next_cursor": "loop"})
                        runner.pages[CID, "loop"] = runner.pages[CID, None]
                    assert collect(isolated, runner, now=datetime(2026, 8, 5, 12, tzinfo=ZONE))["blocked"] == "snapshot-cursor-invalid"
                    assert not (isolated / "conversations").exists()

    def test_failed_later_page_does_not_create_a_trusted_anchor(self):
        isolated = self.root
        runner = SnapshotRunner()
        runner.pages[CID, None] = ([message()], {"has_more": True, "next_cursor": "next"})
        def fail_next(args):
            if "--cursor" in args and args[1] == "read":
                return 2, {"error": {"code": "invalid-cursor"}}
            return runner(args)
        assert collect(isolated, fail_next, now=datetime(2026, 8, 5, 12, tzinfo=ZONE))["blocked"] == "invalid-cursor"
        assert not (isolated / "conversations").exists()

    def test_first_page_crossing_floor_stops_and_records_the_whole_page(self):
        for complete in (True, False):
            with self.subTest(shards_complete=complete), tempfile.TemporaryDirectory(dir=self.root) as directory:
                root = Path(directory)
                runner = SnapshotRunner()
                runner.freshness.update(snapshotCapturedAt=NOW.isoformat(), coverage={
                    "expectedMessageShards": 5, "readableMessageShards": 5 if complete else 1, "complete": complete})
                # Deliberately shuffle: neither endpoint is the oldest dated row.
                runner.pages[CID, None] = ([message("0:new", "2026-09-28T12:00:00+08:00"),
                    message("0:old", "2026-09-21T23:59:59+08:00"), message("0:unknown", "undated"),
                    message("1:new", "2026-09-22T12:00:00+08:00")], {"has_more": True, "next_cursor": "older"})
                runner.pages[CID, "older"] = ([message("0:history")], {"has_more": False})
                result = collect(root, runner)
                assert [c[1] for c in runner.calls] == ["status", "catalog", "read"]
                item = result["conversations"][0]
                assert item["pages"] == 1 and item["added"] == 4
                obs = observations(root)[0]
                assert obs["snapshot"]["read"] == {"pages": 1, "limit": 200, "exhausted": False,
                                                      "stop": "floor", "floor": "2026-09-22T00:00:00+08:00"}
                assert obs["snapshot"]["complete"] is complete
                assert obs["from"] == "2026-09-21T23:59:59+08:00" and obs["to"] == NOW.isoformat()
                assert obs["tail"] is False
                assert daily.day_status([obs], date(2026, 9, 22), ZONE) == ("complete" if complete else "partial")
                assert daily.day_status([obs], date(2026, 9, 21), ZONE) == "partial"
                assert daily.day_status([obs], date(2026, 9, 20), ZONE) == "not-reached"
                assert set(daily.load_messages(daily.conversation_dir(root, "wechat", CID))) == {
                    "0:new", "0:old", "0:unknown", "1:new"}
                receipt = json.loads((root / "receipts/collect/2026-09-29/120000-wechat.json").read_text())
                assert receipt["providers"][0]["conversations"][0]["stop"] == "floor"
                assert receipt["providers"][0]["conversations"][0]["floor"] == obs["snapshot"]["read"]["floor"]

    def test_latest_trusted_snapshot_extends_floor_for_only_its_conversation(self):
        runner = SnapshotRunner()
        runner.freshness.update(snapshotCapturedAt=NOW.isoformat(), coverage={
            "expectedMessageShards": 5, "readableMessageShards": 5, "complete": True})
        folder = daily.conversation_dir(self.root, "wechat", CID)
        for source, complete, cutoff in [("snapshot", True, "2026-09-01T12:00:00+08:00"),
                                         ("snapshot", True, "2026-09-10T12:00:00+08:00"),
                                         ("snapshot", False, "2026-09-25T12:00:00+08:00"),
                                         ("sdk-page", True, "2026-09-28T12:00:00+08:00")]:
            daily.add_observation(folder, {"source": source, "snapshot": {"complete": complete}, "to": cutoff})
        prior = observations(self.root)
        runner.catalogs[None]["entries"].append({"conversationId": "second", "conversationName": "second"})
        runner.pages[CID, None] = ([message("0:1", "2026-09-21T12:00:00+08:00")], {"has_more": True, "next_cursor": "equal"})
        runner.pages[CID, "equal"] = ([message("0:2", "2026-09-10T12:00:00+08:00")], {"has_more": True, "next_cursor": "cross"})
        runner.pages[CID, "cross"] = ([message("0:3", "2026-09-10T11:59:59+08:00")], {"has_more": True, "next_cursor": "history"})
        runner.pages[CID, "history"] = ([message("0:4")], {"has_more": False})
        runner.pages["second", None] = runner.pages[CID, None]
        runner.pages["second", "equal"] = ([], {"has_more": False})
        result = collect(self.root, runner)
        assert [item["pages"] for item in result["conversations"]] == [3, 1]
        assert [item["floor"] for item in result["conversations"]] == [
            "2026-09-10T12:00:00+08:00", "2026-09-22T00:00:00+08:00"]
        assert observations(self.root)[:-1] == prior
        obs = observations(self.root)[-1]
        assert obs["snapshot"]["read"]["stop"] == "floor" and obs["snapshot"]["complete"] is True
        assert obs["from"] == "2026-09-10T11:59:59+08:00"

    def test_recent_trusted_snapshot_does_not_shorten_the_lookback(self):
        runner = SnapshotRunner()
        daily.add_observation(daily.conversation_dir(self.root, "wechat", CID), {
            "source": "snapshot", "snapshot": {"complete": True}, "to": "2026-09-28T12:00:00+08:00"})
        runner.pages[CID, None] = ([message("0:1", "2026-09-27T12:00:00+08:00")], {"has_more": True, "next_cursor": "cross"})
        runner.pages[CID, "cross"] = ([message("0:2", "2026-09-21T12:00:00+08:00")], {"has_more": True, "next_cursor": "older"})
        runner.pages[CID, "older"] = ([message()], {"has_more": False})
        item = collect(self.root, runner)["conversations"][0]
        assert item["pages"] == 2 and item["stop"] == "floor"
        assert item["floor"] == "2026-09-22T00:00:00+08:00"

    def test_untrusted_or_undated_observations_do_not_extend_floor(self):
        for source, complete, cutoff in [("snapshot", False, CAPTURED), ("snapshot", 1, CAPTURED),
                                         ("live-dom", True, CAPTURED), ("snapshot", True, "unknown")]:
            with self.subTest(source=source, complete=complete, cutoff=cutoff), tempfile.TemporaryDirectory(dir=self.root) as directory:
                root = Path(directory)
                daily.add_observation(daily.conversation_dir(root, "wechat", CID), {
                    "source": source, "snapshot": {"complete": complete}, "to": cutoff})
                runner = SnapshotRunner()
                runner.pages[CID, None] = ([message(time="2026-09-21T12:00:00+08:00")], {"has_more": True, "next_cursor": "older"})
                runner.pages[CID, "older"] = ([message("0:older")], {"has_more": False})
                item = collect(root, runner)["conversations"][0]
                assert item["pages"] == 1 and item["stop"] == "floor"
                assert item["floor"] == "2026-09-22T00:00:00+08:00"

    def test_unknown_times_and_floor_equality_require_another_page(self):
        runner = SnapshotRunner()
        runner.pages[CID, None] = ([message("0:unknown", None), message("0:invalid", "yesterday")],
                                    {"has_more": True, "next_cursor": "equal"})
        runner.pages[CID, "equal"] = ([message("0:equal", "2026-09-21T16:00:00Z")], {"has_more": True, "next_cursor": "cross"})
        runner.pages[CID, "cross"] = ([message("0:old", "2026-09-21T15:59:59Z")], {"has_more": True, "next_cursor": "older"})
        runner.pages[CID, "older"] = ([message()], {"has_more": False})
        item = collect(self.root, runner)["conversations"][0]
        assert item["pages"] == 3 and item["added"] == 4 and item["stop"] == "floor"

    def test_short_conversation_exhausted_above_floor_is_complete(self):
        runner = SnapshotRunner()
        runner.freshness.update(snapshotCapturedAt=NOW.isoformat(), coverage={
            "expectedMessageShards": 5, "readableMessageShards": 5, "complete": True})
        runner.pages[CID, None] = ([message(time="2026-09-27T12:00:00+08:00")], {"has_more": False})
        item = collect(self.root, runner)["conversations"][0]
        assert item["pages"] == 1 and item["exhausted"] is True and item["stop"] == "exhausted"
        obs = observations(self.root)[0]
        assert obs["snapshot"]["complete"] is True
        assert obs["snapshot"]["read"]["floor"] == "2026-09-22T00:00:00+08:00"
        assert daily.day_status([obs], date(2026, 9, 28), ZONE) == "complete"
        assert daily.day_status([obs], date(2026, 9, 21), ZONE) == "not-reached"

    def test_floor_uses_settings_timezone_calendar_midnight(self):
        daily._write_json(self.root / "daily.json", {"timezone": "America/Los_Angeles"})
        item = collect(self.root, SnapshotRunner())["conversations"][0]
        # NOW is September 28 in Los Angeles, so the lower bound is September 21.
        assert item["floor"] == "2026-09-21T00:00:00-07:00"

    def test_repeated_cursor_on_crossing_page_still_blocks_without_merging(self):
        runner = SnapshotRunner()
        runner.pages[CID, None] = ([message(time="2026-09-28T12:00:00+08:00")], {"has_more": True, "next_cursor": "loop"})
        runner.pages[CID, "loop"] = ([message("0:old", "2026-09-21T12:00:00+08:00")], {"has_more": True, "next_cursor": "loop"})
        assert collect(self.root, runner)["blocked"] == "snapshot-cursor-invalid"
        assert not (self.root / "conversations").exists()

    def test_empty_snapshot_after_cutoff_is_not_observed(self):
        isolated = self.root
        runner = SnapshotRunner()
        runner.pages[CID, None] = ([], {"has_more": False})
        collect(isolated, runner)
        assert daily.day_status(observations(isolated), date(2026, 8, 4), ZONE) == "unreadable"
        assert daily.day_status(observations(isolated), date(2026, 8, 5), ZONE) == "not-observed"

    def test_run_orders_snapshot_after_browser_sync_and_uses_existing_notification_policy(self):
        isolated = self.root
        events, sent = [], []
        runner = SnapshotRunner()
        runner.failure = "catalog"
        def browser_collect(root, provider, *args):
            events.append(("collect", provider))
            return {"provider": provider, "conversations": []}
        def sync(root, since, *, provider, runner):
            events.append(("sync", provider))
            return {"conversations": []}
        def snapshot(args):
            events.append((args[1], "wechat"))
            return runner(args)
        self.enterContext(patch.object(daily, "collect_provider", browser_collect))
        self.enterContext(patch.object(daily, "sync", sync))
        self.enterContext(patch.object(daily, "chrome_render_flag", lambda: True))
        for _ in range(4):
            status = daily.run(isolated, today=date(2026, 8, 5), runner=snapshot,
                               rendering=lambda root: "fixture", notify=lambda *args: sent.append(args))
            assert status["issues"] == {"wechat:collect": "fixture-catalog-failed"}
        assert events[:6] == [("collect", "feishu"), ("collect", "teams"), ("sync", "feishu"), ("sync", "teams"),
                              ("status", "wechat"), ("catalog", "wechat")]
        assert len(sent) == 1
        assert json.loads((isolated / "jobs/notify.json").read_text())["wechat:collect"]["count"] == 4
        runner.failure = None
        status = daily.run(isolated, today=date(2026, 8, 5), runner=snapshot, rendering=lambda root: "fixture", notify=lambda *a: sent.append(a))
        assert status["issues"] == {}
        assert json.loads((isolated / "jobs/notify.json").read_text()) == {}
        assert json.loads((isolated / "digest/2026-08-04/index.json").read_text())["conversations"][0]["provider"] == "wechat"
        assert json.loads((isolated / "digest/2026-08-05/index.json").read_text())["coverage"][0]["status"] == "not-observed"

    def configure_refresh(self):
        command = self.root / "fixture sync"
        command.write_text("fixture only; never execute this file\n")
        command.chmod(0o700)
        daily._write_json(self.root / "daily.json", {"wechatSync": str(command)})
        return str(command)

    def run_with_refresh(self, *, invoke=None, snapshot=None, events=None, sent=None):
        events = events if events is not None else []
        sent = sent if sent is not None else []
        snapshot = snapshot or SnapshotRunner()
        def browser_collect(root, provider, *args):
            events.append(("collect", provider))
            return {"provider": provider, "conversations": []}
        def browser_sync(root, since, *, provider, runner):
            events.append(("sync", provider))
            return {"conversations": []}
        def reader(args):
            events.append((args[1], "wechat"))
            return snapshot(args)
        def refresh(args, **kwargs):
            events.append(("refresh", "wechat"))
            if invoke is None:
                raise AssertionError("Unconfigured refresh must not invoke a process")
            return invoke(args, **kwargs)
        with patch.object(daily, "collect_provider", browser_collect), patch.object(daily, "sync", browser_sync), \
                patch.object(daily, "chrome_render_flag", lambda: True), patch.object(daily.subprocess, "run", refresh):
            return daily.run(self.root, today=date(2026, 8, 5), runner=reader,
                             rendering=lambda root: "fixture", notify=lambda *args: sent.append(args))

    def test_run_refreshes_after_browser_sync_before_snapshot_and_records_success(self):
        command = self.configure_refresh()
        events, sent = [], []
        def invoke(args, **kwargs):
            assert args == [command, "--run", "--quiet"]
            assert kwargs["timeout"] == 600 and kwargs["capture_output"] and kwargs["text"]
            # Exit zero remains published even when receipt persistence emitted a warning.
            return daily.subprocess.CompletedProcess(args, 0, "PRIVATE STDOUT", '{"warning":"PRIVATE STDERR"}')
        status = self.run_with_refresh(invoke=invoke, events=events, sent=sent)
        assert events[:7] == [("collect", "feishu"), ("collect", "teams"), ("sync", "feishu"), ("sync", "teams"),
                              ("refresh", "wechat"), ("status", "wechat"), ("catalog", "wechat")]
        assert events.count(("refresh", "wechat")) == 1
        assert status["refresh"] == {"provider": "wechat", "result": "published", "code": None}
        assert status["issues"] == {} and sent == []
        receipts = list((self.root / "receipts/run").glob("*/*.json"))
        assert len(receipts) == 1
        stored = json.loads(receipts[0].read_text())
        assert stored["refresh"] == status["refresh"]
        assert "PRIVATE" not in json.dumps(stored)

    def test_run_without_refresh_configuration_skips_without_an_issue(self):
        events, sent = [], []
        status = self.run_with_refresh(events=events, sent=sent)
        assert status["refresh"] == {"provider": "wechat", "result": "skipped", "code": "not-configured"}
        assert ("refresh", "wechat") not in events and ("read", "wechat") in events
        assert status["issues"] == {} and status["notified"] == [] and sent == []

    def test_refresh_failure_collects_old_snapshot_and_reuses_notification_recovery(self):
        self.configure_refresh()
        events, sent = [], []
        def failed(args, **kwargs):
            return daily.subprocess.CompletedProcess(args, 2, "PRIVATE STDOUT", json.dumps({
                "schema_version": 1, "error": {"code": "wechat-sync-not-ready", "message": "PRIVATE STDERR"}}))
        for attempt in range(4):
            status = self.run_with_refresh(invoke=failed, events=events, sent=sent)
            assert status["refresh"] == {"provider": "wechat", "result": "blocked", "code": "wechat-sync-not-ready"}
            assert status["issues"] == {"wechat:refresh": "wechat-sync-not-ready"}
            assert len(sent) == (1 if attempt >= 2 else 0)
            assert status["collect"][-1]["provider"] == "wechat" and status["collect"][-1]["errors"] == {}
            assert "PRIVATE" not in json.dumps(status)
        assert events.count(("refresh", "wechat")) == 4 and events.count(("read", "wechat")) == 4
        obs = observations(self.root)[-1]
        assert obs["to"] == "2026-08-04T20:09:38+08:00"
        assert daily.day_status([obs], date(2026, 8, 5), ZONE) == "not-observed"
        notice = json.loads((self.root / "jobs/notify.json").read_text())
        assert notice["wechat:refresh"]["count"] == 4
        recovered = self.run_with_refresh(invoke=lambda args, **kwargs: daily.subprocess.CompletedProcess(args, 0, "", ""), sent=sent)
        assert recovered["issues"] == {} and json.loads((self.root / "jobs/notify.json").read_text()) == {}
        for _ in range(3):
            self.run_with_refresh(invoke=failed, sent=sent)
        assert len(sent) == 2

    def test_refresh_timeout_still_collects_without_persisting_process_output(self):
        self.configure_refresh()
        events = []
        def timeout(args, **kwargs):
            raise daily.subprocess.TimeoutExpired(args, kwargs["timeout"], output="PRIVATE OUT", stderr="PRIVATE ERR")
        status = self.run_with_refresh(invoke=timeout, events=events)
        assert status["refresh"] == {"provider": "wechat", "result": "blocked", "code": "refresh-timeout"}
        assert status["issues"] == {"wechat:refresh": "refresh-timeout"}
        assert events.count(("refresh", "wechat")) == 1 and ("read", "wechat") in events
        assert "PRIVATE" not in json.dumps(status)

    def test_refresh_errors_use_only_valid_stderr_error_codes(self):
        self.configure_refresh()
        for stderr, expected in [('{"error":{"code":"invalid-wechat-keys","detail":"PRIVATE"}}', "invalid-wechat-keys"),
                                 ("PRIVATE plain error", "refresh-failed"), ("[]", "refresh-failed"),
                                 ('{"error":{"code":42}}', "refresh-failed"),
                                 ('{"error":{"code":"PRIVATE error body"}}', "refresh-failed")]:
            with self.subTest(stderr=stderr):
                def failed(args, **kwargs):
                    return daily.subprocess.CompletedProcess(args, 1, '{"error":{"code":"ignore-stdout"}}', stderr)
                status = self.run_with_refresh(invoke=failed)
                assert status["refresh"]["code"] == expected and status["issues"] == {"wechat:refresh": expected}
                assert status["collect"][-1]["conversations"] == 1 and "PRIVATE" not in json.dumps(status)

    def test_refresh_spawn_failure_is_bounded_and_still_collects(self):
        self.configure_refresh()
        def failed(args, **kwargs):
            raise FileNotFoundError("PRIVATE path disappeared after settings validation")
        status = self.run_with_refresh(invoke=failed)
        assert status["refresh"] == {"provider": "wechat", "result": "blocked", "code": "refresh-failed"}
        assert status["collect"][-1]["conversations"] == 1 and "PRIVATE" not in json.dumps(status)

    def test_settings_require_an_absolute_executable_file_without_blocking_collection(self):
        command = self.configure_refresh()
        assert daily.settings(self.root)["wechatSync"] == command
        not_executable = self.root / "not-executable"
        not_executable.write_text("fixture only")
        not_executable.chmod(0o600)
        for value in ["relative/wechat-sync", "", 42, str(self.root), str(not_executable), str(self.root / "missing")]:
            with self.subTest(value=value):
                daily._write_json(self.root / "daily.json", {"wechatSync": value})
                events = []
                status = self.run_with_refresh(events=events)
                assert status["refresh"] == {"provider": "wechat", "result": "blocked", "code": "invalid-wechat-sync"}
                assert status["issues"] == {"wechat:refresh": "invalid-wechat-sync"}
                assert ("refresh", "wechat") not in events and ("read", "wechat") in events
                assert "blocked" not in collect(self.root, SnapshotRunner())

    def test_standalone_collect_and_digest_do_not_refresh_even_when_configured(self):
        self.configure_refresh()
        # setUp forbids every real subprocess; only the snapshot reader is simulated.
        result = collect(self.root, SnapshotRunner())
        assert "blocked" not in result and result["conversations"][0]["added"] == 2
        index, _ = daily.digest(self.root, date(2026, 8, 4))
        assert index["conversations"][0]["count"] == 1

    def test_cli_collect_accepts_wechat_but_sync_does_not(self):
        isolated = self.root
        providers = []
        def fake_collect(root, selected):
            assert root == Path(os.environ["OPSAIL_CHAT_DATA_ROOT"])
            providers.append(selected)
            return {"providers": [], "chrome_render_flag": None}
        self.enterContext(patch.object(daily, "collect", fake_collect))
        assert daily.main(["collect", "--provider", "wechat"]) == 0
        assert daily.main(["collect"]) == 0
        assert providers == [["wechat"], ["feishu", "teams", "wechat"]]
        assert daily.PROVIDERS == ("feishu", "teams")
        with self.assertRaises(SystemExit) as error:
            daily.main(["sync", "--provider", "wechat", "--since", "2026-08-04"])
        assert error.exception.code == 2


if __name__ == "__main__":
    unittest.main()
