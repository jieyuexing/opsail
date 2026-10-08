"""Private, local bindings for the Opsail chat readers. Never emit bindings."""

from __future__ import annotations

import json
import os
import stat
from pathlib import Path
from urllib.parse import urlsplit

PROVIDERS = ("wechat", "feishu", "teams")
MODES = ("snapshot", "live-api", "live-dom")
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from paths import CHAT_DATA_ROOT, CHAT_BINDING_FILE

DEFAULT_DATA_ROOT = CHAT_DATA_ROOT
DEFAULT_BINDING_FILE = CHAT_BINDING_FILE
DEFAULT_EXTENSION_CONFIG_FILE = DEFAULT_DATA_ROOT / "config.json"


class ChatError(RuntimeError):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code
        self.public_message = message


def active_path(value: str, *, directory: bool = True) -> Path:
    """Reject retired sources before and after symlink resolution."""
    if not isinstance(value, str) or not value or "\x00" in value:
        raise ChatError("invalid-binding", "Invalid private source binding.")
    path = Path(value)
    if not path.is_absolute():
        raise ChatError("invalid-binding", "Source binding must be absolute.")
    try:
        extra_denied = json.loads(os.environ.get("OPSAIL_CHAT_DENIED_PATH_COMPONENTS", "[]"))
        if not isinstance(extra_denied, list) or any(not isinstance(item, str) for item in extra_denied):
            raise ValueError()
    except (ValueError, TypeError):
        raise ChatError("invalid-binding", "Invalid source path policy.") from None
    for candidate in (path, path.resolve()):
        parts = candidate.parts
        if any(p in {".Trash", "old", *extra_denied} or p.startswith("retired-") for p in parts) or any(
            parts[i:i + 2] == (".local", "archive") for i in range(len(parts) - 1)
        ):
            raise ChatError("retired-source-denied", "Retired sources cannot be runtime bindings.")
    resolved = path.resolve(strict=True)
    if directory and not resolved.is_dir():
        raise ChatError("invalid-binding", "Source directory is unavailable.")
    if not directory and not resolved.is_file():
        raise ChatError("invalid-binding", "Private binding file is unavailable.")
    return resolved


def load_bindings() -> dict:
    """Load legacy bindings, with extension bindings from the fixed data root.

    An explicitly verified authoritative extension binding in ``config.json``
    owns browser selection; ``bindings.json`` retains snapshot custody. Legacy
    configurations keep their prior merge rules until an operator runs bind.
    An explicit OPSAIL_CHAT_BINDING_FILE always remains isolated.
    """
    raw = os.environ.get("OPSAIL_CHAT_BINDING_FILE", str(DEFAULT_BINDING_FILE))
    legacy = _load_private_json(Path(raw), schema="bindings") if Path(raw).exists() else {}
    # An explicit binding file is an isolated source selection. In particular,
    # fixtures and separately configured operators must not inherit this user's
    # real Chrome account from the fixed installation record.
    if "OPSAIL_CHAT_BINDING_FILE" in os.environ:
        return legacy
    extension_unavailable = False
    try:
        if DEFAULT_EXTENSION_CONFIG_FILE.exists():
            extension = _load_private_json(DEFAULT_EXTENSION_CONFIG_FILE, schema="extension")
        else:
            extension = {}
            extension_unavailable = (DEFAULT_EXTENSION_CONFIG_FILE.parent / "runtime/current/extension").exists()
    except ChatError:
        # Preserve snapshot custody, but never fall back to an old browser when
        # the installed authority cannot be read.
        if not legacy:
            raise
        extension = {}
        extension_unavailable = True
    if extension_unavailable:
        legacy = {provider: {key: val for key, val in value.items() if key != "browser"}
                  if provider in {"feishu", "teams"} and isinstance(value, dict) else value
                  for provider, value in legacy.items()}
    merged = dict(extension)
    for provider, value in legacy.items():
        prior = merged.get(provider, {})
        if not isinstance(value, dict) or not isinstance(prior, dict):
            merged[provider] = value
            continue
        combined = {**prior, **value}
        if isinstance(prior.get("browser"), dict) and isinstance(value.get("browser"), dict):
            combined["browser"] = prior["browser"] if prior.get("_browser_authority") else {**prior["browser"], **value["browser"]}
        merged[provider] = combined
    for value in merged.values():
        if isinstance(value, dict):
            value.pop("_browser_authority", None)
    return merged


def _load_private_json(path: Path, *, schema: str) -> dict:
    path = active_path(str(path), directory=False)
    info = path.stat()
    if stat.S_IMODE(info.st_mode) != 0o600 or info.st_uid != os.getuid():
        raise ChatError("invalid-binding", "Private bindings must be owned by the current user with mode 0600.")
    if info.st_size > 65536:
        raise ChatError("invalid-binding", "Private binding file exceeds its size limit.")
    try:
        config = json.loads(path.read_text())
    except (ValueError, UnicodeError):
        raise ChatError("invalid-binding", "Private binding file is invalid.") from None
    if not isinstance(config, dict):
        raise ChatError("invalid-binding", "Unsupported private binding schema.")
    bindings = config.get("providers")
    if not isinstance(bindings, dict) or set(bindings) - set(PROVIDERS):
        raise ChatError("invalid-binding", "Invalid provider bindings.")
    if schema == "bindings":
        if set(config) - {"schemaVersion", "providers"} or config.get("schemaVersion") != 1:
            raise ChatError("invalid-binding", "Unsupported private binding schema.")
        return bindings
    required = {"schemaVersion", "dataRoot", "extensionId", "buildId", "nodePath", "providers"}
    # The native host owns and validates the optional identity build allow-list.
    identity_builds = config.get("identityBuilds")
    if identity_builds is not None and not (isinstance(identity_builds, dict) and set(identity_builds) <= set(PROVIDERS) and all(isinstance(v, list) for v in identity_builds.values())):
        raise ChatError("invalid-binding", "Extension installation record is invalid.")
    if set(config) - {"identityBuilds"} != required or config.get("schemaVersion") != 1 or config.get("dataRoot") != str(DEFAULT_DATA_ROOT) or not _opaque_id(config.get("extensionId"), 128) or not _opaque_id(config.get("buildId"), 128) or not isinstance(config.get("nodePath"), str) or not config["nodePath"]:
        raise ChatError("invalid-binding", "Extension installation record is invalid.")
    result: dict[str, dict] = {}
    for provider, value in bindings.items():
        if not isinstance(value, dict) or set(value) - {"profileId", "pending", "targetUrl", "allowedOrigins", "accountHash", "tenantHash", "authoritative"} or not _opaque_id(value.get("profileId")):
            raise ChatError("invalid-binding", "Extension provider binding is invalid.")
        if value.get("authoritative") not in (None, True):
            raise ChatError("invalid-binding", "Extension binding authority is invalid.")
        if value.get("pending") is True:
            # A failed initial extension handshake must not make unrelated
            # existing readers unavailable.  The installer retains this state
            # for doctor/bind, but it is not a selectable chat source yet.
            if any(key not in value for key in ("targetUrl", "allowedOrigins")) or any(key in value for key in ("accountHash", "tenantHash")):
                raise ChatError("invalid-binding", "Pending extension binding is invalid.")
            continue
        if value.get("pending") not in {None, False}:
            raise ChatError("invalid-binding", "Extension provider binding is invalid.")
        result[provider] = {"_browser_authority": value.get("authoritative") is True, "browser": {
            "transport": "chrome-extension", "target_url": value.get("targetUrl"), "allowed_origins": value.get("allowedOrigins"),
            "account_hash": value.get("accountHash"), "tenant_hash": value.get("tenantHash"), "profile_id": value.get("profileId"),
            "build_id": config["buildId"],
        }}
    return result


def _opaque_id(value: object, limit: int = 80) -> bool:
    return isinstance(value, str) and bool(value) and len(value) <= limit and all(c.isalnum() or c in "._-" for c in value)


def _normalize_browser_origin(provider: str, value: object) -> str | None:
    """Accept one canonical HTTPS origin explicitly bound to its provider."""
    if not isinstance(value, str):
        return None
    try:
        parsed = urlsplit(value)
        port = parsed.port
    except ValueError:
        return None
    if (
        parsed.scheme != "https"
        or not parsed.hostname
        or parsed.username
        or parsed.password
        or parsed.path not in {"", "/"}
        or parsed.query
        or parsed.fragment
        or port not in {None, 443}
    ):
        return None
    host = parsed.hostname.lower()
    if provider == "feishu":
        if host != "feishu.cn" and not host.endswith(".feishu.cn"):
            return None
    elif provider == "teams":
        if host not in {"teams.microsoftonline.cn", "teams.microsoft.com", "teams.cloud.microsoft"}:
            return None
    else:
        return None
    return f"https://{host}"


def validate_binding(provider: str, raw: dict, mode: str) -> dict:
    if not isinstance(raw, dict) or set(raw) - {"snapshot_root", "browser"}:
        raise ChatError("invalid-binding", "Invalid provider configuration.")
    result = dict(raw)
    if mode == "snapshot":
        if not raw.get("snapshot_root"):
            raise ChatError("not-configured", "This snapshot source has not been configured.")
        result["snapshot_root"] = str(active_path(raw["snapshot_root"]))
    else:
        if provider == "wechat":
            raise ChatError("mode-unavailable", "WeChat supports decrypted local snapshots only.")
        browser = raw.get("browser")
        if not isinstance(browser, dict) or set(browser) - {"transport", "endpoint", "target_url", "allowed_origins", "profile_id", "account_hash", "tenant_hash", "build_id"}:
            raise ChatError("not-configured", "No supported browser session is bound to this provider.")
        if browser.get("transport") not in {"edge-active-tab", "chrome-active-tab", "chrome-tab", "cdp", "chrome-extension"}:
            raise ChatError("invalid-binding", "Unsupported browser transport.")
        if browser.get("transport") == "cdp":
            endpoint = urlsplit(browser.get("endpoint", ""))
            if endpoint.scheme not in {"http", "ws"} or endpoint.hostname not in {"localhost", "127.0.0.1", "::1"} or endpoint.username or endpoint.password or endpoint.query or endpoint.fragment:
                raise ChatError("invalid-binding", "CDP requires an explicit local browser endpoint.")
        origins = browser.get("allowed_origins", [])
        if not isinstance(origins, list) or not origins:
            raise ChatError("invalid-binding", "Browser origins must match the selected platform.")
        if provider == "feishu":
            normalized_origins = [_normalize_browser_origin(provider, origin) for origin in origins]
            if any(origin is None for origin in normalized_origins):
                raise ChatError("invalid-binding", "Browser origins must match the selected platform.")
        else:
            allowed = {"https://teams.microsoftonline.cn", "https://teams.microsoft.com", "https://teams.cloud.microsoft"}
            if any(origin not in allowed for origin in origins):
                raise ChatError("invalid-binding", "Browser origins must match the selected platform.")
            normalized_origins = origins
        if not isinstance(browser.get("target_url"), str) or not browser["target_url"]:
            raise ChatError("invalid-binding", "An exact browser target must be configured.")
        if browser.get("target_url"):
            target = urlsplit(browser["target_url"])
            target_origin = _normalize_browser_origin(provider, f"{target.scheme}://{target.netloc}") if provider == "feishu" else f"{target.scheme}://{target.netloc}"
            if target_origin not in normalized_origins or target.username or target.password:
                raise ChatError("invalid-binding", "Browser target does not match its platform binding.")
        if browser.get("transport") == "chrome-extension":
            for key in ("profile_id", "account_hash", "tenant_hash", "build_id"):
                if not _opaque_id(browser.get(key), 128 if key == "build_id" else 80):
                    raise ChatError("invalid-binding", "Chrome extension binding is incomplete.")
    return result
