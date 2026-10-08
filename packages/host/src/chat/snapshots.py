"""Read-only, bounded chat snapshot observations.

Bindings are private Host input.  Results deliberately expose neither local paths
nor credential material.  This is a clean-room format reader, not a retired CLI.
"""
from __future__ import annotations

import base64
import hashlib
import json
import re
import subprocess
import tempfile
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from typing import Any
from urllib.parse import unquote, urlparse

try:
    from .config import ChatError, active_path
except ImportError:  # direct script execution during isolated adapter checks
    from config import ChatError, active_path

MAX_READ = 200
MAX_CATALOG = 100
_MESSAGE_TYPES = {1: "text", 3: "image", 34: "voice", 43: "video", 47: "emoji", 48: "location", 49: "file", 10000: "system", 10002: "revoke"}
_MEDIA_TYPES = {"image", "voice", "video", "emoji", "location", "file"}
_ZSTD_MAGIC = b"\x28\xb5\x2f\xfd"
_MAX_ZSTD_INPUT_BYTES = 4 * 1024 * 1024
_MAX_ZSTD_OUTPUT_BYTES = 1024 * 1024
_ZSTD_TIMEOUT_SECONDS = 2


class SnapshotError(ChatError):
    def __init__(self, code: str, message: str | None = None, metadata: dict[str, Any] | None = None):
        if message is None:
            message, code = code, "invalid-snapshot"
        super().__init__(code, message)
        self.metadata = metadata or {}


def _digest(value: Any) -> str:
    return hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def _anchor_digest(anchor: tuple[Any, ...]) -> str:
    """Hash SQLite values without passing bytes through JSON's default encoder."""
    normalized = [
        {"bytes_b64": base64.b64encode(value).decode("ascii")} if isinstance(value, bytes) else value
        for value in anchor
    ]
    return _digest(normalized)


def _now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def _root(binding: dict[str, Any]) -> Path:
    value = binding.get("snapshot_root")
    if not isinstance(value, str):
        raise SnapshotError("not-configured", "Chat snapshot binding is unavailable.")
    path = Path(value)
    if not path.is_absolute() or not path.is_dir():
        raise SnapshotError("invalid-binding", "Chat snapshot root must be an existing absolute directory.")
    return active_path(str(path))


def _within(root: Path, path: Path, label: str) -> Path:
    try:
        resolved = active_path(str(path), directory=path.is_dir())
        resolved.relative_to(root)
    except (OSError, ValueError) as exc:
        raise SnapshotError("invalid-binding", f"{label} escaped its snapshot root.") from exc
    return resolved


def _limit(args: dict[str, Any], maximum: int) -> int:
    value = args.get("limit", 50)
    if not isinstance(value, int) or isinstance(value, bool) or not 1 <= value <= maximum:
        raise SnapshotError("invalid-request", f"Limit must be an integer from 1 to {maximum}.")
    return value


def _cursor_encode(payload: dict[str, Any]) -> str:
    raw = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode()
    return base64.urlsafe_b64encode(raw).decode().rstrip("=")


def _cursor_decode(value: Any, expected: dict[str, str]) -> dict[str, Any]:
    if not isinstance(value, str) or not value or len(value) > 2048:
        raise SnapshotError("invalid-cursor", "Snapshot cursor is invalid.")
    try:
        raw = base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))
        item = json.loads(raw)
    except (ValueError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise SnapshotError("invalid-cursor", "Snapshot cursor is invalid.") from exc
    if not isinstance(item, dict) or any(item.get(k) != v for k, v in expected.items()):
        raise SnapshotError("invalid-cursor", "Snapshot cursor does not match this source.")
    return item


def _subject(provider: str, identity: str) -> str:
    namespace = "snapshot-conversation" if provider == "feishu" else "conversation"
    return f"chat://{provider}/{namespace}/{hashlib.sha256(identity.encode()).hexdigest()[:24]}"


def _directory_identity(root: Path) -> str:
    rows = []
    for path in sorted(root.rglob("*")):
        resolved = _within(root, path, "Snapshot entry")
        if resolved.is_file():
            stat = resolved.stat()
            rows.append((str(resolved.relative_to(root)), stat.st_size, stat.st_mtime_ns))
    return _digest(rows)


def _media(provider: str, subject: str, mid: str, raw: dict[str, Any], observed: str) -> tuple[list[str], list[dict[str, Any]]]:
    supplied = raw.get("media")
    if supplied is not None:
        if not isinstance(supplied, list):
            raise SnapshotError("invalid-snapshot", "Snapshot media must be a list.")
        refs, result = [], []
        for ordinal, item in enumerate(supplied):
            if not isinstance(item, dict):
                raise SnapshotError("invalid-snapshot", "Snapshot media item is invalid.")
            kind, locator, alt_text = item.get("type"), item.get("locator"), item.get("alt_text")
            if kind not in _MEDIA_TYPES or not isinstance(locator, str) or not locator or not isinstance(alt_text, str):
                raise SnapshotError("invalid-snapshot", "Snapshot media item has an invalid type or locator.")
            locator_digest = hashlib.sha256(locator.encode()).hexdigest()
            media_id = hashlib.sha256(f"{subject}|{mid}|{ordinal}|{kind}|{locator_digest}".encode()).hexdigest()
            refs.append(media_id)
            result.append({"media_id": media_id, "parent_message_id": mid, "ordinal_in_message": ordinal, "media_type": kind, "mime_type": None, "source_locator": locator, "locator_digest": locator_digest, "capture_status": "unavailable", "blob_digest": None, "byte_size": None, "alt_text": alt_text, "observed_at": observed, "failure_reason": "snapshot metadata does not prove media bytes", "extensions": {}})
        return refs, result
    kind = str(raw.get("type") or "text")
    if kind not in _MEDIA_TYPES:
        return [], []
    locator = f"{provider}-snapshot://message/{mid}/{kind}/0"
    locator_digest = hashlib.sha256(locator.encode()).hexdigest()
    media_id = hashlib.sha256(f"{subject}|{mid}|0|{kind}|{locator_digest}".encode()).hexdigest()
    return [media_id], [{"media_id": media_id, "parent_message_id": mid, "ordinal_in_message": 0, "media_type": kind, "mime_type": None, "source_locator": locator, "locator_digest": locator_digest, "capture_status": "unavailable", "blob_digest": None, "byte_size": None, "alt_text": "", "observed_at": observed, "failure_reason": "snapshot metadata does not prove media bytes", "extensions": {}}]


def _observation(provider: str, identity: str, name: str, raw_messages: list[dict[str, Any]], completeness: str, pagination: dict[str, Any] | None = None) -> dict[str, Any]:
    observed = _now()
    subject = _subject(provider, identity)
    messages, media = [], []
    for index, raw in enumerate(raw_messages):
        mid = str(raw.get("id") or "")
        id_status = "provider" if mid else "synthetic"
        if not mid:
            mid = "synthetic-" + hashlib.sha256(f"{identity}|{raw.get('time')}|{raw.get('text')}|{index}".encode()).hexdigest()[:24]
        refs, items = _media(provider, subject, mid, raw, observed)
        media.extend(items)
        text = str(raw.get("text") or "")
        msg = {"message_id": mid, "revision": int(raw.get("revision") or 1), "sent_at": raw.get("time"), "edited_at": raw.get("edited_at"), "sender": {"display_name": str(raw.get("sender") or ""), "ref": raw.get("sender_ref"), "is_self": raw.get("is_self")}, "message_type": "mixed" if refs and text else str(raw.get("type") or "text"), "text": text, "deleted": bool(raw.get("deleted")), "reply_to": [], "media_refs": refs, "source_anchor": {"provider_ref": f"{provider}-snapshot://message/{mid}", "observed_at": observed}, "extensions": {"id_status": id_status, "id_origin": "snapshot-provider-id" if id_status == "provider" else "snapshot-synthetic-id", "source_mode": "snapshot"}}
        msg["content_digest"] = _digest(msg)
        messages.append(msg)
    source = dict((pagination or {}).pop("source", {}))
    source.setdefault("mode", "snapshot")
    source.setdefault("identity_kind", {"feishu": "snapshot-chat-id", "teams": "teams-url-conversation-id", "wechat": "wechat-local-conversation-id"}[provider])
    source.setdefault("conversation_id", identity)
    capture = {"schema": 1, "kind": "conversation-capture", "provider_id": provider, "subject_ref": subject, "observed_at": observed, "completeness": completeness, "sensitivity": "private-communication", "source_window": {"limit": len(messages), "start": messages[0]["sent_at"] if messages else None, "end": messages[-1]["sent_at"] if messages else None, "boundary_status": "known" if completeness == "bounded-window" else "unknown"}, "messages": messages, "media": media, "extensions": {"source": source, **({"pagination": pagination} if pagination else {})}}
    capture["capture_id"] = _digest({"provider_id": provider, "subject_ref": subject, "observed_at": observed, "message_digests": [m["content_digest"] for m in messages], "media_locator_digests": [m["locator_digest"] for m in media]})
    capture["content_digest"] = _digest({k: v for k, v in capture.items() if k != "content_digest"})
    content = {"conversation_name": name, "messages": raw_messages, "capture": capture}
    if provider == "teams" and pagination and isinstance(pagination.get("snapshot_observed_at"), str):
        content["snapshot_observed_at"] = pagination["snapshot_observed_at"]
    return {"schema": 1, "provider_id": provider, "source_kind": "conversation", "subject_ref": subject, "observed_at": observed, "completeness": completeness, "sensitivity": "private-communication", "delivery": "ephemeral-stdout", "item_count": len(messages), "content_digest": _digest(content), "content": content}


def _feishu_captures(root: Path) -> tuple[list[dict[str, Any]], dict[str, list[dict[str, Any]]]]:
    entries: dict[str, dict[str, Any]] = {}
    captures: dict[str, list[dict[str, Any]]] = {}
    for path in sorted(root.glob("*/captures/*.json")):
        path = _within(root, path, "Feishu snapshot file")
        try:
            item = json.loads(path.read_text(encoding="utf-8"))
            if not isinstance(item, dict) or item.get("schema_version") != 1 or item.get("kind") != "chat_export_capture" or item.get("provider") != "feishu" or not isinstance(item.get("chat"), dict) or not isinstance(item.get("messages"), list):
                raise SnapshotError("invalid-snapshot", "Feishu snapshot capture schema is invalid.")
            chat = item.get("chat", {})
            ident, name = str(chat.get("id") or ""), str(chat.get("name") or "")
        except (OSError, json.JSONDecodeError, AttributeError) as exc:
            raise SnapshotError("invalid-snapshot", "Feishu snapshot metadata is invalid.") from exc
        captures.setdefault(ident, []).append(item)
        if ident and name:
            entries[ident] = {"conversationId": ident, "conversationName": name, "identityKind": "snapshot-chat-id"}
    return sorted(entries.values(), key=lambda e: (e["conversationName"].casefold(), e["conversationId"])), captures


def _feishu_catalog(root: Path) -> list[dict[str, Any]]:
    entries, _ = _feishu_captures(root)
    return entries


def _feishu_times(messages: list[dict[str, Any]], capture_date: str) -> list[str | None]:
    try:
        reference = datetime.strptime(capture_date[:10], "%Y-%m-%d").date()
    except ValueError as exc:
        raise SnapshotError("invalid-snapshot", "Feishu capture date is invalid.") from exc
    def explicit(label: str) -> date | None:
        label = label.strip()
        if "今天" in label: return reference
        if "昨天" in label: return reference - timedelta(days=1)
        if "前天" in label: return reference - timedelta(days=2)
        found = re.search(r"(?:(?P<year>\d{4})[-年])?(?P<month>\d{1,2})[-月](?P<day>\d{1,2})", label)
        if not found: return None
        try:
            value = date(int(found.group("year") or reference.year), int(found.group("month")), int(found.group("day")))
            return value if found.group("year") or value <= reference + timedelta(days=1) else date(value.year - 1, value.month, value.day)
        except ValueError: return None
    dates = [explicit(str(m.get("time_label") or "")) for m in messages]
    following, current, previous, result = [None] * len(messages), None, None, []
    next_date = None
    for index in range(len(messages) - 1, -1, -1):
        if dates[index] is not None: next_date = dates[index]
        following[index] = next_date
    for index, item in enumerate(messages):
        label, exact = str(item.get("time_label") or ""), dates[index]
        clock = re.search(r"(?<!\d)([01]?\d|2[0-3]):([0-5]\d)(?!\d)", label)
        clock_value = (int(clock.group(1)), int(clock.group(2))) if clock else None
        if exact is not None: current, previous = exact, clock_value
        else:
            if current is None: current = following[index] or reference
            elif clock_value is not None and previous is not None and clock_value < previous and current + timedelta(days=1) <= reference: current += timedelta(days=1)
            if clock_value is not None: previous = clock_value
        result.append(datetime(current.year, current.month, current.day, *clock_value, tzinfo=timezone(timedelta(hours=8))).isoformat() if current and clock_value else None)
    return result


def _feishu_read(root: Path, args: dict[str, Any], limit: int) -> tuple[str, str, list[dict[str, Any]], str, dict[str, Any] | None]:
    wanted_id, wanted_name = args.get("conversationId"), args.get("conversationName")
    if args.get("cursor"):
        raise SnapshotError("cursor-unsupported", "Feishu snapshots do not support cursor.")
    if args.get("conversationUrl") is not None:
        raise SnapshotError("invalid-request", "Feishu snapshots do not accept conversation URL.")
    if wanted_id is not None and not isinstance(wanted_id, str) or wanted_name is not None and not isinstance(wanted_name, str) or not wanted_id and not wanted_name:
        raise SnapshotError("invalid-request", "Feishu read needs an exact conversation id or name.")
    entries, captures = _feishu_captures(root)
    matches = [e for e in entries if (not wanted_id or e["conversationId"] == wanted_id) and (not wanted_name or e["conversationName"] == wanted_name)]
    if len(matches) != 1:
        code = "ambiguous-conversation" if len(matches) > 1 else "conversation-not-found"
        raise SnapshotError(code, "Feishu snapshot conversation is missing or ambiguous.", {"candidates": [{"conversationId": item["conversationId"], "conversationName": item["conversationName"], "identityKind": item["identityKind"]} for item in matches]} if code == "ambiguous-conversation" else None)
    chosen, all_rows = matches[0], []
    latest_capture_date = None
    for item in captures[chosen["conversationId"]]:
        capture_date = str(item.get("capture_date") or "")
        latest_capture_date = max(latest_capture_date or capture_date, capture_date)
        times = _feishu_times(item["messages"], capture_date)
        updated_at = str(item.get("updated_at") or "")
        for index, (raw, normalized) in enumerate(zip(item["messages"], times)):
            if not isinstance(raw, dict) or not raw.get("id"):
                raise SnapshotError("invalid-snapshot", "Feishu snapshot message has no stable id.")
            all_rows.append((capture_date, updated_at, index, {"id": str(raw["id"]), "sender": raw.get("sender_raw") or raw.get("sender"), "time": normalized, "type": raw.get("type") or "text", "text": raw.get("content") or "", "is_self": bool(raw.get("is_self")), "revision": raw.get("revision"), "edited_at": raw.get("edited_at")}))
    # A later capture is authoritative for a message revision, independent of
    # the directory's lexical order; then choose the newest message window.
    unique = {row[3]["id"]: row for row in sorted(all_rows, key=lambda row: (row[0], row[1], row[2]))}
    rows = [row[3] for row in sorted(unique.values(), key=lambda row: (row[3]["time"] or "", row[0], row[1], row[2], row[3]["id"]))[-limit:]]
    return chosen["conversationId"], chosen["conversationName"], rows, "bounded-window", {"available_message_count": len(unique), "returned": len(rows), "truncated": len(unique) > limit, "selection": "bounded-latest", "cursor_supported": False, "source": {"mode": "snapshot", "identity_kind": "snapshot-chat-id", "conversation_id": chosen["conversationId"], "snapshot_observed_at": latest_capture_date}}


def _teams_catalog(root: Path) -> list[dict[str, Any]]:
    entries = []
    for directory in sorted(p for p in root.iterdir() if p.is_dir()):
        directory = _within(root, directory, "Teams conversation directory")
        state, messages = directory / "state.json", directory / "messages.jsonl"
        if not state.is_file() or not messages.is_file():
            continue
        try:
            item = json.loads(_within(root, state, "Teams state").read_text(encoding="utf-8"))
            url = str(item["chat_url"])
            ident = _teams_id(url)
        except (KeyError, TypeError, ValueError, OSError, json.JSONDecodeError) as exc:
            raise SnapshotError("invalid-snapshot", "Teams snapshot state is invalid.") from exc
        entries.append({"conversationId": ident, "conversationName": str(item.get("chat_name") or directory.name), "conversationUrl": url, "identityKind": "teams-url-conversation-id"})
    return entries


def _teams_id(url: str) -> str:
    parsed = urlparse(url)
    if parsed.scheme != "https" or parsed.hostname not in {"teams.microsoftonline.cn", "teams.microsoft.com", "teams.cloud.microsoft"} or parsed.username or parsed.password:
        raise SnapshotError("invalid-request", "Teams conversation URL is invalid.")
    pieces = [unquote(x) for x in parsed.path.split("/")]
    try: return pieces[pieces.index("chat") + 1]
    except (ValueError, IndexError): raise SnapshotError("invalid-request", "Teams URL has no exact conversation id.")


def _teams_read(root: Path, args: dict[str, Any], limit: int) -> tuple[str, str, list[dict[str, Any]], str, dict[str, Any] | None]:
    wanted_id, wanted_url, wanted_name = args.get("conversationId"), args.get("conversationUrl"), args.get("conversationName")
    if args.get("cursor"):
        raise SnapshotError("cursor-unsupported", "Teams snapshots do not support cursor.")
    if not any((wanted_id, wanted_url, wanted_name)) or any(value is not None and not isinstance(value, str) for value in (wanted_id, wanted_url, wanted_name)):
        raise SnapshotError("invalid-request", "Teams read needs exact conversation identity.")
    entries = _teams_catalog(root)
    chosen = [e for e in entries if (not wanted_id or wanted_id == e["conversationId"]) and (not wanted_url or wanted_url == e["conversationUrl"]) and (not wanted_name or wanted_name == e["conversationName"])]
    if len(chosen) != 1: raise SnapshotError("ambiguous-conversation" if len(chosen) > 1 else "conversation-not-found", "Teams snapshot conversation is missing or ambiguous.", {"candidates": chosen} if len(chosen) > 1 else None)
    chosen = chosen[0]
    state_path = None
    for candidate in root.iterdir():
        if not candidate.is_dir():
            continue
        directory = _within(root, candidate, "Teams conversation directory")
        if not (directory / "state.json").is_file():
            continue
        possible_state = _within(root, directory / "state.json", "Teams state")
        possible = json.loads(possible_state.read_text(encoding="utf-8"))
        if _teams_id(str(possible.get("chat_url") or "")) == chosen["conversationId"] and possible.get("chat_url") == chosen["conversationUrl"] and str(possible.get("chat_name") or directory.name) == chosen["conversationName"]:
            state_path = possible_state
            break
    if state_path is None:
        raise SnapshotError("conversation-not-found", "Teams snapshot conversation was not found.")
    directory = state_path.parent
    state = json.loads(state_path.read_text(encoding="utf-8")); message_path = _within(root, directory / "messages.jsonl", "Teams messages")
    rows = []
    message_lines = [line for line in message_path.read_text(encoding="utf-8").split("\n") if line.strip()]
    mids = set()
    for line in message_lines:
        raw = json.loads(line)
        if not isinstance(raw, dict):
            raise SnapshotError("invalid-snapshot", "Teams snapshot contains an invalid item.")
        mid = raw.get("mid")
        if not isinstance(mid, str) or not mid or mid in mids:
            raise SnapshotError("invalid-snapshot", "Teams snapshot message id is missing or duplicated.")
        mids.add(mid)
        text = raw.get("body")
        nontext = raw.get("has_only_nontext") is True
        media = raw.get("media", [])
        if not isinstance(media, list):
            raise SnapshotError("invalid-snapshot", "Teams snapshot media must be a list.")
        normalized_media = []
        for item in media:
            if not isinstance(item, dict):
                raise SnapshotError("invalid-snapshot", "Teams snapshot media item is invalid.")
            kind, locator, alt_text = item.get("type"), item.get("locator"), item.get("alt_text")
            if kind not in _MEDIA_TYPES or not isinstance(locator, str) or not locator or not isinstance(alt_text, str):
                raise SnapshotError("invalid-snapshot", "Teams snapshot media item has an invalid type or locator.")
            normalized_media.append({"type": kind, "locator": locator, "alt_text": alt_text})
        rows.append({"id": mid, "sender": raw.get("author") or "", "time": raw.get("time_iso") or None, "type": "unknown" if nontext else "text", "text": text.strip() if isinstance(text, str) else "", "is_self": False, "media": normalized_media})
    if state.get("total_count") != len(message_lines): raise SnapshotError("invalid-snapshot", "Teams state does not match messages.")
    if not isinstance(state.get("last_export_at"), str):
        raise SnapshotError("invalid-snapshot", "Teams snapshot state has no observation time.")
    completeness = "bounded-window" if state.get("increment_complete") is True and state.get("hit_known_boundary") is True else "unknown-or-truncated"
    return chosen["conversationId"], chosen["conversationName"], rows[-limit:], completeness, {"available_message_count": len(rows), "returned": min(len(rows), limit), "truncated": len(rows) > limit, "selection": "bounded-latest", "cursor_supported": False, "snapshot_observed_at": state.get("last_export_at"), "source": {"mode": "snapshot", "identity_kind": "teams-url-conversation-id", "conversation_id": chosen["conversationId"], "snapshot_observed_at": state.get("last_export_at")}}


def _wechat_dbs(root: Path) -> list[Path]:
    directory = root / "message"
    if not directory.is_dir(): return []
    return [_within(root, p, "WeChat message database") for p in sorted(directory.glob("message_[0-9]*.db"))]


def _file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _wechat_names(root: Path) -> dict[str, str]:
    import sqlite3
    path = root / "contact" / "contact.db"
    if not path.is_file(): return {}
    path = _within(root, path, "WeChat contact database")
    result = {}
    with sqlite3.connect(f"file:{path}?mode=ro", uri=True) as con:
        for table in ("contact", "stranger"):
            try:
                for username, remark, nickname in con.execute(f"SELECT username, remark, nick_name FROM {table}"):
                    if username and username not in result: result[str(username)] = str(remark or nickname or username)
            except sqlite3.Error: pass
    return result


def _stable_database_identity(path: Path, inventory: dict[Path, str] | None = None) -> str:
    before, content = path.stat(), _file_hash(path)
    after = path.stat()
    fields = ("st_dev", "st_ino", "st_size", "st_mtime_ns", "st_ctime_ns")
    if tuple(getattr(before, field) for field in fields) != tuple(getattr(after, field) for field in fields):
        raise SnapshotError("snapshot-changed", "WeChat snapshot changed during bounded read.")
    for suffix in ("-wal", "-shm"):
        if path.with_name(path.name + suffix).exists():
            raise SnapshotError("snapshot-unstable", "WeChat snapshot has live SQLite sidecars.")
    if inventory is not None:
        inventory[path] = content
    return _digest({"content": content, "stat": [getattr(after, field) for field in fields]})


def _wechat_shards(root: Path, table: str) -> list[tuple[int, Path]]:
    """Return each message shard which actually owns the requested table."""
    import sqlite3
    shards = []
    for db in _wechat_dbs(root):
        match = re.fullmatch(r"message_(\d+)\.db", db.name)
        if match is None:
            continue
        with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as con:
            if con.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (table,)).fetchone():
                shards.append((int(match.group(1)), db))
    return shards


def _wechat_snapshot_identity(databases: list[Path], inventory: dict[Path, str] | None = None) -> str:
    """Bind cursors to the complete published message database generation."""
    return _digest([(path.name, _stable_database_identity(path, inventory)) for path in databases])


_STAT_FIELDS = ("st_dev", "st_ino", "st_size", "st_mtime_ns", "st_ctime_ns")
_WECHAT_METADATA = ("session/session.db", "contact/contact.db", "head_image/head_image.db")


def _wechat_generation_signature(root: Path, databases: list[Path]) -> list[tuple[str, tuple[int, ...] | None]]:
    """Stat-only identity of exactly the files the generation identity hashes.

    The content digest already binds each file's device, inode, size, mtime and
    ctime, so an unchanged signature after a bounded read proves the same
    generation without streaming every database a second time.
    """
    rows: list[tuple[str, tuple[int, ...] | None]] = []
    for path in databases:
        rows.append((path.name, _stat_fields(path)))
    for relative in _WECHAT_METADATA:
        path = root / relative
        if path.exists() and not path.is_file():
            raise SnapshotError("invalid-snapshot", "WeChat snapshot metadata database is invalid.")
        # Same symlink and root-escape guard as the content hash path.
        rows.append((relative, _stat_fields(_within(root, path, "WeChat metadata database")) if path.is_file() else None))
    return rows


def _stat_fields(path: Path) -> tuple[int, ...]:
    for suffix in ("-wal", "-shm"):
        if path.with_name(path.name + suffix).exists():
            raise SnapshotError("snapshot-unstable", "WeChat snapshot has live SQLite sidecars.")
    info = path.stat()
    return tuple(getattr(info, field) for field in _STAT_FIELDS)


def _wechat_generation_identity(root: Path, databases: list[Path], inventory: dict[Path, str] | None = None) -> str:
    """Include related published databases, even if message bytes are unchanged."""
    metadata = []
    for relative in _WECHAT_METADATA:
        path = root / relative
        if path.exists() and not path.is_file():
            raise SnapshotError("invalid-snapshot", "WeChat snapshot metadata database is invalid.")
        metadata.append((relative, _stable_database_identity(_within(root, path, "WeChat metadata database"), inventory) if path.is_file() else None))
    return _digest({"messages": _wechat_snapshot_identity(databases, inventory), "metadata": metadata})


def _zstd_decode(blob: bytes) -> str:
    if len(blob) > _MAX_ZSTD_INPUT_BYTES:
        raise SnapshotError("message-decode-failed", "Compressed WeChat text exceeds the bounded input size.")
    try:
        import resource
        limit = _MAX_ZSTD_OUTPUT_BYTES
        def restrict_output() -> None:
            resource.setrlimit(resource.RLIMIT_FSIZE, (limit, limit))
    except (ImportError, AttributeError):  # pragma: no cover - supported deployment is POSIX
        restrict_output = None
    with tempfile.TemporaryDirectory(prefix="opsail-wechat-zstd-") as directory:
        source, destination = Path(directory) / "message.zst", Path(directory) / "message.txt"
        source.write_bytes(blob)
        try:
            completed = subprocess.run(
                ["zstd", "--quiet", "--decompress", "--force", "-o", str(destination), str(source)],
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                timeout=_ZSTD_TIMEOUT_SECONDS, check=False, preexec_fn=restrict_output,
            )
        except FileNotFoundError as exc:
            raise SnapshotError("decoder-unavailable", "WeChat compressed text decoder is unavailable.") from exc
        except subprocess.TimeoutExpired as exc:
            raise SnapshotError("message-decode-failed", "Compressed WeChat text exceeded the decode time limit.") from exc
        if completed.returncode != 0 or not destination.is_file():
            raise SnapshotError("message-decode-failed", "Compressed WeChat text could not be decoded safely.")
        if destination.stat().st_size > _MAX_ZSTD_OUTPUT_BYTES:
            raise SnapshotError("message-decode-failed", "Compressed WeChat text exceeds the bounded output size.")
        try:
            return destination.read_text(encoding="utf-8")
        except UnicodeDecodeError as exc:
            raise SnapshotError("message-decode-failed", "Compressed WeChat text is not valid UTF-8.") from exc


def _decode_wechat_text(content: Any) -> str:
    if content is None:
        return ""
    if isinstance(content, str):
        return content
    if not isinstance(content, bytes):
        return str(content)
    if content.startswith(_ZSTD_MAGIC):
        return _zstd_decode(content)
    try:
        return content.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise SnapshotError("message-decode-failed", "WeChat text is not valid UTF-8.") from exc


def _wechat_catalog(root: Path) -> list[dict[str, Any]]:
    import sqlite3
    session = _within(root, root / "session" / "session.db", "WeChat session database")
    tables = set()
    for db in _wechat_dbs(root):
        with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as con: tables.update(str(r[0]) for r in con.execute("SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'Msg_%'"))
    with sqlite3.connect(f"file:{session}?mode=ro", uri=True) as con: ids = [str(r[0]) for r in con.execute("SELECT username FROM SessionTable WHERE username IS NOT NULL AND username != '' ORDER BY username")]
    names = _wechat_names(root)
    return [{"conversationId": ident, "conversationName": names.get(ident, ident), "identityKind": "wechat-local-conversation-id"} for ident in ids if "Msg_" + hashlib.md5(ident.encode()).hexdigest() in tables]


def _wechat_read(root: Path, args: dict[str, Any], limit: int) -> tuple[str, str, list[dict[str, Any]], str, dict[str, Any] | None]:
    import sqlite3
    ident, wanted_name, wanted_url = args.get("conversationId"), args.get("conversationName"), args.get("conversationUrl")
    if not isinstance(ident, str) or not ident or wanted_url is not None or wanted_name is not None and not isinstance(wanted_name, str): raise SnapshotError("invalid-request", "WeChat read needs one exact conversation id.")
    names = _wechat_names(root)
    if wanted_name is not None and wanted_name != names.get(ident, ident): raise SnapshotError("identity-mismatch", "WeChat conversation identifiers disagree.")
    table = "Msg_" + hashlib.md5(ident.encode()).hexdigest()
    databases = _wechat_dbs(root)
    shards = _wechat_shards(root, table)
    if not shards: raise SnapshotError("conversation-not-found", "WeChat snapshot conversation was not found.")
    inventory = {}
    expected = {"p": "wechat", "s": ident, "d": _wechat_generation_identity(root, databases, inventory)}
    generation = _wechat_generation_signature(root, databases)
    cursor = _cursor_decode(args["cursor"], expected) if args.get("cursor") else None
    before = cursor.get("b") if cursor else None
    if before is not None and (not isinstance(before, list) or len(before) != 3 or not all(isinstance(x, int) for x in before) or not isinstance(cursor.get("a"), str)):
        raise SnapshotError("invalid-cursor", "Snapshot cursor is invalid.")
    if before is not None:
        anchor_path = next((path for shard, path in shards if shard == before[1]), None)
        if anchor_path is None: raise SnapshotError("snapshot-changed", "Cursor shard no longer matches this snapshot.")
        with sqlite3.connect(f"file:{anchor_path}?mode=ro", uri=True) as con:
            anchor = con.execute(f"SELECT local_id, local_type, create_time, real_sender_id, CASE WHEN (local_type & 4294967295) = 1 THEN message_content ELSE '' END FROM [{table}] WHERE create_time = ? AND local_id = ?", (before[0], before[2])).fetchone()
        if anchor is None or _anchor_digest((before[1], *anchor)) != cursor["a"]: raise SnapshotError("snapshot-changed", "Cursor anchor no longer matches this snapshot.")
    rows, sender_map = [], {}
    for shard, database in shards:
        if before is None: where, params = "", []
        elif shard < before[1]: where, params = "WHERE create_time <= ? ", [before[0]]
        elif shard == before[1]: where, params = "WHERE (create_time < ? OR (create_time = ? AND local_id < ?)) ", [before[0], before[0], before[2]]
        else: where, params = "WHERE create_time < ? ", [before[0]]
        with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as con:
            try: sender_map.update({(shard, int(rowid)): str(username) for rowid, username in con.execute("SELECT rowid, user_name FROM Name2Id WHERE user_name != ''")})
            except sqlite3.Error: pass
            rows.extend((int(created or 0), shard, int(local_id), local_type, sender_id, content) for local_id, local_type, created, sender_id, content in con.execute(f"SELECT local_id, local_type, create_time, real_sender_id, CASE WHEN (local_type & 4294967295) = 1 THEN message_content ELSE '' END FROM [{table}] {where}ORDER BY create_time DESC, local_id DESC LIMIT ?", (*params, limit + 1)))
    rows.sort(key=lambda row: (row[0], row[1], row[2]), reverse=True)
    more = len(rows) > limit; rows = list(reversed(rows[:limit]))
    if _wechat_dbs(root) != databases or _wechat_generation_signature(root, databases) != generation:
        raise SnapshotError("snapshot-changed", "WeChat snapshot changed during bounded read.")
    messages = []
    group = ident.endswith("@chatroom")
    for created, shard, local_id, local_type, sender_id, content in rows:
        kind = _MESSAGE_TYPES.get(int(local_type) & 0xffffffff, "unknown")
        text = _decode_wechat_text(content) if kind == "text" else ""
        sender = text.split(":\n", 1)[0] if group and ":\n" in text else sender_map.get((shard, int(sender_id)), "") if sender_id not in (None, "") else ""
        if group and ":\n" in text: text = text.split(":\n", 1)[1]
        messages.append({"id": f"{shard}:{local_id}", "sender": names.get(sender, sender), "sender_ref": sender, "time": datetime.fromtimestamp(created, timezone.utc).isoformat().replace("+00:00", "Z") if created else None, "type": kind, "text": text if kind == "text" else f"[{kind}]", "is_self": None, "deleted": kind == "revoke"})
    page = {"page_size": limit, "returned": len(messages), "has_more": more}
    if more and rows:
        anchor = rows[0]
        page["next_cursor"] = _cursor_encode({**expected, "b": [anchor[0], anchor[1], anchor[2]], "a": _anchor_digest((anchor[1], anchor[2], anchor[3], anchor[0], anchor[4], anchor[5]))})
    latest_mtime = max(path.stat().st_mtime for _, path in shards)
    page["source"] = {"mode": "snapshot", "identity_kind": "wechat-local-conversation-id", "conversation_id": ident, "snapshot_observed_at": datetime.fromtimestamp(latest_mtime, timezone.utc).isoformat().replace("+00:00", "Z"), "snapshot_observed_at_kind": "filesystem-mtime"}
    from .freshness import verified_generation_receipt
    receipt = verified_generation_receipt(root, [{"relative": path.relative_to(root).as_posix(), "sha256": digest} for path, digest in inventory.items()])
    if receipt is not None:
        page["source"].update(snapshot_observed_at=receipt["checked_at"], snapshot_observed_at_kind="verified-sync-receipt")
    return ident, names.get(ident, ident), messages, "bounded-window", page


def catalog(provider: str, binding: dict[str, Any], args: dict[str, Any]) -> dict[str, Any]:
    root, limit = _root(binding), _limit(args, MAX_CATALOG)
    readers = {"feishu": _feishu_catalog, "teams": _teams_catalog, "wechat": _wechat_catalog}
    if provider not in readers: raise SnapshotError("unsupported-provider", "Unsupported chat snapshot provider.")
    entries = readers[provider](root)
    expected = {"p": provider, "d": _directory_identity(root)}
    cursor = _cursor_decode(args["cursor"], expected) if args.get("cursor") else None
    offset = cursor.get("o", 0) if cursor else 0
    if not isinstance(offset, int) or offset < 0 or offset > len(entries):
        raise SnapshotError("invalid-cursor", "Snapshot catalog cursor is invalid.")
    page = entries[offset:offset + limit]
    next_offset = offset + len(page)
    return {"entries": page, "nextCursor": _cursor_encode({**expected, "o": next_offset}) if next_offset < len(entries) else None, "complete": next_offset >= len(entries)}


def read(provider: str, binding: dict[str, Any], args: dict[str, Any]) -> dict[str, Any]:
    root, limit = _root(binding), _limit(args, MAX_READ)
    readers = {"feishu": _feishu_read, "teams": _teams_read, "wechat": _wechat_read}
    if provider not in readers: raise SnapshotError("unsupported-provider", "Unsupported chat snapshot provider.")
    ident, name, rows, completeness, pagination = readers[provider](root, args, limit)
    return _observation(provider, ident, name, rows, completeness, pagination)
