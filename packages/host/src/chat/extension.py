"""Bounded Unix-socket client for the Opsail Chrome extension native host.

The native host owns Chrome Native Messaging.  This module only sends one
fixed, schema-checked command over the per-profile local socket; callers never
pass URLs, JavaScript, headers, cookies, or a socket path.
"""

from __future__ import annotations

import json
import os
import socket
import stat
import uuid
from pathlib import Path
from typing import Any

from .config import ChatError, DEFAULT_DATA_ROOT
from browser.ipc import runtime_socket_path

PROTOCOL_VERSION = 1
MAX_BYTES = 512 * 1024
TIMEOUT_SECONDS = 60
OPERATIONS = {"status", "catalog", "read", "prepare", "select", "messagesPage", "scrollBack"}


class ExtensionError(ChatError):
    def __init__(self, code: str, message: str):
        super().__init__(code, message)


def _socket_path(profile_id: str) -> Path:
    if not isinstance(profile_id, str) or not profile_id or len(profile_id) > 80 or any(c not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-" for c in profile_id):
        raise ExtensionError("invalid-binding", "Chrome extension profile binding is invalid.")
    try:
        return runtime_socket_path(DEFAULT_DATA_ROOT, profile_id)
    except ValueError as exc:
        raise ExtensionError("socket-path-too-long", str(exc)) from exc


def _safe_socket(path: Path) -> None:
    try:
        info = path.lstat()
    except FileNotFoundError as exc:
        raise ExtensionError("extension-unavailable", "Opsail Chrome extension host is not running for this profile.") from exc
    if not stat.S_ISSOCK(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) & 0o077:
        raise ExtensionError("extension-unavailable", "Opsail Chrome extension socket is not privately owned.")


def _request_args(operation: str, args: dict[str, Any]) -> dict[str, Any]:
    if not isinstance(args, dict):
        raise ExtensionError("invalid-request", "Chrome extension arguments must be an object.")
    allowed = {
        "status": set(), "catalog": {"limit"},
        "read": {"conversationId", "conversationName", "limit"},
        "prepare": set(), "select": {"conversationId", "conversationName"},
        "messagesPage": {"conversationId", "cursor", "limit"},
        "scrollBack": {"conversationId", "direction"},
    }[operation]
    if set(args) - allowed:
        raise ExtensionError("invalid-request", "Unsupported Chrome extension arguments.")
    return dict(args)


def request(binding: dict[str, Any], operation: str, args: dict[str, Any]) -> dict[str, Any]:
    """Make one request and reject all malformed or oversized native replies."""
    if operation not in OPERATIONS:
        raise ExtensionError("invalid-request", "Unsupported Chrome extension operation.")
    browser = binding.get("browser") if isinstance(binding, dict) else None
    if not isinstance(browser, dict) or browser.get("transport") != "chrome-extension":
        raise ExtensionError("not-configured", "No Opsail Chrome extension is selected for this provider.")
    profile_id = browser.get("profile_id")
    path = _socket_path(profile_id)
    _safe_socket(path)
    request_id = uuid.uuid4().hex
    packet = {"protocolVersion": PROTOCOL_VERSION, "requestId": request_id, "operation": operation,
              "provider": binding.get("_provider"), "args": _request_args(operation, args)}
    if packet["provider"] not in {"feishu", "teams"}:
        raise ExtensionError("invalid-binding", "Chrome extension provider binding is invalid.")
    encoded = (json.dumps(packet, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8")
    if len(encoded) > MAX_BYTES:
        raise ExtensionError("invalid-request", "Chrome extension request exceeds its size limit.")
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(TIMEOUT_SECONDS)
            client.connect(str(path))
            client.sendall(encoded)
            chunks: list[bytes] = []
            size = 0
            while b"\n" not in b"".join(chunks):
                chunk = client.recv(min(65536, MAX_BYTES - size + 1))
                if not chunk:
                    break
                chunks.append(chunk)
                size += len(chunk)
                if size > MAX_BYTES:
                    raise ExtensionError("extension-invalid-response", "Chrome extension response exceeds its size limit.")
    except socket.timeout as exc:
        raise ExtensionError("provider-timeout", "Opsail Chrome extension host exceeded its deadline.") from exc
    except OSError as exc:
        raise ExtensionError("extension-unavailable", "Opsail Chrome extension host is unavailable for this profile.") from exc
    raw = b"".join(chunks)
    if raw.count(b"\n") != 1 or not raw.endswith(b"\n"):
        raise ExtensionError("extension-invalid-response", "Chrome extension host returned an invalid response frame.")
    try:
        response = json.loads(raw[:-1])
    except (UnicodeDecodeError, ValueError) as exc:
        raise ExtensionError("extension-invalid-response", "Chrome extension host returned invalid JSON.") from exc
    if not isinstance(response, dict) or set(response) - {"type", "protocolVersion", "requestId", "ok", "data", "error"} or response.get("type") != "response" or response.get("protocolVersion") != PROTOCOL_VERSION or response.get("requestId") != request_id or not isinstance(response.get("ok"), bool):
        raise ExtensionError("extension-invalid-response", "Chrome extension host response does not match the request.")
    if response["ok"]:
        if set(response) - {"type", "protocolVersion", "requestId", "ok", "data"} or not isinstance(response.get("data"), dict):
            raise ExtensionError("extension-invalid-response", "Chrome extension host returned invalid data.")
        return response["data"]
    error = response.get("error")
    # The native host may attach bounded diagnostics and a fixed next step to an error.
    if set(response) - {"type", "protocolVersion", "requestId", "ok", "error"} or not isinstance(error, dict) or set(error) - {"code", "message", "diagnostics", "nextStep"} or not isinstance(error.get("code"), str) or not isinstance(error.get("message"), str) or not isinstance(error.get("diagnostics", {}), dict) or not isinstance(error.get("nextStep", ""), str):
        raise ExtensionError("extension-invalid-response", "Chrome extension host returned an invalid error.")
    raise ExtensionError(error["code"] if error["code"].replace("-", "").isalnum() else "extension-read-failed", error["message"][:512])
