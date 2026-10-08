"""Read-only freshness metadata. Never serialize bindings or message bodies."""
from __future__ import annotations

import hashlib
import json
import os
import re
import stat
from datetime import datetime, timedelta, timezone
from pathlib import Path

from .config import ChatError, active_path

_SHARD = re.compile(r"message_[0-9]+\.db\Z")
_TABLE = re.compile(r"Msg_[0-9a-f]+\Z")
_META = ("contact/contact.db", "session/session.db", "head_image/head_image.db")


def _error():
    return ChatError("snapshot-metadata-unavailable", "Snapshot freshness could not be verified.")


def _stamp(value):
    if not isinstance(value, str) or len(value) > 64:
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
        if parsed.tzinfo is None:
            parsed = parsed.replace(tzinfo=timezone(timedelta(hours=8)))
        return parsed.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")
    except (ValueError, OverflowError):
        return None


def _signature(path):
    info = path.stat()
    return info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns


def _inside(root, path):
    resolved = active_path(str(path), directory=False)
    if not resolved.is_relative_to(root):
        raise _error()
    return resolved


def _sha(path):
    before = _signature(path)
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(chunk)
    if _signature(path) != before:
        raise _error()
    return result.hexdigest()


def _json(root, path, *, private=False):
    path = _inside(root, path)
    info = path.stat()
    if info.st_size > 1024 * 1024:
        raise _error()
    if private and (stat.S_IMODE(info.st_mode) != 0o600 or info.st_uid != os.getuid()):
        raise _error()
    before = _signature(path)
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict) or _signature(path) != before:
        raise _error()
    return value


def _custody(root):
    if root.name == "decrypted":
        if root.parent.parent.name == "generations" and root.parent.parent.parent.name == "sync":
            return root.parent.parent.parent.parent
        return root.parent
    return None


def verified_generation_receipt(root: Path, inventory: list[dict]) -> dict | None:
    """Bind receipt time to an inventory already hashed by the bounded reader."""
    custody = _custody(root)
    if custody is None or root.parent.parent.name != "generations":
        return None
    receipt = _json(custody, root.parent / "receipt.json")
    rows = receipt.get("database_stats")
    if receipt.get("schema_version") != 1 or not isinstance(rows, list) or not all(isinstance(row, dict) for row in rows):
        raise _error()
    recorded = [{"relative": row.get("relative"), "sha256": row.get("sha256")} for row in rows]
    inventory = sorted(inventory, key=lambda row: row["relative"])
    if sorted(recorded, key=lambda row: str(row["relative"])) != inventory:
        raise _error()
    digest = hashlib.sha256()
    for row in inventory:
        digest.update(row["relative"].encode()); digest.update(b"\0"); digest.update(row["sha256"].encode())
    version = receipt.get("snapshot_version")
    if not _stamp(receipt.get("checked_at")) or version != digest.hexdigest() or receipt.get("snapshot_digest") != version or root.parent.name != version:
        raise _error()
    return receipt


def _wechat(root, output):
    import sqlite3

    shard_paths = sorted(path for path in (root / "message").glob("*.db") if _SHARD.fullmatch(path.name))
    if not shard_paths:
        raise _error()
    database_paths = shard_paths + [root / name for name in _META if (root / name).is_file()]
    signatures = {path: _signature(_inside(root, path)) for path in database_paths}
    inventory, latest = [], []
    for path in sorted(database_paths):
        if any(path.with_name(path.name + suffix).exists() for suffix in ("-wal", "-shm")):
            raise _error()
        maximum = None
        with sqlite3.connect(path.as_uri() + "?mode=ro", uri=True) as connection:
            if path in shard_paths:
                tables = [row[0] for row in connection.execute("SELECT name FROM sqlite_master WHERE type='table'") if _TABLE.fullmatch(row[0])]
                for table in tables:
                    value = connection.execute(f"SELECT MAX(create_time) FROM [{table}]").fetchone()[0]
                    if isinstance(value, (int, float)) and value > 0:
                        maximum = max(maximum or value, value)
            else:
                connection.execute("SELECT COUNT(*) FROM sqlite_master").fetchone()
        if maximum is not None:
            latest.append(datetime.fromtimestamp(maximum, timezone.utc).isoformat().replace("+00:00", "Z"))
        inventory.append({"relative": path.relative_to(root).as_posix(), "sha256": _sha(path)})
    if any(_signature(path) != before for path, before in signatures.items()):
        raise _error()
    # This inventory identity also works for legacy snapshots without a receipt.
    digest = hashlib.sha256()
    for row in inventory:
        digest.update(row["relative"].encode()); digest.update(b"\0"); digest.update(row["sha256"].encode())
    output["snapshotVersion"] = digest.hexdigest()
    output["latestMessageAt"] = max(latest) if latest else None
    times = [datetime.fromtimestamp(path.stat().st_mtime, timezone.utc).isoformat().replace("+00:00", "Z") for path in database_paths]
    output.update(snapshotCapturedAt=max(times), oldestCaptureAt=min(times), snapshotTimeKind="filesystem-mtime")
    custody = _custody(root)
    expected = None
    if custody and (custody / "sync/binding.json").is_file():
        binding = _json(custody, custody / "sync/binding.json", private=True)
        if binding.get("schema_version") != 1 or not isinstance(binding.get("source_root"), str):
            raise _error()
        source = active_path(binding["source_root"])
        expected = sorted(path.name for path in (source / "message").glob("*.db") if _SHARD.fullmatch(path.name))
    readable = sorted(path.name for path in shard_paths)
    output["coverage"] = {"expectedMessageShards": len(expected) if expected is not None else None,
                          "readableMessageShards": len(readable), "databaseCount": len(database_paths),
                          "complete": set(readable) == set(expected) and len(database_paths) == len(readable) + len(_META) if expected is not None else None}
    receipt = verified_generation_receipt(root, inventory)
    if receipt is not None:
        captured = _stamp(receipt.get("checked_at"))
        version = receipt.get("snapshot_version")
        output.update(snapshotCapturedAt=captured, oldestCaptureAt=captured,
                      snapshotTimeKind="verified-sync-receipt", snapshotVersion=version,
                      lastSuccessfulSyncAt=_stamp(receipt.get("last_successful_sync_at")))
    if custody and (custody / "sync/receipts/latest-attempt.json").is_file():
        attempt = _json(custody, custody / "sync/receipts/latest-attempt.json")
        result, code = attempt.get("result"), attempt.get("code")
        if result not in {"success", "unchanged", "failed", "blocked", "published"} or code is not None and (not isinstance(code, str) or not re.fullmatch(r"[a-z][a-z0-9-]{0,79}", code)):
            raise _error()
        output["lastSyncResult"] = {"checkedAt": _stamp(attempt.get("checked_at")), "result": result, "code": code}
        if result in {"success", "unchanged", "published"} and attempt.get("snapshot_version") == output["snapshotVersion"] and output["snapshotTimeKind"] == "verified-sync-receipt":
            output["lastSuccessfulSyncAt"] = _stamp(attempt.get("last_successful_sync_at"))


def _json_source(provider, root, output):
    files = sorted(root.glob("*/state.json"))
    if not files or len(files) > 2000:
        return
    captures, latest, inventory = [], [], []
    for path in files:
        state = _json(root, path)
        captured = _stamp(state.get("last_export_at"))
        if captured:
            captures.append(captured)
        if provider == "teams":
            sent = _stamp(state.get("seen_max_time"))
            if sent:
                latest.append(sent)
        inventory.append((path.relative_to(root).as_posix(), _sha(path)))
    output.update(snapshotCapturedAt=max(captures) if captures else None,
                  oldestCaptureAt=min(captures) if captures else None,
                  snapshotTimeKind="provider-export-metadata" if captures else "unknown",
                  latestMessageAt=max(latest) if latest else None,
                  snapshotVersion=hashlib.sha256(json.dumps(inventory, separators=(",", ":")).encode()).hexdigest())


def snapshot_freshness(provider: str, binding: dict) -> dict:
    """Inspect a snapshot only. Never connect to a browser or start synchronization."""
    output = {"checkedAt": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
              "snapshotCapturedAt": None, "oldestCaptureAt": None, "snapshotTimeKind": "unknown",
              "lastSuccessfulSyncAt": None, "latestMessageAt": None, "snapshotVersion": None,
              "coverage": None, "lastSyncResult": None}
    try:
        root = active_path(binding["snapshot_root"])
        if provider == "wechat":
            import sqlite3
            try:
                _wechat(root, output)
            except sqlite3.Error as exc:
                raise _error() from exc
        else:
            _json_source(provider, root, output)
        return output
    except (OSError, ValueError, TypeError, KeyError, OverflowError) as exc:
        raise _error() from exc
