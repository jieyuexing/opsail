"""Bounded, read-only browser views for Feishu and Teams conversations.

This module deliberately has no browser discovery, profile launch, navigation,
or snapshot-update support. A caller supplies a validated private binding to an
existing page. ``live-dom`` only reports what that page currently renders; it
never scrolls a conversation or chat rail.
"""

from __future__ import annotations

import hashlib
import json
from datetime import datetime
import re
import subprocess
from typing import Any, Callable
from urllib.parse import unquote, urlparse

from .live_sources.capture import build_visible_observation
from .config import ChatError


MAX_LIMIT = 200
DEFAULT_LIMIT = 50
_LIVE_DOM = "live-dom"


class ChatLiveError(ChatError):
    """A fail-closed live chat reader error safe to expose to a caller."""

    def __init__(self, message: str):
        super().__init__("live-read-failed", message)


def _allowed_origins(provider: str) -> set[str]:
    if provider == "feishu":
        return {"https://feishu.cn", "https://www.feishu.cn"}
    if provider == "teams":
        return {
            "https://teams.microsoft.com",
            "https://teams.microsoftonline.cn",
            "https://teams.cloud.microsoft",
        }
    raise ChatLiveError("live browser reading is only available for feishu or teams")


def _origin(url: str) -> str:
    parsed = urlparse(url)
    if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password:
        raise ChatLiveError("browser target must be an https URL without credentials")
    try:
        port = parsed.port
    except ValueError as exc:
        raise ChatLiveError("browser target must use a valid HTTPS port") from exc
    return f"https://{parsed.hostname.lower()}" + (f":{port}" if port not in {None, 443} else "")


def _bound_origin(provider: str, value: str) -> str:
    """Normalize one explicit private binding origin, failing closed on URL parts."""
    if provider != "feishu":
        normalized = value.rstrip("/").lower()
        if normalized not in _allowed_origins(provider):
            raise ChatLiveError("live browser binding contains an unapproved origin")
        return normalized
    try:
        parsed = urlparse(value)
        origin = _origin(value)
    except ChatLiveError:
        raise
    if parsed.path not in {"", "/"} or parsed.params or parsed.query or parsed.fragment:
        raise ChatLiveError("live browser binding origin must not include a path, query, or fragment")
    try:
        port = parsed.port
    except ValueError as exc:
        raise ChatLiveError("live browser binding origin must use a valid HTTPS port") from exc
    if port not in {None, 443}:
        raise ChatLiveError("live browser binding origin must use the standard HTTPS port")
    host = parsed.hostname or ""
    if host != "feishu.cn" and not host.endswith(".feishu.cn"):
        raise ChatLiveError("live browser binding contains an unapproved origin")
    return origin


def _is_allowed_url(provider: str, url: str, origins: set[str]) -> bool:
    try:
        origin = _origin(url)
    except ChatLiveError:
        return False
    if origin not in origins:
        return False
    host = urlparse(url).hostname or ""
    if provider == "feishu":
        return host == "feishu.cn" or host.endswith(".feishu.cn")
    return host in {"teams.microsoft.com", "teams.microsoftonline.cn", "teams.cloud.microsoft"} or host.endswith(".teams.microsoft.com") or host.endswith(".teams.microsoftonline.cn") or host.endswith(".teams.cloud.microsoft")


def _binding_browser(provider: str, binding: dict[str, Any]) -> dict[str, Any]:
    browser = binding.get("browser") if isinstance(binding, dict) else None
    if not isinstance(browser, dict):
        raise ChatLiveError("live browser binding is not configured")
    transport = browser.get("transport")
    if transport not in {"edge-active-tab", "chrome-active-tab", "chrome-tab", "cdp", "chrome-extension"}:
        raise ChatLiveError("live browser transport must be edge-active-tab, chrome-active-tab, chrome-tab, cdp, or chrome-extension")
    origins = browser.get("allowed_origins")
    if not isinstance(origins, list) or not origins or not all(isinstance(x, str) for x in origins):
        raise ChatLiveError("live browser binding has no allowed origins")
    normalized = {_bound_origin(provider, value) for value in origins}
    target_url = browser.get("target_url")
    if not isinstance(target_url, str) or not _is_allowed_url(provider, target_url, normalized):
        raise ChatLiveError("live browser binding must name one allowed target URL")
    if transport == "cdp":
        endpoint = browser.get("endpoint")
        if not isinstance(endpoint, str):
            raise ChatLiveError("CDP browser binding has no endpoint")
        parsed = urlparse(endpoint)
        if parsed.scheme not in {"http", "ws"} or parsed.hostname not in {"127.0.0.1", "localhost", "::1"}:
            raise ChatLiveError("CDP endpoint must be an explicit localhost endpoint")
    if transport == "chrome-extension":
        for key in ("profile_id", "account_hash", "tenant_hash", "build_id"):
            if not isinstance(browser.get(key), str) or not browser[key]:
                raise ChatLiveError("Chrome extension browser binding is incomplete")
    return {**browser, "allowed_origins": normalized}


def _limit(args: dict[str, Any]) -> int:
    value = args.get("limit", DEFAULT_LIMIT)
    if isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= MAX_LIMIT:
        raise ChatLiveError("limit must be an integer between 1 and 200")
    if args.get("cursor") is not None:
        raise ChatLiveError("live-dom does not support cursors")
    return value


def _require_live_dom(args: dict[str, Any]) -> None:
    if args.get("mode") != _LIVE_DOM:
        raise ChatLiveError("this reader only implements live-dom")


def _conversation_id_for_name(provider: str, name: str) -> str:
    return "dom-name:" + hashlib.sha256(f"{provider}\0{name}".encode("utf-8")).hexdigest()[:24]


def _teams_conversation_id(url: str) -> str | None:
    parsed = urlparse(url)
    for value in (parsed.path, parsed.fragment):
        match = re.search(r"/(?:l/chat|conversations)/([^/?#]+)", value)
        if match:
            return unquote(match.group(1))
    return None


def _select_page(provider: str, browser: dict[str, Any], playwright: Any) -> tuple[Any, Callable[[], None]]:
    """Return the existing page; closing the returned resource never closes a user browser."""
    target_url = browser["target_url"]
    if browser["transport"] in {"edge-active-tab", "chrome-active-tab", "chrome-tab"}:
        # Test-only seam. Production private JSON cannot carry this object and
        # therefore uses _edge_javascript below.
        page = browser.get("page")
        if page is not None:
            if getattr(page, "url", None) != target_url:
                raise ChatLiveError("active browser tab no longer matches the configured target")
            return page, lambda: None
        raise ChatLiveError("edge active-tab is read through the macOS browser bridge")

    try:
        browser_handle = playwright.chromium.connect_over_cdp(browser["endpoint"])
    except Exception as exc:
        raise ChatLiveError("could not connect to configured CDP browser") from exc
    pages = [page for context in browser_handle.contexts for page in context.pages]
    matched = [item for item in pages if getattr(item, "url", None) == target_url]
    if len(matched) != 1:
        raise ChatLiveError("configured browser target page is missing or ambiguous")
    page = matched[0]
    # Python Playwright Browser has close(), but no disconnect().  Do not call
    # close(): sync_playwright's context exit tears down only its own driver
    # connection and leaves the caller-owned CDP browser running.
    return page, lambda: None


_FEISHU_CATALOG = r"""() => Array.from(document.querySelectorAll('.a11y_feed_card_item')).map(item => {
 const name=(item.querySelector('.a11y_feed_card_main [style*="min-width"]')?.textContent||'').trim();
 return {name, selected:item.getAttribute('aria-selected') === 'true' || item.classList.contains('active')};
}).filter(item => item.name)"""
_TEAMS_CATALOG = r"""() => Array.from(document.querySelectorAll('[id^="title-chat-list-item_"]')).map(item => {
 const prefix='title-chat-list-item_'; const id=item.id.slice(prefix.length); return {id,name:(item.textContent||'').trim(),selected:item.closest('[aria-selected="true"],[data-is-selected="true"],[data-selected="true"]') !== null};
}).filter(item => item.id && item.name)"""
_FEISHU_MESSAGES = r"""() => Array.from(document.querySelectorAll('.messageItem-wrapper[data-id]')).map(item => {
 const self=item.querySelector('.message-self') !== null || item.classList.contains('message-self'); const sender=(item.querySelector('.message-info-name')?.textContent||'').trim(); const time=(item.querySelector('.message-layout-time-tip')?.textContent||'').trim(); const text=(item.querySelector('.message-text,.message-post')?.textContent||'').trim().replace(/展开$/, '').trim(); const id=item.getAttribute('data-id')||''; const media=[]; [['image','.im-image-message img,.message-image img,.chat-image__wrapper img'],['file','[class*="file-message"],[class*="fileCard"]'],['video','video'],['voice','audio']].forEach(([type,selector])=>Array.from(item.querySelectorAll(selector)).forEach((node,n)=>media.push({type,locator:`dom://messageItem/${encodeURIComponent(id||'unknown')}/${type}/${n}`,alt_text:(node.alt||node.getAttribute('aria-label')||'').trim()}))); return {id,sender,time,text,is_self:self,media};
}).filter(item=>item.text||item.media.length)"""
_TEAMS_MESSAGES = r"""() => Array.from(document.querySelectorAll('[data-tid="chat-pane-message"]')).map(message => {
 const item=message.closest('[data-tid="chat-pane-item"]'); const id=message.getAttribute('data-mid')||''; const media=[]; [['image','img[data-tid="lazy-image-2"]'],['file','[data-tid*="attachment"],[data-tid*="file-card"],a[download]'],['video','video'],['voice','audio']].forEach(([type,selector])=>Array.from(message.querySelectorAll(selector)).forEach((node,n)=>media.push({type,locator:`dom://chat-pane-message/${encodeURIComponent(id||'unknown')}/${type}/${n}`,alt_text:(node.alt||node.getAttribute('aria-label')||'').trim()}))); return {id,sender:(item?.querySelector('[data-tid="message-author-name"]')?.textContent||'').trim(),time:item?.querySelector('time')?.getAttribute('datetime')||'',text:(message.innerText||'').trim(),is_self:false,media};
}).filter(item=>item.text||item.media.length)"""
_TEAMS_EVIDENCE = r"""() => Array.from(document.querySelectorAll('[id^="title-chat-list-item_"]')).map(item => ({id:item.id.slice('title-chat-list-item_'.length),name:(item.textContent||'').trim(),selected:item.closest('[aria-selected="true"],[data-is-selected="true"],[data-selected="true"]') !== null})).filter(item => item.selected)"""


def _evaluate(page: Any, script: str, label: str) -> list[dict[str, Any]]:
    try:
        result = page.evaluate(script)
    except Exception as exc:
        raise ChatLiveError(f"browser could not read visible {label}") from exc
    if not isinstance(result, list) or not all(isinstance(item, dict) for item in result):
        raise ChatLiveError(f"browser returned invalid visible {label}")
    return result


def _run_osascript(application: str, command: str) -> str:
    """Run a fixed browser command and turn known Apple-event failures into safe errors."""
    try:
        result = subprocess.run(["osascript", "-e", command], text=True, capture_output=True, check=False, timeout=30)
    except subprocess.TimeoutExpired as exc:
        raise ChatLiveError(f"{application} active-tab read timed out") from exc
    if result.returncode != 0:
        error = (result.stderr or result.stdout or "").lower()
        if "not authorized" in error or "not permitted" in error or "-1743" in error:
            raise ChatLiveError(f"macOS denied permission to read the {application} active tab")
        if "isn’t running" in error or "isn't running" in error or "-600" in error:
            raise ChatLiveError(f"{application} is unavailable")
        if "opsail-no-matching-tab" in error or "opsail_no_matching_tab" in error:
            raise ChatLiveError(f"{application} has no tab matching the configured target")
        if "opsail-ambiguous-matching-tab" in error or "opsail_ambiguous_matching_tab" in error:
            raise ChatLiveError(f"{application} has multiple tabs matching the configured target")
        error_codes = set(re.findall(r"\((-?\d{1,6})\)|error(?: number)?\s*:?[ ]*(-?\d{1,6})", error))
        numeric_codes = {first or second for first, second in error_codes}
        javascript_apple_events = "12" in numeric_codes and "javascript" in error and "applescript" in error and "execut" in error
        if javascript_apple_events:
            raise ChatLiveError(f"{application} has disabled JavaScript from Apple events")
        if "can’t get window" in error or "can't get window" in error or "can’t get active tab" in error or "can't get active tab" in error or "invalid index" in error or "no windows" in error:
            raise ChatLiveError(f"{application} has no active browser tab")
        code = re.search(r"\((-?\d{1,6})\)|error(?: number)?\s*:?[ ]*(-?\d{1,6})", error)
        if code:
            raise ChatLiveError(f"{application} active tab could not be read (AppleScript error {code.group(1) or code.group(2)})")
        raise ChatLiveError(f"{application} active tab could not be read")
    return result.stdout.strip()


def _apple_script_string(value: str) -> str:
    """Quote module-owned text for AppleScript without turning Unicode into invalid ``\\u`` tokens."""
    return json.dumps(value, ensure_ascii=False)


def _active_tab_javascript(application: str, source: str) -> str:
    """Run fixed read-only JavaScript in one browser's current active tab.

    The source is module-owned constants only.  Callers cannot supply
    JavaScript, a URL, cookies, or arbitrary AppleScript.
    """
    command = "tell application " + _apple_script_string(application) + "\n" + f"execute active tab of front window javascript {_apple_script_string(source)}\n" + "end tell"
    return _run_osascript(application, command)


def _edge_javascript(source: str) -> str:
    """Compatibility seam for Edge's fixed active-tab reader."""
    return _active_tab_javascript("Microsoft Edge", source)


def _chrome_javascript(source: str) -> str:
    """Run fixed read-only JavaScript in Google Chrome's current active tab."""
    return _active_tab_javascript("Google Chrome", source)


def _chrome_tab_javascript(target_url: str, source: str) -> str:
    """Read one exact existing Chrome tab without focusing, navigating, or selecting it."""
    target = _apple_script_string(target_url)
    command = (
        'tell application "Google Chrome"\n'
        "set matchCount to 0\n"
        "repeat with theWindow in windows\n"
        "repeat with theTab in tabs of theWindow\n"
        f"if URL of theTab is {target} then set matchCount to matchCount + 1\n"
        "end repeat\n"
        "end repeat\n"
        'if matchCount is 0 then error "OPSAIL_NO_MATCHING_TAB"\n'
        'if matchCount is not 1 then error "OPSAIL_AMBIGUOUS_MATCHING_TAB"\n'
        "repeat with theWindow in windows\n"
        "repeat with theTab in tabs of theWindow\n"
        f"if URL of theTab is {target} then\n"
        f"set scriptResult to execute theTab javascript {_apple_script_string(source)}\n"
        "return scriptResult\n"
        "end if\n"
        "end repeat\n"
        "end repeat\n"
        "end tell"
    )
    return _run_osascript("Google Chrome", command)


def _edge_visible(provider: str, browser: dict[str, Any], script: str, label: str) -> list[dict[str, Any]]:
    return _active_tab_visible(provider, browser, script, label, _edge_javascript, "Microsoft Edge")


def _chrome_visible(provider: str, browser: dict[str, Any], script: str, label: str) -> list[dict[str, Any]]:
    return _active_tab_visible(provider, browser, script, label, _chrome_javascript, "Google Chrome")


def _chrome_tab_visible(provider: str, browser: dict[str, Any], script: str, label: str) -> list[dict[str, Any]]:
    wrapped = "(() => JSON.stringify({page_url: location.href, value: (" + script + ")()}))()"
    raw = _chrome_tab_javascript(browser["target_url"], wrapped)
    try:
        result = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise ChatLiveError("Google Chrome returned invalid visible chat data") from exc
    if not isinstance(result, dict) or result.get("page_url") != browser["target_url"]:
        raise ChatLiveError("Google Chrome matched tab no longer has the configured target")
    value = result.get("value")
    if not isinstance(value, list) or not all(isinstance(item, dict) for item in value):
        raise ChatLiveError(f"Google Chrome returned invalid visible {label}")
    return value


def _active_tab_visible(provider: str, browser: dict[str, Any], script: str, label: str, execute: Callable[[str], str], application: str) -> list[dict[str, Any]]:
    wrapped = "(() => JSON.stringify({page_url: location.href, value: (" + script + ")()}))()"
    raw = execute(wrapped)
    try:
        result = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise ChatLiveError(f"{application} returned invalid visible chat data") from exc
    if not isinstance(result, dict) or result.get("page_url") != browser["target_url"]:
        raise ChatLiveError(f"{application} active tab no longer matches the configured target")
    value = result.get("value")
    if not isinstance(value, list) or not all(isinstance(item, dict) for item in value):
        raise ChatLiveError(f"{application} returned invalid visible {label}")
    return value


def _catalog_visible(provider: str, page: Any, limit: int, browser: dict[str, Any] | None = None) -> dict[str, Any]:
    script = _FEISHU_CATALOG if provider == "feishu" else _TEAMS_CATALOG
    if browser is None:
        raw = _evaluate(page, script, "conversation catalog")
    elif browser["transport"] == "chrome-active-tab":
        raw = _chrome_visible(provider, browser, script, "conversation catalog")
    elif browser["transport"] == "chrome-tab":
        raw = _chrome_tab_visible(provider, browser, script, "conversation catalog")
    else:
        raw = _edge_visible(provider, browser, script, "conversation catalog")
    entries: list[dict[str, str]] = []
    for ordinal, item in enumerate(raw):
        name = str(item.get("name") or "").strip()
        identity = str(item.get("id") or (_conversation_id_for_name(provider, name) + f":visible:{ordinal}")).strip()
        if not name or not identity:
            continue
        entry: dict[str, str] = {"conversationId": identity, "conversationName": name, "identityKind": "provider-dom" if provider == "teams" else "visible-dom-name"}
        entries.append(entry)
        if len(entries) >= limit:
            break
    return {"entries": entries, "nextCursor": None, "complete": False}


def _status_diagnostics(value: Any) -> dict[str, Any]:
    """Project only known metadata; page-owned names, URLs and values never escape."""
    if not isinstance(value, dict):
        return {}
    result = {}
    for key in ("catalogCount", "stableIdCount", "selectedCount", "messageNodeCount", "verifiedMessageScopeCount",
                "stableMessageIdCount", "messageRowScopeCount", "messagePaneScopeCount", "rowScopeMatchCount",
                "paneScopeMatchCount", "resolvedScopeMatchCount", "displayedMessageCount", "conflictingTabCount"):
        if type(value.get(key)) is int and 0 <= value[key] <= 1_000_000:
            result[key] = value[key]
    for key in ("identityPresent", "catalogIdentityVerified", "messageScopeVerified", "loginFormPresent", "buildQualified", "behavioralContractPassed", "bundleNamesTruncated", "providerDegraded", "ownedTabClosed", "ownedTabCloseFailed"):
        if type(value.get(key)) is bool:
            result[key] = value[key]
    if value.get("identityKind") in ("dom-metadata", "teams-authenticated-context", "feishu-session-user"):
        result["identityKind"] = value["identityKind"]
    if isinstance(value.get("bundleNames"), list):
        result["bundleNames"] = list(dict.fromkeys(name for name in value["bundleNames"] if isinstance(name, str) and re.fullmatch(r"index\.[a-f0-9]{6,64}\.js", name)))[:16]
    bootstrap = value.get("bootstrap")
    if isinstance(bootstrap, dict):
        # Describe field types, never arbitrary field names or script resource URLs.
        summary = {}
        types = {}
        budget = [4000]
        def visit(node, depth=0):
            if depth > 8 or budget[0] <= 0:
                return
            budget[0] -= 1
            if isinstance(node, list):
                for item in node[:100]:
                    visit(item, depth + 1)
            elif isinstance(node, dict):
                kind = node.get("type")
                if isinstance(kind, str) and kind in ("string", "number", "boolean", "object", "undefined", "function", "symbol", "bigint", "null", "accessor"):
                    types[kind] = types.get(kind, 0) + 1
                for key in ("fields", "children", "slices", "sampleFields", "user", "tenant", "authenticationUser", "profile", "prototype", "parentPrototype", "innerClient", "context"):
                    if key in node:
                        visit(node[key], depth + 1)
        for key in ("globals", "componentShapes", "stateShapes", "conversationComparisons", "identityShapes", "clientShapes", "selectionShapes", "currentTreeShapes"):
            if isinstance(bootstrap.get(key), list):
                summary[key + "Count"] = min(len(bootstrap[key]), 1000)
                visit(bootstrap[key])
            elif type(bootstrap.get(key + "Count")) is int and 0 <= bootstrap[key + "Count"] <= 1_000_000:
                summary[key + "Count"] = bootstrap[key + "Count"]
        if isinstance(bootstrap.get("fieldTypes"), dict):
            for kind in ("string", "number", "boolean", "object", "undefined", "function", "symbol", "bigint", "null", "accessor"):
                count = bootstrap["fieldTypes"].get(kind)
                if type(count) is int and 0 <= count <= 1_000_000:
                    types[kind] = count
        summary["fieldTypes"] = types
        result["bootstrap"] = summary
    return result


def status(provider: str, binding: dict[str, Any], args: dict[str, Any] | None = None) -> dict[str, Any]:
    """Report capability configuration without connecting to browsers or reading content."""
    if provider not in {"feishu", "teams"}:
        raise ChatLiveError("live status is only available for feishu or teams")
    dom = {"state": "unavailable", "reason": None, "transport": None, "targetConfigured": False, "capabilities": {}, "diagnostics": {}}
    try:
        browser = _binding_browser(provider, binding)
        dom.update(transport=browser["transport"], targetConfigured=True)
        if browser["transport"] == "chrome-extension":
            from .extension import ExtensionError, request
            try:
                result = request({**binding, "_provider": provider}, "status", {})
                if set(result) - {"ready", "reason", "profileId", "accountHash", "tenantHash", "capabilities", "adapterVersion", "diagnostics"} or not isinstance(result.get("ready"), bool):
                    raise ChatLiveError("Chrome extension returned invalid status")
                if result.get("profileId") != browser["profile_id"]:
                    raise ChatLiveError("Chrome extension identity no longer matches the bound profile")
                if result["ready"] and (result.get("accountHash") != browser["account_hash"] or result.get("tenantHash") != browser["tenant_hash"]):
                    raise ChatLiveError("Chrome extension identity no longer matches the bound account")
                capabilities = {key: val for key, val in (result.get("capabilities") or {}).items()
                                if key in {"catalog", "read", "history", "channels", "replies"} and type(val) is bool} if isinstance(result.get("capabilities"), dict) else {}
                readable = result["ready"] and capabilities.get("catalog") is True
                reason = result.get("reason")
                # Codes originate in the extension, but never trust arbitrary provider text.
                reasons = {"identity-unverified", "identity-verified", "account-changed", "tenant-changed", "identity-changed",
                           "tab-not-prepared", "tab-missing", "page-changed", "paused", "site-permission-required",
                           "browser-operation-failed", "invalid-page-output", "browser-api-unqualified"}
                dom.update(state="ready" if readable else "unavailable",
                           reason=(reason if isinstance(reason, str) and reason in reasons else "extension-unavailable") if not result["ready"] else (None if readable else "browser-api-unqualified"),
                           capabilities=capabilities, diagnostics=_status_diagnostics(result.get("diagnostics")))
            except ExtensionError as exc:
                dom.update(reason=exc.code if exc.code in {"extension-unavailable", "extension-offline", "site-permission-required", "provider-timeout", "extension-invalid-response", "build-changed", "implementation-changed", "not-bound", "bridge-disabled"} else "extension-unavailable")
        else:
            dom.update(state="configured")
    except ChatLiveError as exc:
        dom.update(state="unavailable", reason=str(exc))
    return {"provider": provider, "liveApi": {"state": "unavailable", "reason": "no verified provider API protocol is configured"}, "liveDom": dom}


def catalog(provider: str, binding: dict[str, Any], args: dict[str, Any], *, playwright_factory: Callable[[], Any] | None = None) -> dict[str, Any]:
    _require_live_dom(args)
    limit = _limit(args)
    browser = _binding_browser(provider, binding)
    if browser["transport"] == "chrome-extension":
        from .extension import ExtensionError, request
        try:
            result = request({**binding, "_provider": provider}, "catalog", {"limit": limit})
        except ExtensionError as exc:
            raise ChatLiveError(exc.public_message) from exc
        if set(result) - {"entries", "nextCursor", "complete"} or result.get("nextCursor") is not None or result.get("complete") is not False or not isinstance(result.get("entries"), list):
            raise ChatLiveError("Chrome extension returned an invalid visible conversation catalog")
        entries = []
        for item in result["entries"]:
            position = item.get("lastMessagePosition") if isinstance(item, dict) else None
            if not isinstance(item, dict) or set(item) - {"conversationId", "conversationName", "identityKind", "lastMessagePosition"} or not all(isinstance(item.get(key), str) and item[key] for key in ("conversationId", "conversationName", "identityKind")) \
                    or position is not None and (type(position) is not int or position < 1):
                raise ChatLiveError("Chrome extension returned an invalid visible conversation catalog")
            entries.append({key: item[key] for key in ("conversationId", "conversationName", "identityKind", "lastMessagePosition") if key in item})
            if len(entries) >= limit:
                break
        return {"entries": entries, "nextCursor": None, "complete": False}
    if browser["transport"] in {"edge-active-tab", "chrome-active-tab", "chrome-tab"}:
        if browser.get("page") is not None:
            page, close = _select_page(provider, browser, None)
            try:
                return _catalog_visible(provider, page, limit)
            finally:
                close()
        return _catalog_visible(provider, None, limit, browser)
    if playwright_factory is None:
        try:
            from playwright.sync_api import sync_playwright
        except ImportError as exc:
            raise ChatLiveError("playwright is required for configured CDP browser reading") from exc
        playwright_factory = sync_playwright
    with playwright_factory() as playwright:
        page, close = _select_page(provider, browser, playwright)
        try:
            return _catalog_visible(provider, page, limit)
        finally:
            close()


def read(provider: str, binding: dict[str, Any], args: dict[str, Any], *, playwright_factory: Callable[[], Any] | None = None) -> dict[str, Any]:
    _require_live_dom(args)
    limit = _limit(args)
    browser = _binding_browser(provider, binding)
    requested_id = args.get("conversationId")
    requested_name = args.get("conversationName")
    requested_url = args.get("conversationUrl")
    if provider == "teams":
        if requested_url is not None and (not isinstance(requested_url, str) or not _is_allowed_url(provider, requested_url, browser["allowed_origins"])):
            raise ChatLiveError("requested Teams URL is outside the bound platform")
        url_id = _teams_conversation_id(requested_url) if isinstance(requested_url, str) else None
        if requested_id is not None and not isinstance(requested_id, str):
            raise ChatLiveError("Teams conversation id must be text")
        if requested_id is not None and url_id is not None and unquote(requested_id) != url_id:
            raise ChatLiveError("Teams conversation id does not match requested URL")
        actual_id = unquote(str(requested_id or url_id or ""))
        if requested_name is None and requested_id is None and requested_url is None:
            raise ChatLiveError("an exact Teams conversation identifier or URL is required")
        if requested_url and not url_id:
            raise ChatLiveError("Teams URL must identify one exact conversation")
        conversation_id, conversation_name = actual_id, str(requested_name or actual_id)
    else:
        if requested_url is not None:
            raise ChatLiveError("Feishu DOM reads accept a catalog ID or exact name, not a URL selector")
        if not requested_name and not requested_id:
            raise ChatLiveError("an exact Feishu catalog ID or name is required")
        conversation_id, conversation_name = str(requested_id or ""), str(requested_name or "").strip()

    if browser["transport"] == "chrome-extension":
        from .extension import ExtensionError, request
        extension_args = {"limit": limit}
        if requested_id:
            extension_args["conversationId"] = requested_id
        if requested_name:
            extension_args["conversationName"] = requested_name
        try:
            result = request({**binding, "_provider": provider}, "read", extension_args)
        except ExtensionError as exc:
            raise ChatLiveError(exc.public_message) from exc
        allowed = {"conversationId", "conversationName", "pageUrl", "messages"}
        if set(result) - allowed or not all(isinstance(result.get(key), str) and result[key] for key in ("conversationId", "conversationName", "pageUrl")) or not isinstance(result.get("messages"), list):
            raise ChatLiveError("Chrome extension returned an invalid conversation window")
        exact_id, exact_name, page_url = result["conversationId"], result["conversationName"], result["pageUrl"]
        if not _is_allowed_url(provider, page_url, browser["allowed_origins"]) or page_url != browser["target_url"]:
            raise ChatLiveError("Chrome extension returned a page outside the bound target")
        if requested_id and ((unquote(requested_id) if provider == "teams" else requested_id) != exact_id):
            raise ChatLiveError("Chrome extension selected a different conversation")
        if requested_name and requested_name != exact_name:
            raise ChatLiveError("Chrome extension selected a different conversation")
        raw = result["messages"]
        if len(raw) > limit or not all(isinstance(row, dict) and set(row) - {"id", "sender", "time", "text", "is_self", "media", "sender_ref", "raw_time", "date_status", "time_source", "time_zone"} == set() for row in raw):
            raise ChatLiveError("Chrome extension returned invalid visible messages")
        for row in raw:
            if not all(isinstance(row.get(key), str) for key in ("id", "sender", "time", "text")) or "is_self" not in row or (row["is_self"] is not None and not isinstance(row["is_self"], bool)) or not isinstance(row.get("media"), list):
                raise ChatLiveError("Chrome extension returned invalid visible messages")
            if "sender_ref" in row and row["sender_ref"] is not None and not isinstance(row["sender_ref"], str):
                raise ChatLiveError("Chrome extension returned invalid visible messages")
            time_keys = {"raw_time", "date_status", "time_source", "time_zone"}
            if time_keys.intersection(row):
                if not all(isinstance(row.get(key), str) for key in time_keys) or row["date_status"] not in {"known", "unknown"} or row["time_source"] not in {"message-metadata", "time-element", "visible-time", "date-separator", "raw"} or row["time_zone"] not in {"explicit-offset", "unknown"}:
                    raise ChatLiveError("Chrome extension returned invalid visible messages")
                if row["date_status"] == "known":
                    try:
                        if "T" not in row["time"]:
                            raise ValueError()
                        timestamp = datetime.fromisoformat(row["time"].replace("Z", "+00:00"))
                        if (timestamp.tzinfo is not None) != (row["time_zone"] == "explicit-offset") or row["time_source"] == "raw":
                            raise ValueError()
                    except ValueError as exc:
                        raise ChatLiveError("Chrome extension returned invalid visible messages") from exc
                elif row["time"] != row["raw_time"] or row["time_source"] != "raw" or row["time_zone"] != "unknown":
                    raise ChatLiveError("Chrome extension returned invalid visible messages")
            for media in row["media"]:
                if not isinstance(media, dict) or set(media) - {"type", "locator", "alt_text"} or not all(isinstance(media.get(key), str) for key in ("type", "locator", "alt_text")):
                    raise ChatLiveError("Chrome extension returned invalid visible messages")
        return build_visible_observation(provider, exact_id, exact_name, page_url, raw, limit, source_mode="chrome-extension")

    def collect(page: Any, edge_browser: dict[str, Any] | None = None) -> dict[str, Any]:
        # Observe selection and messages in one browser evaluation. Otherwise a
        # user switching chats between calls could relabel another chat's body.
        evidence_script = _FEISHU_CATALOG if provider == "feishu" else _TEAMS_EVIDENCE
        messages_script = _FEISHU_MESSAGES if provider == "feishu" else _TEAMS_MESSAGES
        combined = "() => [{page_url:location.href, evidence:(" + evidence_script + ")(), messages:(" + messages_script + ")().slice(-" + str(limit) + ")}]"
        if edge_browser is None:
            packet = _evaluate(page, combined, "conversation window")
        elif edge_browser["transport"] == "chrome-active-tab":
            packet = _chrome_visible(provider, edge_browser, combined, "conversation window")
        elif edge_browser["transport"] == "chrome-tab":
            packet = _chrome_tab_visible(provider, edge_browser, combined, "conversation window")
        else:
            packet = _edge_visible(provider, edge_browser, combined, "conversation window")
        if len(packet) != 1 or packet[0].get("page_url") != browser["target_url"] or not isinstance(packet[0].get("evidence"), list) or not isinstance(packet[0].get("messages"), list):
            raise ChatLiveError("selected browser page changed or returned an invalid conversation window")
        visible, raw = packet[0]["evidence"], packet[0]["messages"]
        if provider == "feishu":
            candidates = []
            for ordinal, entry in enumerate(visible):
                name = str(entry.get("name") or "").strip()
                identity = _conversation_id_for_name(provider, name) + f":visible:{ordinal}"
                if (not requested_name or name == conversation_name) and (not requested_id or identity == requested_id):
                    candidates.append({**entry, "id": identity})
            matches = candidates
            selected = [entry for entry in matches if entry.get("selected") is True]
            if len(matches) != 1 or len(selected) != 1:
                raise ChatLiveError("exact Feishu conversation is not uniquely selected in the configured page")
            exact_id, exact_name = selected[0]["id"], str(selected[0]["name"]).strip()
        else:
            matching = [entry for entry in visible if entry.get("selected") is True and (not conversation_id or unquote(str(entry.get("id") or "")) == conversation_id) and (not requested_name or str(entry.get("name") or "").strip() == conversation_name)]
            if len(matching) != 1:
                raise ChatLiveError("exact Teams conversation is not selected in the configured page")
            exact_id, exact_name = unquote(str(matching[0]["id"])), str(matching[0]["name"]).strip()
        if not all(isinstance(row, dict) for row in raw):
            raise ChatLiveError("browser returned an invalid message")
        return build_visible_observation(provider, exact_id, exact_name, packet[0]["page_url"], raw, limit)

    if browser["transport"] in {"edge-active-tab", "chrome-active-tab", "chrome-tab"}:
        if browser.get("page") is not None:
            page, close = _select_page(provider, browser, None)
            try:
                return collect(page)
            finally:
                close()
        return collect(None, browser)
    if playwright_factory is None:
        try:
            from playwright.sync_api import sync_playwright
        except ImportError as exc:
            raise ChatLiveError("playwright is required for configured CDP browser reading") from exc
        playwright_factory = sync_playwright
    with playwright_factory() as playwright:
        page, close = _select_page(provider, browser, playwright)
        try:
            return collect(page)
        finally:
            close()
