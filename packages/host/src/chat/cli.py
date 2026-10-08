#!/usr/bin/env python3
"""Single-request stdin/stdout adapter. No chat content or credentials are saved."""

from __future__ import annotations

import importlib.util
import json
import os
import platform
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from chat.config import ChatError, MODES, PROVIDERS, load_bindings, validate_binding

MAX_INPUT = 65536
MAX_OUTPUT = 16 * 1024 * 1024


def validate_request(request: dict) -> tuple[str, dict]:
    if not isinstance(request, dict) or set(request) != {"operation", "args"}:
        raise ChatError("invalid-request", "Expected one operation and an argument object.")
    operation, args = request["operation"], request["args"]
    if operation not in {"status", "catalog", "read"} or not isinstance(args, dict):
        raise ChatError("invalid-request", "Unsupported chat operation.")
    allowed = {"provider"} if operation == "status" else {"provider", "mode", "limit", "cursor"}
    if operation == "read":
        allowed |= {"conversationId", "conversationName", "conversationUrl"}
    if set(args) - allowed:
        raise ChatError("invalid-request", "Unsupported chat arguments.")
    if args.get("provider") not in PROVIDERS and not (operation == "status" and "provider" not in args):
        raise ChatError("invalid-request", "Select wechat, feishu, or teams.")
    if operation != "status":
        if args.get("mode") not in MODES:
            raise ChatError("invalid-request", "An explicit source mode is required.")
        limit = args.get("limit", 50)
        if type(limit) is not int or not 1 <= limit <= (100 if operation == "catalog" else 200):
            raise ChatError("invalid-request", "Requested page size is outside its bound.")
        for field in {"cursor", "conversationId", "conversationName", "conversationUrl"} & set(args):
            value = args[field]
            if not isinstance(value, str) or not value.strip() or len(value) > (8192 if field == "cursor" else 2048) or any(ord(c) < 32 for c in value):
                raise ChatError("invalid-request", "Invalid conversation selector or cursor.")
        if operation == "read" and not any(args.get(f) for f in ("conversationId", "conversationName", "conversationUrl")):
            raise ChatError("invalid-request", "An exact conversation selector is required.")
    return operation, args


def statuses(bindings: dict, selected: str | None) -> dict:
    rows = []
    for provider in ([selected] if selected else PROVIDERS):
        modes = {}
        for mode in MODES:
            row = {"configured": False, "available": False, "validation": "not-validated", "diagnostic": "not-configured"}
            if mode == "live-api":
                row.update(diagnostic="api-protocol-unverified", validation="unverified")
            try:
                binding = validate_binding(provider, bindings.get(provider, {}), mode)
                row["configured"] = True
                if mode == "snapshot":
                    row.update(available=True, diagnostic="snapshot-root-available")
                    from chat.freshness import snapshot_freshness
                    row["freshness"] = snapshot_freshness(provider, binding)
                    if row["freshness"]["coverage"] and row["freshness"]["coverage"]["complete"] is False:
                        row["diagnostic"] = "snapshot-coverage-incomplete"
                elif mode == "live-dom":
                    transport = binding["browser"]["transport"]
                    if transport == "chrome-extension":
                        from chat import live
                        extension = live.status(provider, binding)["liveDom"]
                        ready = extension["state"] == "ready"
                        row.update(available=ready, diagnostic="browser-session-ready" if ready else "browser-session-not-validated",
                                   extension={key: extension.get(key) for key in ("state", "reason", "transport", "targetConfigured", "capabilities", "diagnostics")})
                    else:
                        ready = platform.system() == "Darwin" if transport in {"edge-active-tab", "chrome-active-tab", "chrome-tab"} else importlib.util.find_spec("playwright") is not None
                        row.update(available=ready, diagnostic="browser-session-not-validated" if ready else "browser-dependency-unavailable")
            except (ChatError, OSError, ValueError) as error:
                row["diagnostic"] = error.code if isinstance(error, ChatError) else "source-unavailable"
                row["available"] = False
            if provider == "wechat" and mode != "snapshot":
                row.update(configured=False, available=False, diagnostic="mode-unavailable")
            modes[mode] = row
        rows.append({"provider": provider, "modes": modes})
    return {"providers": rows}


def execute(request: dict) -> dict:
    operation, args = validate_request(request)
    bindings = load_bindings()
    if operation == "status":
        data = statuses(bindings, args.get("provider"))
    else:
        provider, mode = args["provider"], args["mode"]
        if mode == "live-api":
            raise ChatError("api-protocol-unverified", "Interface authentication and exact-conversation pagination have not been qualified. Select live-dom explicitly for a visible window, or snapshot for local history.")
        binding = validate_binding(provider, bindings.get(provider, {}), mode)
        if mode == "snapshot":
            from chat import snapshots as adapter
        else:
            from chat import live as adapter
        data = getattr(adapter, operation)(provider, binding, args)
    return {"schemaVersion": 1, "operation": operation, "provider": args.get("provider"), "mode": args.get("mode"), "exitCode": 0, "data": data}


def main() -> int:
    request = None
    try:
        raw = sys.stdin.buffer.read(MAX_INPUT + 1)
        if len(raw) > MAX_INPUT:
            raise ChatError("invalid-request", "Chat request exceeds its size limit.")
        request = json.loads(raw)
        result = execute(request)
        serialized = json.dumps(result, ensure_ascii=False, allow_nan=False)
        if len(serialized.encode()) > MAX_OUTPUT:
            raise ChatError("output-too-large", "Chat result exceeds 16 MiB; request a smaller page.")
    except Exception as error:
        if isinstance(error, ChatError):
            code, message = error.code, error.public_message
        else:
            code, message = "provider-read-failed", "Selected provider could not return a verified result. No source fallback was attempted."
        result = {"schemaVersion": 1, "exitCode": 2, "error": {"code": code, "message": message}}
        candidates = getattr(error, "metadata", {}).get("candidates") if isinstance(error, ChatError) else None
        if isinstance(candidates, list):
            result["error"]["candidates"] = [
                {k: row[k] for k in ("conversationId", "conversationName", "identityKind") if isinstance(row.get(k), str)}
                for row in candidates[:100] if isinstance(row, dict)
            ]
        serialized = json.dumps(result, ensure_ascii=False)
    print(serialized)
    return result["exitCode"]


if __name__ == "__main__":
    raise SystemExit(main())
