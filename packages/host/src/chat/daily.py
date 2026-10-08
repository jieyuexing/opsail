#!/usr/bin/env python3
"""Daily chat accumulation and digest index over the existing read-only readers.

`collect` drives the existing scripts (status → prepare → catalog → select →
read) for every sidebar conversation and merges the visible windows into the
private data root by message ID. `digest` builds a deterministic per-day index
(coverage, mentions, questions, ticket hits) and can export the day's messages
for a separately written narrative digest. Nothing here sends messages,
downloads attachments, or writes Wiki/Jira state.
WeChat collection only pages the existing snapshot via status/catalog/read.
"""
from __future__ import annotations

import argparse
import fcntl
from functools import wraps
import json
import os
import re
import subprocess
import sys
import threading
from datetime import date, datetime, timedelta
from pathlib import Path
from typing import Any, Callable
from urllib.parse import quote
from zoneinfo import ZoneInfo

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from chat.config import DEFAULT_DATA_ROOT

BIN = Path(__file__).resolve().parents[2] / "bin"
PROVIDERS = ("feishu", "teams")
DIGEST_PROVIDERS = (*PROVIDERS, "wechat")
SNAPSHOT_PAGE_SIZE = 200  # The public reader's maximum; never bypass its boundary.
WECHAT_LOOKBACK_DAYS = 7
# Identity is optional and supplied by the operator or embedding instance.
WECHAT_SELF_REF = os.environ.get("OPSAIL_CHAT_WECHAT_SELF_REF") or None
MAX_OBSERVATIONS = 500
FIELDS = ("time", "date_status", "sender", "sender_ref", "is_self", "text", "media", "state")
SYNC_PAGE_SIZE, SYNC_MAX_PAGES = 50, 40
SCROLL_MAX_STEPS, SCROLL_SETTLE_SECONDS = 30, 1.5
BROWSER_START_SECONDS, BROWSER_POLL_SECONDS = 60, 3
PREPARABLE = ("tab-not-prepared", "tab-missing")
Runner = Callable[[list[str]], tuple[int, dict[str, Any] | None]]


class DailyError(RuntimeError):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


def run_cli(args: list[str]) -> tuple[int, dict[str, Any] | None]:
    """Run one existing Opsail CLI and return its exit code and JSON envelope."""
    try:
        done = subprocess.run([str(BIN / args[0]), *args[1:]], capture_output=True, text=True, timeout=180)
    except subprocess.TimeoutExpired:
        return 124, None
    for stream in (done.stdout, done.stderr):
        try:
            return done.returncode, json.loads(stream)
        except ValueError:
            continue
    return done.returncode, None


def settings(root: Path) -> dict[str, Any]:
    path = root / "daily.json"
    value = json.loads(path.read_text(encoding="utf-8")) if path.exists() else {}
    command, error = value.get("wechatSync"), None
    if command is not None:
        try:
            valid = isinstance(command, str) and Path(command).is_absolute() and Path(command).is_file() and os.access(command, os.X_OK)
        except (OSError, ValueError):
            valid = False
        if not valid:
            command, error = None, "invalid-wechat-sync"
    # A refresh configuration problem must not prevent standalone reads or digests.
    return {"timezone": value.get("timezone", "Asia/Shanghai"), "me": [n for n in value.get("me", []) if isinstance(n, str) and n],
            "wechatSync": command, "wechatSyncError": error}


def conversation_dir(root: Path, provider: str, conversation_id: str) -> Path:
    return root / "conversations" / provider / quote(conversation_id, safe="")


def _write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(json.dumps(value, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
    os.chmod(tmp, 0o600)
    tmp.replace(path)


def _read_json(path: Path, default: Any) -> Any:
    return json.loads(path.read_text(encoding="utf-8")) if path.exists() else default


# ---- time --------------------------------------------------------------------------

def parse_time(value: Any, zone: ZoneInfo) -> datetime | None:
    """ISO with offset is exact; an offset-less ISO is the page's local wall time."""
    if not isinstance(value, str) or not re.match(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}", value):
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return parsed.astimezone(zone) if parsed.tzinfo else parsed.replace(tzinfo=zone)


def day_bounds(day: date, zone: ZoneInfo) -> tuple[datetime, datetime]:
    start = datetime(day.year, day.month, day.day, tzinfo=zone)
    return start, start + timedelta(days=1)


# ---- messages ----------------------------------------------------------------------

def normalize(row: dict[str, Any], zone: ZoneInfo, *, source: str = "live-dom", me: str | None = None) -> dict[str, Any]:
    moment = parse_time(row.get("time"), zone) if source == "snapshot" or row.get("date_status", "known") == "known" else None
    is_self = row.get("is_self")
    if source == "snapshot":
        is_self = row.get("sender_ref") == me if me and row.get("sender_ref") else None
    return {
        "id": row.get("id"),
        "time": moment.isoformat() if moment else row.get("time"),
        "date_status": "known" if moment else "unknown",
        "sender": row.get("sender") or "",
        "sender_ref": row.get("sender_ref"),
        "is_self": is_self,
        "text": row.get("text") or "",
        "media": [{"type": m.get("type")} for m in row.get("media") or [] if isinstance(m, dict)],
        "state": row.get("state"),
    }


EMOJI = re.compile(r"\[[^\[\]\s]{1,40}\]")


def _same_text(a: str, b: str) -> bool:
    """DOM and SDK projections differ in line breaks and emoji tokens; that is not an edit."""
    key = lambda text: "".join(EMOJI.sub("", text or "").split())
    return key(a) == key(b)


def _better(old: dict[str, Any], new: dict[str, Any], *, readable: bool = False) -> dict[str, Any]:
    """Keep the strongest evidence per field; a weaker later window never erases it.
    `readable` marks a DOM row, whose rendered text wins over an equivalent SDK projection."""
    merged = dict(old)
    if new["date_status"] == "known" or old.get("date_status") != "known":
        merged["time"], merged["date_status"] = new["time"], new["date_status"]
    named = lambda r: r.get("sender") and r.get("sender") != r.get("sender_ref")
    if named(new) or not named(old):
        merged["sender"] = new["sender"] or old.get("sender", "")
    merged["sender_ref"] = new["sender_ref"] or old.get("sender_ref")
    if isinstance(new["is_self"], bool):
        merged["is_self"] = new["is_self"]
    if new["text"] and (readable or not _same_text(new["text"], old.get("text") or "")):
        merged["text"] = new["text"]
    if new["media"] and not (old.get("media") and all(m.get("type") in ("unavailable", "inline-media") for m in new["media"])):
        merged["media"] = new["media"]
    if new.get("state"):
        merged["state"] = new["state"]
    return merged


def load_messages(folder: Path) -> dict[str, dict[str, Any]]:
    latest: dict[str, dict[str, Any]] = {}
    path = folder / "messages.jsonl"
    if path.exists():
        for line in path.read_text(encoding="utf-8").split("\n"):
            if line.strip():
                record = json.loads(line)
                latest[record["id"]] = record
    return latest


def merge_messages(folder: Path, rows: list[dict[str, Any]], *, source: str, observed_at: str) -> tuple[int, int]:
    """Append new messages and changed revisions; unchanged rows write nothing."""
    latest = load_messages(folder)
    lines, added, updated = [], 0, 0
    for row in rows:
        if not row.get("id"):
            continue
        old = latest.get(row["id"])
        if old is None:
            record = {**row, "rev": 1, "source": source, "first_seen": observed_at, "observed_at": observed_at}
            added += 1
        else:
            merged = _better(old, row, readable=source == "live-dom" and _same_text(row["text"], old.get("text") or ""))
            if all(merged.get(k) == old.get(k) for k in FIELDS):
                continue
            record = {**merged, "rev": old.get("rev", 1) + 1, "source": source, "observed_at": observed_at}
            updated += 1
        latest[row["id"]] = record
        lines.append(json.dumps(record, ensure_ascii=False))
    if lines:
        folder.mkdir(parents=True, exist_ok=True, mode=0o700)
        path = folder / "messages.jsonl"
        with path.open("a", encoding="utf-8") as handle:
            handle.write("\n".join(lines) + "\n")
        os.chmod(path, 0o600)
    return added, updated


# ---- coverage ----------------------------------------------------------------------

def observation(rows: list[dict[str, Any]], zone: ZoneInfo, *, source: str, observed_at: str, tail: bool = True) -> dict[str, Any]:
    """A visible window is one contiguous run: proven from its oldest to newest dated row.
    `tail` is false for a window scrolled away from the newest messages."""
    times = sorted(t for t in (parse_time(r["time"], zone) for r in rows if r["date_status"] == "known") if t)
    return {"source": source, "observed_at": observed_at, "count": len(rows), **({} if tail else {"tail": False}),
            "from": times[0].isoformat() if times else None, "to": times[-1].isoformat() if times else None}


def add_observation(folder: Path, value: dict[str, Any]) -> None:
    path = folder / "coverage.json"
    data = _read_json(path, {"observations": []})
    data["observations"] = (data["observations"] + [value])[-MAX_OBSERVATIONS:]
    _write_json(path, data)


def _union(intervals: list[tuple[datetime, datetime]]) -> list[tuple[datetime, datetime]]:
    result: list[tuple[datetime, datetime]] = []
    for start, end in sorted(intervals):
        if result and start <= result[-1][1]:
            result[-1] = (result[-1][0], max(result[-1][1], end))
        else:
            result.append((start, end))
    return result


def day_status(observations: list[dict[str, Any]], day: date, zone: ZoneInfo) -> str:
    """complete: dated rows prove the whole day; likely-complete: only if each window
    was the pane's tail at observation time; partial: the day was only partly seen;
    not-reached: every window started after the day."""
    start, end = day_bounds(day, zone)
    if observations and all(o.get("source") == "snapshot" and parse_time(o.get("to"), zone)
                            and parse_time(o["to"], zone) < start for o in observations):
        return "not-observed"
    dated = [o for o in observations if o.get("from")]
    if not dated:
        return "unreadable" if observations else "not-observed"
    # Incomplete shards or an unproven page boundary cannot prove a whole day.
    trusted = [o for o in dated if o.get("source") != "snapshot" or o.get("snapshot", {}).get("complete") is True]
    proven = _union([(parse_time(o["from"], zone), parse_time(o["to"], zone)) for o in trusted])
    tail_interval = lambda o: (parse_time(o["from"], zone), parse_time(o["observed_at"] if o.get("tail", True) else o["to"], zone))
    tail = _union([tail_interval(o) for o in trusted])
    if any(a < start and b >= end for a, b in proven):
        return "complete"
    if any(a < start and b >= end for a, b in tail):
        return "likely-complete"
    tail = _union([tail_interval(o) for o in dated])
    if any(a < end and b >= start for a, b in tail):
        return "partial"
    # Every window began after the day: its messages, if any, are above the pane.
    return "not-reached" if any(a >= end for a, _ in tail) else "not-observed"


# ---- collect -----------------------------------------------------------------------

class Lock:
    """Nonblocking, process-owned lease. Never unlink the flock inode.

    The old mkdir/PID marker is checked conservatively for upgrade safety.
    Drain old-code jobs before switching versions: they do not know this flock.
    """
    def __init__(self, root: Path):
        self.path = root / "jobs" / "daily-collect.flock"
        self.legacy = root / "jobs" / "daily-collect.lock"
        self.fd: int | None = None

    def __enter__(self) -> "Lock":
        if self.fd is not None:
            raise RuntimeError("Lock is already held by this instance.")
        self.path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        fd = os.open(self.path, os.O_CREAT | os.O_RDWR | os.O_CLOEXEC | os.O_NOFOLLOW, 0o600)
        try:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise DailyError("collect-busy", "Another daily collection is running.") from None
            self._check_legacy()
        except BaseException:
            os.close(fd)
            raise
        self.fd = fd
        return self

    def _check_legacy(self) -> None:
        if not self.legacy.exists():
            return
        try:
            owner = _read_json(self.legacy / "owner.json", {})
            pid = owner.get("pid") if isinstance(owner, dict) else None
            if type(pid) is int and pid > 0:
                try:
                    os.kill(pid, 0)
                except ProcessLookupError:
                    return  # Only confirmed death permits legacy recovery.
        except (OSError, ValueError):
            pass
        # Missing/partial metadata may be a live old process between mkdir and
        # owner publication. EPERM and PID reuse are not proof of death either.
        raise DailyError("collect-busy", "Legacy collection lock is live or unverified; drain old jobs before migration.")

    def __exit__(self, *_: Any) -> None:
        if self.fd is not None:
            try:
                os.close(self.fd)
            finally:
                self.fd = None


_operation_locks = threading.local()


def _serialized(operation: Callable) -> Callable:
    """Hold one lease for a synchronous operation, including its nested steps.

    Only the current thread's call stack may reuse a lease. PID is part of the
    key so a fork cannot mistake the parent's stack for its own lease.
    """
    @wraps(operation)
    def invoke(root: Path, *args: Any, **kwargs: Any) -> Any:
        key = (os.getpid(), root.resolve())
        active = getattr(_operation_locks, "active", None)
        if active is None:
            active = _operation_locks.active = set()
        if key in active:
            return operation(root, *args, **kwargs)
        with Lock(root):
            active.add(key)
            try:
                return operation(root, *args, **kwargs)
            finally:
                active.remove(key)
    return invoke


def _code(envelope: dict[str, Any] | None, fallback: str) -> str:
    error = (envelope or {}).get("error")
    return error.get("code", fallback) if isinstance(error, dict) else fallback


def _ready(runner: Runner, provider: str) -> str | None:
    code, envelope = runner(["chat", "status", "--provider", provider])
    providers = ((envelope or {}).get("data") or {}).get("providers") or []
    live = ((providers[0] if providers else {}).get("modes") or {}).get("live-dom") or {}
    if code == 0 and live.get("available"):
        return None
    return (live.get("extension") or {}).get("reason") or live.get("diagnostic") or _code(envelope, "status-unavailable")


# Chrome stops rendering a window that other apps fully cover; Feishu and Teams
# then never paint a newly selected conversation. This switch keeps it rendering.
RENDER_FLAG = "--disable-backgrounding-occluded-windows"


def launch_chrome() -> None:
    """Start Chrome in the background without focusing it; its profile loads the bound extension."""
    subprocess.run(["open", "-g", "-a", "Google Chrome", "--args", RENDER_FLAG], check=False, capture_output=True, timeout=30)


def chrome_render_flag() -> bool | None:
    """Whether the running Chrome keeps covered windows rendering (None: not running or unknown)."""
    try:
        listing = subprocess.run(["ps", "-axo", "command="], capture_output=True, text=True, timeout=10).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    main = [line for line in listing.splitlines() if "/Google Chrome.app/Contents/MacOS/Google Chrome" in line and "--type=" not in line]
    return all(RENDER_FLAG in line for line in main) if main else None


def user_tab_count(targets: list[str]) -> int | None:
    """Tabs other than blank/new-tab pages and the bound pages. Only the count leaves AppleScript."""
    skip = "{" + ", ".join(json.dumps(t) for t in targets) + "}"
    script = f"""tell application "Google Chrome"
  set n to 0
  repeat with w in windows
    repeat with t in tabs of w
      set u to URL of t
      if not (u starts with "chrome://newtab" or u starts with "chrome://new-tab-page" or u is "about:blank" or {skip} contains u) then set n to n + 1
    end repeat
  end repeat
  return n
end tell"""
    try:
        done = subprocess.run(["osascript", "-e", script], capture_output=True, text=True, timeout=30)
        return int(done.stdout.strip()) if done.returncode == 0 else None
    except (OSError, ValueError, subprocess.SubprocessError):
        return None


def quit_chrome() -> bool:
    subprocess.run(["osascript", "-e", 'quit app "Google Chrome"'], capture_output=True, timeout=30)
    for _ in range(20):
        if chrome_render_flag() is None:
            return True
        __import__("time").sleep(1)
    return False


def ensure_rendering(root: Path, *, flag: Callable[[], bool | None] = chrome_render_flag, count: Callable[[list[str]], int | None] = user_tab_count,
                     quit: Callable[[], bool] = quit_chrome, launch: Callable[[], None] = launch_chrome) -> str:
    """Restart a flagless Chrome only when it holds no user tabs (user decision 2026-09-24)."""
    state = flag()
    if state is None:
        return "not-running"
    if state:
        return "flag-present"
    targets = [p.get("targetUrl") for p in (_read_json(root / "config.json", {}).get("providers") or {}).values() if isinstance(p, dict) and p.get("targetUrl")]
    if count(targets) != 0:
        return "kept-user-tabs"
    if not quit():
        return "quit-failed"
    launch()
    return "restarted"


def ensure_ready(runner: Runner, provider: str, *, launch: Callable[[], None] = launch_chrome,
                 sleep: Callable[[float], None] | None = None) -> str | None:
    """Unattended runs start a closed Chrome and wait for the extension; other blockers stay blocked.
    A missing bound tab is not a blocker: the caller prepares it next."""
    blocked = _ready(runner, provider)
    if blocked in PREPARABLE:
        return None
    if blocked != "extension-unavailable":
        return blocked
    launch()
    sleep = sleep or __import__("time").sleep
    for _ in range(BROWSER_START_SECONDS // BROWSER_POLL_SECONDS):
        sleep(BROWSER_POLL_SECONDS)
        blocked = _ready(runner, provider)
        if blocked != "extension-unavailable":
            return None if blocked in PREPARABLE else blocked
    return blocked


CATALOG_ATTEMPTS, CATALOG_WAIT_SECONDS = 12, 3  # a cold Chrome start renders the Feishu feed slowly


def stable_catalog(runner: Runner, provider: str, sleep: Callable[[float], None]) -> tuple[list[dict[str, Any]] | None, str | None]:
    """A freshly prepared page renders its sidebar gradually: retry failures and
    accept the catalog once two consecutive reads return the same conversations."""
    previous, code = None, "catalog-failed"
    for attempt in range(CATALOG_ATTEMPTS):
        if attempt:
            sleep(CATALOG_WAIT_SECONDS)
        status, envelope = runner(["chat", "catalog", "--provider", provider, "--mode", "live-dom", "--limit", "100"])
        entries = ((envelope or {}).get("data") or {}).get("entries") if status == 0 else None
        if not isinstance(entries, list):
            code = _code(envelope, "catalog-failed")
            continue
        ids = [e.get("conversationId") for e in entries]
        if previous is not None and ids == previous[0]:
            return entries, None
        previous = (ids, entries)
    return (previous[1], None) if previous else (None, code)


def collect_provider(root: Path, provider: str, zone: ZoneInfo, runner: Runner, now: Callable[[], datetime],
                     ensure: Callable[[Runner, str], str | None] = ensure_ready, sleep: Callable[[float], None] | None = None) -> dict[str, Any]:
    result: dict[str, Any] = {"provider": provider, "conversations": []}
    blocked = ensure(runner, provider)
    if blocked is None:
        code, envelope = runner(["chat", "prepare", "--provider", provider, "--quiet"])
        blocked = None if code == 0 else _code(envelope, "prepare-failed")
    if blocked is None:
        entries, blocked = stable_catalog(runner, provider, sleep or __import__("time").sleep)
    if blocked is not None:
        return {**result, "blocked": blocked}
    for entry in entries:
        cid, name = entry.get("conversationId"), entry.get("conversationName")
        if not isinstance(cid, str) or not cid:
            continue
        item: dict[str, Any] = {"conversation_id": cid, "name": name}
        folder = conversation_dir(root, provider, cid)
        meta = _read_json(folder / "conversation.json", None)
        position = entry.get("lastMessagePosition")
        if meta and isinstance(position, int) and meta.get("last_position") == position:
            # The feed's last message position is unchanged: nothing new, no click.
            observed_at = now().isoformat()
            latest = max((m["time"] for m in load_messages(folder).values() if m.get("date_status") == "known"), default=None)
            if latest:
                add_observation(folder, {"source": "feed-position", "observed_at": observed_at, "count": 0, "from": latest, "to": observed_at})
            _write_json(folder / "conversation.json", {**meta, "name": name, "last_catalog_seen": observed_at})
            result["conversations"].append({**item, "skipped": "unchanged"})
            continue
        for attempt in range(2):  # one same-path retry for a transient pane failure
            code, envelope = runner(["chat", "select", "--provider", provider, "--conversation-id", cid, "--quiet"])
            if code == 0:
                code, envelope = runner(["chat", "read", "--provider", provider, "--mode", "live-dom", "--conversation-id", cid, "--limit", "200"])
            if code == 0:
                break
        observed_at = now().isoformat()
        meta = meta or {"provider": provider, "conversation_id": cid, "first_seen": observed_at}
        # Store the catalog position only after a successful read; a newer message makes it differ next time.
        _write_json(folder / "conversation.json", {**meta, "name": name, "last_catalog_seen": observed_at,
                                                   **({"last_position": position} if code == 0 and isinstance(position, int) else {})})
        if code != 0:
            item["error"] = _code(envelope, "read-failed")
        else:
            rows = [normalize(r, zone) for r in ((envelope.get("data") or {}).get("content") or {}).get("messages") or []]
            item["added"], item["updated"] = merge_messages(folder, rows, source="live-dom", observed_at=observed_at)
            item["visible"] = len(rows)
            add_observation(folder, observation(rows, zone, source="live-dom", observed_at=observed_at))
        result["conversations"].append(item)
    return result


def _snapshot_data(runner: Runner, command: str, *args: str) -> dict[str, Any]:
    request = ["chat", command, "--provider", "wechat"]
    if command != "status":
        request += ["--mode", "snapshot"]
    code, envelope = runner([*request, *args])
    data = (envelope or {}).get("data")
    if code != 0 or not isinstance(data, dict):
        raise DailyError(_code(envelope, f"snapshot-{command}-failed"), f"WeChat {command} failed (exit {code}).")
    return data


def _snapshot_catalog(runner: Runner) -> list[dict[str, Any]]:
    entries, seen, cursor = {}, set(), None
    while True:
        data = _snapshot_data(runner, "catalog", "--limit", "100", *(["--cursor", cursor] if cursor else []))
        if not isinstance(data.get("entries"), list):
            raise DailyError("snapshot-catalog-invalid", "Snapshot catalog has no entries list.")
        for entry in data["entries"]:
            cid = entry.get("conversationId")
            if isinstance(cid, str) and cid:
                entries[cid] = entry
        cursor = data.get("nextCursor")
        if not cursor:
            if data.get("complete") is False:
                raise DailyError("snapshot-catalog-incomplete", "Snapshot catalog ended without a continuation cursor.")
            return list(entries.values())
        if not isinstance(cursor, str) or cursor in seen:
            raise DailyError("snapshot-cursor-invalid", "Snapshot catalog cursor did not advance.")
        seen.add(cursor)


def _snapshot_rows(runner: Runner, cid: str, zone: ZoneInfo, floor: datetime) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    rows, seen, cursor, pages = {}, set(), None, 0
    while True:
        data = _snapshot_data(runner, "read", "--conversation-id", cid, "--limit", str(SNAPSHOT_PAGE_SIZE),
                              *(["--cursor", cursor] if cursor else []))
        content = data.get("content") or {}
        if not isinstance(content.get("messages"), list):
            raise DailyError("snapshot-read-invalid", "Snapshot read has no messages list.")
        for row in content["messages"]:
            if not isinstance(row.get("id"), str) or not row["id"]:
                raise DailyError("snapshot-message-id-missing", "Snapshot row has no source message ID.")
            rows[row["id"]] = row
        pages += 1
        pagination = ((content.get("capture") or {}).get("extensions") or {}).get("pagination") or {}
        cursor = pagination.get("next_cursor")
        if cursor and (not isinstance(cursor, str) or cursor in seen):
            raise DailyError("snapshot-cursor-invalid", "Snapshot message cursor did not advance.")
        # Pages move toward older messages; do not depend on their internal order.
        oldest = min((t for t in (parse_time(row.get("time"), zone) for row in content["messages"]) if t), default=None)
        exhausted = not cursor and pagination.get("has_more") is False and pagination.get("truncated") is not True
        stop = "exhausted" if exhausted else "floor" if oldest is not None and oldest < floor else None
        if stop or not cursor:
            return list(rows.values()), {"pages": pages, "limit": SNAPSHOT_PAGE_SIZE, "exhausted": exhausted,
                                         "stop": stop, "floor": floor.isoformat()}
        seen.add(cursor)


def collect_wechat_snapshot(root: Path, zone: ZoneInfo, runner: Runner, now: Callable[[], datetime]) -> dict[str, Any]:
    """Read only the public snapshot boundary; missing shards are metadata, not a blocker."""
    result: dict[str, Any] = {"provider": "wechat", "conversations": []}
    try:
        providers = _snapshot_data(runner, "status").get("providers") or []
        mode = ((providers[0] if providers else {}).get("modes") or {}).get("snapshot") or {}
        if not mode.get("available"):
            raise DailyError(mode.get("diagnostic") or "snapshot-unavailable", "WeChat snapshot is unavailable.")
        freshness = mode.get("freshness") or {}
        cutoff_source = "snapshotCapturedAt" if parse_time(freshness.get("snapshotCapturedAt"), zone) else "latestMessageAt"
        cutoff = parse_time(freshness.get(cutoff_source), zone)
        if cutoff is None:
            raise DailyError("snapshot-time-unknown", "Snapshot status has no dated cutoff.")
        snapshot = {k: freshness.get(k) for k in ("snapshotCapturedAt", "latestMessageAt", "snapshotTimeKind", "snapshotVersion", "coverage")}
        snapshot.update({"cutoff": cutoff.isoformat(), "cutoffSource": cutoff_source})
        shards = snapshot.get("coverage") or {}
        shard_complete = (shards.get("complete") is True and isinstance(shards.get("expectedMessageShards"), int)
                          and shards["expectedMessageShards"] > 0 and shards.get("readableMessageShards") == shards["expectedMessageShards"])
        lookback = day_bounds(now().astimezone(zone).date() - timedelta(days=WECHAT_LOOKBACK_DAYS), zone)[0]
        for entry in _snapshot_catalog(runner):
            cid, name = entry["conversationId"], entry.get("conversationName")
            folder = conversation_dir(root, "wechat", cid)
            floor = lookback
            for prior in reversed(_read_json(folder / "coverage.json", {"observations": []})["observations"]):
                if prior.get("source") == "snapshot" and prior.get("snapshot", {}).get("complete") is True:
                    previous_cutoff = parse_time(prior.get("to"), zone)
                    if previous_cutoff is not None:
                        floor = min(floor, previous_cutoff)
                    break
            raw, pagination = _snapshot_rows(runner, cid, zone, floor)
            rows = [normalize(row, zone, source="snapshot", me=WECHAT_SELF_REF) for row in raw]
            observed_at = now().isoformat()
            meta = _read_json(folder / "conversation.json", {"provider": "wechat", "conversation_id": cid, "first_seen": observed_at})
            _write_json(folder / "conversation.json", {**meta, "name": name, "last_catalog_seen": observed_at})
            added, updated = merge_messages(folder, rows, source="snapshot", observed_at=observed_at)
            obs = observation(rows, zone, source="snapshot", observed_at=observed_at, tail=False)
            # Collection time proves when we read it, never how fresh the snapshot is.
            obs.update({"to": cutoff.isoformat(), "snapshot": {**snapshot, "read": pagination,
                        "complete": shard_complete and pagination["stop"] in ("floor", "exhausted")}})
            add_observation(folder, obs)
            result["conversations"].append({"conversation_id": cid, "name": name, "added": added, "updated": updated,
                                            "visible": len(rows), **pagination})
    except DailyError as error:
        # Stop this source at the first failure; no retry, fallback, or browser action.
        result.update({"blocked": error.code, "detail": str(error)})
    return result


@_serialized
def collect(root: Path, providers: list[str], *, runner: Runner = run_cli, now: Callable[[], datetime] | None = None,
            ensure: Callable[[Runner, str], str | None] = ensure_ready, sleep: Callable[[float], None] | None = None) -> dict[str, Any]:
    zone = ZoneInfo(settings(root)["timezone"])
    now = now or (lambda: datetime.now(zone))
    started = now()
    results = [collect_wechat_snapshot(root, zone, runner, now) if p == "wechat"
               else collect_provider(root, p, zone, runner, now, ensure, sleep) for p in providers]
    receipt = {"operation": "collect", "started_at": started.isoformat(), "finished_at": now().isoformat(), "providers": results,
               "chrome_render_flag": chrome_render_flag() if any(p in PROVIDERS for p in providers) else None}
    suffix = "-wechat" if providers == ["wechat"] else ""
    _write_json(root / "receipts" / "collect" / started.date().isoformat() / f"{started.strftime('%H%M%S')}{suffix}.json", receipt)
    return receipt


# ---- Feishu page-SDK backfill --------------------------------------------------------

Pager = Callable[[str, str | None], dict[str, Any]]
HISTORY_START = "1970-01-01T00:00:00+00:00"


def extension_pager() -> Pager:
    from chat import extension
    from chat.config import load_bindings
    binding = {**load_bindings()["feishu"], "_provider": "feishu"}
    return lambda cid, cursor: extension.request(binding, "messagesPage", {"conversationId": cid, "limit": SYNC_PAGE_SIZE, **({"cursor": cursor} if cursor else {})})


def self_ref(root: Path, provider: str) -> str | None:
    """The operator's sender ID, learned only from rows the DOM reader proved to be self."""
    refs = {m.get("sender_ref") for _, folder, _ in (c for c in conversations(root) if c[0] == provider)
            for m in load_messages(folder).values() if m.get("is_self") is True and m.get("sender_ref")}
    return refs.pop() if len(refs) == 1 else None


def sdk_row(message: dict[str, Any], me: str | None) -> dict[str, Any]:
    sender = message.get("sender") or {}
    ref = sender.get("ref")
    return {"id": message.get("id"), "time": message.get("sent_at"), "date_status": "known",
            "sender": sender.get("display_name") or ref or "", "sender_ref": ref,
            "is_self": (ref == me) if me and ref else None, "text": message.get("text") or "",
            "media": [{"type": m.get("kind")} for m in message.get("media") or [] if isinstance(m, dict)],
            "state": "recalled" if message.get("is_recalled") else "deleted" if message.get("is_deleted") else None}


def contiguous_from(by_position: dict[int, dict[str, Any]], invalid: set[int], missing: set[int]) -> str | None:
    """Oldest time of the gap-free run below the newest position; position 1 is the chat start."""
    if not by_position:
        return None
    position = max(by_position)
    oldest = by_position[position]["sent_at"]
    while position > 1:
        below = position - 1
        if below in by_position:
            oldest = by_position[below]["sent_at"]
        elif below not in invalid or below in missing:
            return oldest
        position = below
    return HISTORY_START


def sync_conversation(root: Path, cid: str, since: datetime, zone: ZoneInfo, pager: Pager, runner: Runner, now: Callable[[], datetime], me: str | None) -> dict[str, Any]:
    code, envelope = runner(["chat", "select", "--provider", "feishu", "--conversation-id", cid, "--quiet"])
    if code != 0:
        return {"error": _code(envelope, "select-failed")}
    by_position: dict[int, dict[str, Any]] = {}
    invalid: set[int] = set()
    missing: set[int] = set()
    cursor, pages, reached = None, 0, False
    while pages < SYNC_MAX_PAGES:
        try:
            data = pager(cid, cursor)
        except Exception as error:  # a rejected page keeps what was already paged
            return {"error": getattr(error, "code", "page-failed"), "pages": pages, **_sync_merge(root, cid, by_position, invalid, missing, zone, now, me)}
        pages += 1
        fresh = 0
        for message in data.get("messages") or []:
            if isinstance(message.get("position"), int) and message["position"] not in by_position:
                by_position[message["position"]] = message
                fresh += 1
        invalid |= set(data.get("invalidPositions") or [])
        missing |= set(data.get("missingPositions") or [])
        oldest = parse_time(min(m["sent_at"] for m in by_position.values()), zone) if by_position else None
        if oldest and oldest < since or 1 in by_position:
            reached = True
            break
        if not fresh or not data.get("nextCursor"):
            break
        cursor = data["nextCursor"]
    return {"pages": pages, "reached": reached, **_sync_merge(root, cid, by_position, invalid, missing, zone, now, me)}


def _sync_merge(root: Path, cid: str, by_position: dict[int, dict[str, Any]], invalid: set[int], missing: set[int], zone: ZoneInfo, now: Callable[[], datetime], me: str | None) -> dict[str, Any]:
    if not by_position:
        return {"added": 0, "updated": 0}
    folder = conversation_dir(root, "feishu", cid)
    observed_at = now().isoformat()
    rows = [normalize(sdk_row(m, me), zone) for _, m in sorted(by_position.items())]
    added, updated = merge_messages(folder, rows, source="sdk-page", observed_at=observed_at)
    start = contiguous_from(by_position, invalid, missing)
    # The first page must contain the feed's lastMessagePosition (the probe rejects
    # it otherwise), so the run is proven up to the observation time.
    add_observation(folder, {"source": "sdk-page", "observed_at": observed_at, "count": len(rows),
                             "from": parse_time(start, zone).isoformat() if start else None, "to": observed_at})
    return {"added": added, "updated": updated}


# ---- DOM scroll backfill (Teams; no page SDK exists) ---------------------------------

Scroller = Callable[[str, str, str], dict[str, Any]]


def extension_scroller(provider: str) -> Scroller:
    from chat import extension
    from chat.config import load_bindings
    binding = {**load_bindings()[provider], "_provider": provider}
    return lambda _provider, cid, direction: extension.request(binding, "scrollBack", {"conversationId": cid, "direction": direction})


def _read_rows(runner: Runner, provider: str, cid: str, zone: ZoneInfo) -> tuple[list[dict[str, Any]] | None, str | None]:
    code, envelope = runner(["chat", "read", "--provider", provider, "--mode", "live-dom", "--conversation-id", cid, "--limit", "200"])
    if code != 0:
        return None, _code(envelope, "read-failed")
    return [normalize(r, zone) for r in ((envelope.get("data") or {}).get("content") or {}).get("messages") or []], None


def scroll_conversation(root: Path, provider: str, cid: str, since: datetime, zone: ZoneInfo, scroller: Scroller, runner: Runner,
                        now: Callable[[], datetime], sleep: Callable[[float], None]) -> dict[str, Any]:
    """Scroll the verified pane up one step at a time, merging each settled window, then return to the bottom."""
    code, envelope = runner(["chat", "select", "--provider", provider, "--conversation-id", cid, "--quiet"])
    if code != 0:
        return {"error": _code(envelope, "select-failed")}
    folder = conversation_dir(root, provider, cid)
    added = updated = steps = stale = 0
    reached, error = False, None
    rows, error = _read_rows(runner, provider, cid, zone)
    seen: set[str] = set()
    tail = True
    try:
        while rows is not None:
            observed_at = now().isoformat()
            a, u = merge_messages(folder, rows, source="live-dom", observed_at=observed_at)
            added, updated = added + a, updated + u
            add_observation(folder, observation(rows, zone, source="live-dom", observed_at=observed_at, tail=tail))
            fresh = {r["id"] for r in rows} - seen
            seen |= fresh
            stale = 0 if fresh else stale + 1
            times = [parse_time(r["time"], zone) for r in rows if r["date_status"] == "known"]
            if any(t and t < since for t in times):
                reached = True
                break
            if stale >= 3 or steps >= SCROLL_MAX_STEPS:
                break
            try:
                moved = scroller(provider, cid, "up")
            except Exception as failure:
                error = getattr(failure, "code", "scroll-failed")
                break
            steps += 1
            if not moved.get("moved") and moved.get("atTop"):
                reached = True  # the conversation start is the boundary
                sleep(SCROLL_SETTLE_SECONDS)
                rows, error = _read_rows(runner, provider, cid, zone)
                if rows is not None:
                    observed_at = now().isoformat()
                    a, u = merge_messages(folder, rows, source="live-dom", observed_at=observed_at)
                    added, updated = added + a, updated + u
                    seen_top = observation(rows, zone, source="live-dom", observed_at=observed_at, tail=False)
                    add_observation(folder, {**seen_top, "from": HISTORY_START} if seen_top["from"] else seen_top)
                break
            tail = False
            sleep(SCROLL_SETTLE_SECONDS)
            rows, error = _read_rows(runner, provider, cid, zone)
    finally:
        try:
            scroller(provider, cid, "bottom")
        except Exception as failure:
            error = error or getattr(failure, "code", "scroll-restore-failed")
    return {"steps": steps, "reached": reached, "added": added, "updated": updated, **({"error": error} if error else {})}


# The page SDK cannot project some bot/card conversations. The first failure is
# reported; the same code on the same conversation afterwards is a known limit.
UNREADABLE = {"selected-chat-page-unrecognized", "selected-chat-anchor-unverified"}


def _known_unreadable(folder: Path, outcome: dict[str, Any]) -> dict[str, Any]:
    code = outcome.get("error")
    if code not in UNREADABLE:
        return outcome
    meta = _read_json(folder / "conversation.json", None)
    if meta is None:
        return outcome
    if meta.get("unreadable") == code:
        return {**{k: v for k, v in outcome.items() if k != "error"}, "unreadable": code}
    _write_json(folder / "conversation.json", {**meta, "unreadable": code})
    return outcome


@_serialized
def sync(root: Path, since: date, *, provider: str = "feishu", conversation_id: str | None = None, runner: Runner = run_cli,
         pager: Pager | None = None, scroller: Scroller | None = None, now: Callable[[], datetime] | None = None,
         sleep: Callable[[float], None] | None = None, ensure: Callable[[Runner, str], str | None] = ensure_ready) -> dict[str, Any]:
    """Backfill each conversation to `since`: Feishu pages its page SDK, Teams scrolls the DOM.
    Conversations whose days since then are already covered are skipped."""
    zone = ZoneInfo(settings(root)["timezone"])
    now = now or (lambda: datetime.now(zone))
    started = now()
    start, _ = day_bounds(since, zone)
    days = [since + timedelta(days=i) for i in range((started.date() - since).days)]
    blocked = ensure(runner, provider)
    if blocked is None:
        code, envelope = runner(["chat", "prepare", "--provider", provider, "--quiet"])
        blocked = None if code == 0 else _code(envelope, "prepare-failed")
    if blocked is not None:
        raise DailyError(blocked, f"{provider} browser session is not ready.")
    if conversation_id:
        targets = [(conversation_id, None)]
    else:
        entries, failed = stable_catalog(runner, provider, sleep or __import__("time").sleep)
        if failed:
            raise DailyError(failed, f"{provider} catalog is unavailable.")
        targets = [(e["conversationId"], e.get("conversationName")) for e in entries
                   if isinstance(e.get("conversationId"), str) and (provider != "feishu" or re.fullmatch(r"\d+", e["conversationId"]))]
    covered = ("complete",) if provider == "feishu" else ("complete", "likely-complete")
    if provider == "feishu":
        pager, me = pager or extension_pager(), self_ref(root, "feishu")
    else:
        scroller, sleep = scroller or extension_scroller(provider), sleep or __import__("time").sleep
    results = []
    for cid, name in targets:
        observations = _read_json(conversation_dir(root, provider, cid) / "coverage.json", {"observations": []})["observations"]
        if not conversation_id and days and all(day_status(observations, d, zone) in covered for d in days):
            results.append({"conversation_id": cid, "name": name, "skipped": "covered"})
            continue
        outcome = sync_conversation(root, cid, start, zone, pager, runner, now, me) if provider == "feishu" \
            else scroll_conversation(root, provider, cid, start, zone, scroller, runner, now, sleep)
        outcome = _known_unreadable(conversation_dir(root, provider, cid), outcome)
        results.append({"conversation_id": cid, "name": name, **outcome})
    receipt = {"operation": "sync", "provider": provider, "since": since.isoformat(), "started_at": started.isoformat(), "finished_at": now().isoformat(), "conversations": results}
    _write_json(root / "receipts" / "sync" / started.date().isoformat() / f"{started.strftime('%H%M%S')}-{provider}.json", receipt)
    return receipt


# ---- digest index ------------------------------------------------------------------

TICKET = re.compile(os.environ.get("OPSAIL_CHAT_TICKET_PATTERN", r"[A-Z][A-Z0-9]*-\d+|(?<![\w#])#\d{5}\b"))
QUESTION = re.compile(r"[?？]|吗\s*$|吗[，,。]")


def _wiki_tickets() -> dict[str, str]:
    # Optional root -> display-prefix mapping. No configured roots means no scan.
    try:
        mappings = json.loads(os.environ.get("OPSAIL_CHAT_TICKET_PATHS", "{}"))
        if not isinstance(mappings, dict) or any(
            not isinstance(base, str) or not Path(base).is_absolute() or not isinstance(prefix, str)
            for base, prefix in mappings.items()
        ):
            raise ValueError()
    except (ValueError, TypeError):
        raise DailyError("invalid-ticket-paths", "Ticket paths must map absolute roots to display prefixes.") from None
    found: dict[str, str] = {}
    for directory, prefix in mappings.items():
        base = Path(directory)
        if base.is_dir():
            for path in base.rglob("*"):
                if path.is_dir() and TICKET.fullmatch(path.name):
                    found.setdefault(path.name, str(Path(prefix) / path.relative_to(base)))
    return found


def conversations(root: Path) -> list[tuple[str, Path, dict[str, Any]]]:
    result = []
    for provider in DIGEST_PROVIDERS:
        base = root / "conversations" / provider
        for folder in sorted(base.iterdir()) if base.is_dir() else []:
            meta = _read_json(folder / "conversation.json", None)
            if meta:
                result.append((provider, folder, meta))
    return result


def build_index(root: Path, day: date) -> tuple[dict[str, Any], dict[str, Any]]:
    config = settings(root)
    zone, me = ZoneInfo(config["timezone"]), config["me"]
    start, end = day_bounds(day, zone)
    tickets_in_wiki = _wiki_tickets()
    coverage, convs, export = [], [], []
    candidates: dict[str, Any] = {"mentions": [], "questions": [], "unreplied": [], "tickets": {}}
    for provider, folder, meta in conversations(root):
        messages = sorted(load_messages(folder).values(), key=lambda m: m.get("time") or "")
        dated = [(m, parse_time(m["time"], zone)) for m in messages if m.get("date_status") == "known"]
        today = [(m, t) for m, t in dated if t and start <= t < end]
        observations = _read_json(folder / "coverage.json", {"observations": []})["observations"]
        status = day_status(observations, day, zone)
        name = meta.get("name") or meta["conversation_id"]
        coverage.append({"provider": provider, "conversation_id": meta["conversation_id"], "name": name, "status": status, "messages": len(today)})
        if provider == "wechat":
            latest = next((o for o in reversed(observations) if o.get("source") == "snapshot"), {})
            coverage[-1].update({"source": "snapshot", "snapshot": latest.get("snapshot")})
        if not today:
            continue
        others = {m.get("sender_ref") or m.get("sender") for m in messages if m.get("is_self") is False}
        p2p = len(others) <= 1
        senders: dict[str, int] = {}
        for m, _ in today:
            label = "(me)" if m.get("is_self") else m.get("sender") or "?"
            senders[label] = senders.get(label, 0) + 1
        convs.append({"provider": provider, "conversation_id": meta["conversation_id"], "name": name, "p2p": p2p, "count": len(today), "senders": senders})
        export.append({"provider": provider, "name": name, "coverage": status, "messages": [
            {"time": t.strftime("%H:%M"), "sender": "我" if m.get("is_self") else m.get("sender"), "text": m.get("text"),
             **({"media": [x["type"] for x in m["media"]]} if m.get("media") else {})} for m, t in today]})
        for index, (m, t) in enumerate(today):
            ref = {"provider": provider, "conversation": name, "id": m["id"], "time": t.strftime("%H:%M"), "sender": m.get("sender"), "excerpt": (m.get("text") or "")[:120]}
            text = m.get("text") or ""
            for key in TICKET.findall(text):
                candidates["tickets"].setdefault(key, {"wiki": tickets_in_wiki.get(key), "refs": []})["refs"].append(ref)
            if m.get("is_self"):
                continue
            later_self = any(n.get("is_self") and (n.get("time") or "") > (m.get("time") or "") for n in messages)
            if any(name_ in text for name_ in me):
                candidates["mentions"].append({**ref, "answered_later": later_self})
            elif QUESTION.search(text) and p2p:
                candidates["questions"].append({**ref, "answered_later": later_self})
        last = today[-1][0]
        if p2p and last.get("is_self") is False and (last.get("text") or "").strip() and not any(n.get("is_self") and (n.get("time") or "") > (last.get("time") or "") for n in messages):
            candidates["unreplied"].append({"provider": provider, "conversation": name, "id": last["id"], "time": today[-1][1].strftime("%H:%M"), "excerpt": (last.get("text") or "")[:120]})
    index = {"date": day.isoformat(), "timezone": config["timezone"], "coverage": coverage, "conversations": convs, "candidates": candidates}
    return index, {"date": day.isoformat(), "coverage": coverage, "conversations": export, "candidates": candidates}


STATUS_ORDER = ("complete", "likely-complete", "partial")


def write_index_md(root: Path) -> Path:
    rows = []
    for folder in sorted((root / "digest").glob("????-??-??"), reverse=True):
        index = _read_json(folder / "index.json", None)
        if not index:
            continue
        active = [c for c in index["coverage"] if c["messages"]]
        counts = {s: sum(1 for c in active if c["status"] == s) for s in STATUS_ORDER}
        cand = index["candidates"]
        gaps = [c["name"] for c in index["coverage"] if c["status"] in ("not-reached", "unreadable")]
        digest = f"[日报]({folder.name}/digest.md)" if (folder / "digest.md").exists() else "未生成"
        rows.append(f"| {folder.name} | {len(active)} | {sum(c['messages'] for c in active)} | "
                    + " / ".join(f"{k} {v}" for k, v in counts.items() if v) + f" | {len(gaps)} | {len(cand['mentions'])} / {len(cand['questions'])} / {len(cand['unreplied'])} | {digest} |")
    path = root / "digest" / "INDEX.md"
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    last = _read_json(root / "jobs" / "last-run.json", None)
    status = [] if not last else [f"最近一次定时运行：{last['finished_at'][:16].replace('T', ' ')} — "
                                  + ("正常" if not last["issues"] else "问题 " + "，".join(f"{k} {v}" for k, v in last["issues"].items())), ""]
    path.write_text("\n".join([
        "# 聊天日报索引", "", *status,
        "私有数据，仅配置的数据目录；不进 Git、不写 Wiki。完整度：complete=消息证明整天覆盖；likely-complete=假定可见窗口为当时最新；partial=只见部分；"
        "缺口=窗口没往前到达该日（not-reached）或读不出消息（unreadable）的会话数，明细见当日 index.json。", "",
        "| 日期 | 有消息的会话 | 消息数 | 完整度（有消息的会话） | 缺口 | @我 / 问句 / 待回复 | 日报 |",
        "| --- | --- | --- | --- | --- | --- | --- |", *rows, ""]), encoding="utf-8")
    return path


def digest(root: Path, day: date) -> tuple[dict[str, Any], dict[str, Any]]:
    index, export = build_index(root, day)
    _write_json(root / "digest" / day.isoformat() / "index.json", index)
    write_index_md(root)
    return index, export


# ---- scheduled run ------------------------------------------------------------------

# Blockers a person must fix are reported at once; others only after three runs in a row.
IMMEDIATE = {"login-required", "identity-unverified", "identity-changed", "account-changed", "tenant-changed",
             "not-bound", "binding-changed", "site-permission-required", "invalid-build", "bridge-disabled"}
NOTIFY_AFTER_RUNS = 3


def notify_local(title: str, message: str) -> None:
    """A local macOS notification only; nothing is ever sent to another person."""
    subprocess.run(["osascript", "-e", f"display notification {json.dumps(message, ensure_ascii=False)} with title {json.dumps(title, ensure_ascii=False)}"],
                   capture_output=True, timeout=30)


def refresh_wechat_snapshot(config: dict[str, Any], *, runner: Callable[..., Any] | None = None) -> dict[str, Any]:
    """Invoke the configured source owner once; retain only status and a bounded error code."""
    result = {"provider": "wechat", "result": "blocked", "code": "refresh-failed"}
    if config.get("wechatSyncError"):
        return {**result, "code": config["wechatSyncError"]}
    command = config.get("wechatSync")
    if command is None:
        return {**result, "result": "skipped", "code": "not-configured"}
    try:
        done = (runner or subprocess.run)([command, "--run", "--quiet"], capture_output=True, text=True,
                                          encoding="utf-8", errors="replace", stdin=subprocess.DEVNULL, timeout=600)
    except subprocess.TimeoutExpired:
        return {**result, "code": "refresh-timeout"}
    except OSError:
        return result
    if done.returncode == 0:
        return {**result, "result": "published", "code": None}
    try:
        envelope = json.loads(done.stderr)
    except (ValueError, TypeError):
        return result
    error = envelope.get("error") if isinstance(envelope, dict) else None
    code = error.get("code") if isinstance(error, dict) else None
    if isinstance(code, str) and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}", code):
        result["code"] = code
    return result


def run_issues(collected: list[dict[str, Any]], synced: list[dict[str, Any]], refresh: dict[str, Any] | None = None) -> dict[str, str]:
    issues: dict[str, str] = {}
    for step, rows in (("collect", collected), ("sync", synced)):
        for row in rows:
            if row.get("blocked"):
                issues[f"{row['provider']}:{step}"] = row["blocked"]
            elif row.get("errors"):
                issues[f"{row['provider']}:{step}"] = "conversation-errors"
    if refresh and refresh.get("result") == "blocked":
        issues["wechat:refresh"] = refresh["code"]
    return issues


def apply_notify_policy(root: Path, issues: dict[str, str], notify: Callable[[str, str], None]) -> list[str]:
    """Same issue is notified once per episode; a resolved issue starts a new episode when it recurs."""
    path = root / "jobs" / "notify.json"
    previous, current, sent = _read_json(path, {}), {}, []
    for key, code in issues.items():
        prior = previous.get(key) if previous.get(key, {}).get("code") == code else {}
        count = prior.get("count", 0) + 1
        notified = prior.get("notified", False)
        if not notified and (code in IMMEDIATE or count >= NOTIFY_AFTER_RUNS):
            notify("Opsail 聊天采集", f"{key} {code}：见 {root / 'digest/INDEX.md'}")
            notified = True
            sent.append(key)
        current[key] = {"code": code, "count": count, "notified": notified}
    _write_json(path, current)
    return sent


def _summaries(receipt_rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [{"provider": p["provider"], **({"blocked": p["blocked"]} if "blocked" in p else {}),
             "conversations": len(p.get("conversations", [])), "skipped": sum(1 for c in p.get("conversations", []) if "skipped" in c),
             "added": sum(c.get("added", 0) for c in p.get("conversations", [])), "updated": sum(c.get("updated", 0) for c in p.get("conversations", [])),
             "errors": {c.get("name") or c["conversation_id"]: c["error"] for c in p.get("conversations", []) if "error" in c}} for p in receipt_rows]


@_serialized
def run(root: Path, *, today: date | None = None, runner: Runner = run_cli, notify: Callable[[str, str], None] = notify_local,
        rendering: Callable[[Path], str] = ensure_rendering) -> dict[str, Any]:
    """Collect/backfill browsers, refresh and collect WeChat snapshots, then index both days."""
    config = settings(root)
    zone = ZoneInfo(config["timezone"])
    started = datetime.now(zone)
    today = today or started.date()
    yesterday = today - timedelta(days=1)
    chrome = rendering(root)
    collected = _summaries(collect(root, list(PROVIDERS), runner=runner)["providers"])
    synced = []
    for provider in PROVIDERS:
        try:
            synced += _summaries([{"provider": provider, "conversations": sync(root, yesterday, provider=provider, runner=runner)["conversations"]}])
        except DailyError as error:
            synced.append({"provider": provider, "blocked": error.code})
    refresh = refresh_wechat_snapshot(config)
    try:
        collected += _summaries(collect(root, ["wechat"], runner=runner)["providers"])
    except DailyError as error:
        collected.append({"provider": "wechat", "blocked": error.code})
    for day in (yesterday, today):
        digest(root, day)
    issues = run_issues(collected, synced, refresh)
    status = {"operation": "run", "started_at": started.isoformat(), "finished_at": datetime.now(zone).isoformat(), "chrome": chrome,
              "chrome_render_flag": chrome_render_flag(), "collect": collected, "sync": synced, "refresh": refresh, "issues": issues}
    _write_json(root / "receipts" / "run" / started.date().isoformat() / f"{started.strftime('%H%M%S')}.json", status)
    _write_json(root / "jobs" / "last-run.json", {k: status[k] for k in ("finished_at", "chrome", "issues")})
    status["notified"] = apply_notify_policy(root, issues, notify)
    write_index_md(root)
    return status


# ---- CLI ---------------------------------------------------------------------------

def _render_hint(flag: bool | None, summary: list[dict[str, Any]]) -> dict[str, str]:
    failed = any(p.get("blocked") or p.get("errors") for p in summary)
    return {"hint": f"Chrome was started without {RENDER_FLAG}; covered windows stop rendering. Quit Chrome and let chat collect start it."} if flag is False and failed else {}

def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="chat")
    sub = parser.add_subparsers(dest="command", required=True)
    c = sub.add_parser("collect", help="采集飞书/Teams 可见窗口与微信 snapshot，按消息 ID 累积")
    c.add_argument("--provider", choices=(*DIGEST_PROVIDERS, "all"), default="all")
    y = sub.add_parser("sync", help="把会话往前补到指定日期：飞书用页面 SDK 翻页，Teams 自动向上滚动")
    y.add_argument("--provider", choices=(*PROVIDERS, "all"), default="all")
    y.add_argument("--since", required=True)
    y.add_argument("--conversation-id")
    sub.add_parser("run", help="定时入口：飞书/Teams collect+sync → 微信 snapshot → 昨天与今天索引，按策略发本机通知")
    d = sub.add_parser("digest", help="生成某日索引 index.json 并刷新 INDEX.md")
    d.add_argument("--date", required=True)
    d.add_argument("--export", action="store_true", help="输出当日消息 JSON 供撰写 digest.md")
    args = parser.parse_args(argv)
    root = Path(os.environ.get("OPSAIL_CHAT_DATA_ROOT", str(DEFAULT_DATA_ROOT)))
    try:
        if args.command == "collect":
            receipt = collect(root, list(DIGEST_PROVIDERS) if args.provider == "all" else [args.provider])
            summary = [{"provider": p["provider"], **({"blocked": p["blocked"]} if "blocked" in p else {}),
                        "conversations": len(p["conversations"]), "added": sum(c.get("added", 0) for c in p["conversations"]),
                        "updated": sum(c.get("updated", 0) for c in p["conversations"]),
                        "errors": [c["name"] for c in p["conversations"] if "error" in c]} for p in receipt["providers"]]
            print(json.dumps({"operation": "collect", "providers": summary, **_render_hint(receipt["chrome_render_flag"], summary)}, ensure_ascii=False))
            return 2 if any("blocked" in p for p in summary) else 0
        if args.command == "run":
            status = run(root)
            print(json.dumps({k: status[k] for k in ("operation", "chrome", "collect", "sync", "issues", "notified")}, ensure_ascii=False))
            return 2 if status["issues"] else 0
        if args.command == "sync":
            providers = list(PROVIDERS) if args.provider == "all" else [args.provider]
            if args.conversation_id and len(providers) != 1:
                raise DailyError("provider-required", "--conversation-id needs one --provider.")
            summary = []
            for provider in providers:
                try:
                    rows = sync(root, date.fromisoformat(args.since), provider=provider, conversation_id=args.conversation_id)["conversations"]
                except DailyError as error:
                    summary.append({"provider": provider, "blocked": error.code})
                    continue
                summary.append({"provider": provider, "conversations": len(rows), "skipped": sum(1 for r in rows if "skipped" in r),
                                "added": sum(r.get("added", 0) for r in rows), "updated": sum(r.get("updated", 0) for r in rows),
                                "unreached": [r["name"] or r["conversation_id"] for r in rows if "skipped" not in r and not r.get("reached") and "error" not in r],
                                "errors": {r["name"] or r["conversation_id"]: r["error"] for r in rows if "error" in r}})
            print(json.dumps({"operation": "sync", "since": args.since, "providers": summary, **_render_hint(chrome_render_flag(), summary)}, ensure_ascii=False))
            return 2 if any("blocked" in p for p in summary) else 0
        day = date.fromisoformat(args.date)
        index, export = digest(root, day)
        print(json.dumps(export if args.export else {"operation": "digest", "date": index["date"],
              "conversations": len(index["conversations"]), "candidates": {k: len(v) for k, v in index["candidates"].items()}}, ensure_ascii=False))
        return 0
    except DailyError as error:
        print(json.dumps({"error": {"code": error.code, "message": str(error)}}), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
