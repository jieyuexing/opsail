"""Unit tests for bounded DOM chat reading; no browser, port, or account is used."""

from __future__ import annotations

import sys
import unittest
import json
import os
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.request
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str((Path(__file__).resolve().parent / '../../../src').resolve()))
from chat.live import ChatLiveError, _FEISHU_CATALOG, _FEISHU_MESSAGES, _chrome_tab_javascript, catalog, read, status  # noqa: E402
from chat.live_sources.capture import build_visible_observation  # noqa: E402
from chat.config import validate_binding  # noqa: E402


class FakePage:
    def __init__(self, url, values):
        self.url = url
        self.values = list(values)
        self.scripts = []

    def evaluate(self, script):
        self.scripts.append(script)
        if script.startswith("() => [{page_url:location.href"):
            return [{"page_url": self.url, "evidence": self.values.pop(0), "messages": self.values.pop(0) if self.values else []}]
        return self.values.pop(0)


class FakeBrowser:
    def __init__(self, page):
        self.contexts = [type("Context", (), {"pages": [page]})()]
        self.disconnected = False
        self.closed = False

    def disconnect(self):
        self.disconnected = True

    def close(self):
        self.closed = True


class FakePlaywright:
    def __init__(self, browser):
        self.browser = browser
        self.chromium = type("Chromium", (), {"connect_over_cdp": lambda _, __: browser})()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        return False


class ExistingPlaywright:
    """Keep the test's owning driver alive while the reader attaches over CDP."""
    def __init__(self, playwright):
        self.playwright = playwright

    def __enter__(self):
        return self.playwright

    def __exit__(self, *_):
        return False


class LiveDomTests(unittest.TestCase):
    def feishu_binding(self, page):
        return {"browser": {"transport": "edge-active-tab", "page": page, "target_url": "https://feishu.cn/next/messenger", "allowed_origins": ["https://feishu.cn"]}}

    def teams_binding(self, page):
        return {"browser": {"transport": "edge-active-tab", "page": page, "target_url": "https://teams.microsoft.com/l/chat/19%3Aexample", "allowed_origins": ["https://teams.microsoft.com"]}}

    def test_feishu_catalog_is_visible_only_and_non_paginating(self):
        page = FakePage("https://feishu.cn/next/messenger", [[{"name": "A"}, {"name": "B"}]])
        result = catalog("feishu", self.feishu_binding(page), {"mode": "live-dom", "limit": 1})
        self.assertEqual(result["entries"][0]["conversationName"], "A")
        self.assertIsNone(result["nextCursor"])
        self.assertFalse(result["complete"])
        self.assertNotIn("scroll", page.scripts[0].lower())

    def test_feishu_read_requires_one_visible_exact_name(self):
        page = FakePage("https://feishu.cn/next/messenger", [[{"name": "same"}, {"name": "same"}]])
        with self.assertRaisesRegex(ChatLiveError, "not uniquely selected"):
            read("feishu", self.feishu_binding(page), {"mode": "live-dom", "conversationName": "same"})

    def test_teams_read_returns_visible_capture(self):
        page = FakePage("https://teams.microsoft.com/l/chat/19%3Aexample", [[{"id": "19%3Aexample", "name": "Example", "selected": True}], [{"id": "m1", "sender": "Ada", "time": "2026-09-07T01:00:00Z", "text": "hello", "is_self": False}]])
        result = read("teams", self.teams_binding(page), {"mode": "live-dom", "conversationId": "19%3Aexample"})
        self.assertEqual(result["completeness"], "visible-window")
        self.assertEqual(result["content"]["capture"]["messages"][0]["text"], "hello")
        self.assertEqual(result["content"]["capture"]["extensions"]["source_mode"], "live-dom")

    def test_teams_rejects_wrong_conversation_before_reading(self):
        page = FakePage("https://teams.microsoft.com/l/chat/19%3Aexample", [[]])
        with self.assertRaisesRegex(ChatLiveError, "not selected"):
            read("teams", self.teams_binding(page), {"mode": "live-dom", "conversationId": "wrong"})
        self.assertEqual(len(page.scripts), 1, "must inspect selected conversation evidence before rejecting")

    def test_status_does_not_connect_or_read(self):
        page = FakePage("https://feishu.cn/next/messenger", [])
        result = status("feishu", self.feishu_binding(page))
        self.assertEqual(result["liveApi"]["state"], "unavailable")
        self.assertEqual(result["liveDom"]["state"], "configured")
        self.assertEqual(page.scripts, [])

    def test_rejects_cursor_and_unapproved_origin(self):
        page = FakePage("https://feishu.cn/next/messenger", [])
        with self.assertRaisesRegex(ChatLiveError, "does not support cursors"):
            catalog("feishu", self.feishu_binding(page), {"mode": "live-dom", "cursor": "no"})
        unsafe = self.feishu_binding(page)
        unsafe["browser"]["allowed_origins"] = ["https://example.test"]
        with self.assertRaisesRegex(ChatLiveError, "unapproved origin"):
            catalog("feishu", unsafe, {"mode": "live-dom"})

    def test_cdp_stops_driver_without_closing_borrowed_browser(self):
        page = FakePage("https://teams.microsoft.com/l/chat/19%3Aexample", [[{"id": "19%3Aexample", "name": "Example", "selected": True}], [{"id": "m1", "sender": "Ada", "time": "", "text": "hello", "is_self": False}]])
        borrowed = FakeBrowser(page)
        binding = {"browser": {"transport": "cdp", "endpoint": "http://127.0.0.1:9222", "target_url": page.url, "allowed_origins": ["https://teams.microsoft.com"]}}
        result = read("teams", binding, {"mode": "live-dom", "conversationId": "19%3Aexample"}, playwright_factory=lambda: FakePlaywright(borrowed))
        self.assertEqual(result["item_count"], 1)
        self.assertFalse(borrowed.disconnected)
        self.assertFalse(borrowed.closed)

    def test_edge_active_tab_uses_fixed_script_and_checks_current_url(self):
        binding = {"browser": {"transport": "edge-active-tab", "target_url": "https://teams.microsoft.com/l/chat/19%3Aexample", "allowed_origins": ["https://teams.microsoft.com"]}}
        responses = [json.dumps({"page_url": binding["browser"]["target_url"], "value": [{"page_url": binding["browser"]["target_url"], "evidence": [{"id": "19%3Aexample", "name": "Example", "selected": True}], "messages": [{"id": "m1", "sender": "Ada", "time": "", "text": "hello", "is_self": False}]}]})]
        with patch("chat.live._edge_javascript", side_effect=responses) as invoke:
            result = read("teams", binding, {"mode": "live-dom", "conversationId": "19%3Aexample"})
        self.assertEqual(result["item_count"], 1)
        self.assertIn("chat-pane-message", invoke.call_args.args[0])
        self.assertEqual(invoke.call_count, 1, "selection and messages must be captured atomically")

    def test_cdp_duplicate_target_tabs_fail_without_reading_either_account(self):
        page = FakePage("https://teams.microsoft.com/l/chat/19%3Aexample", [])
        borrowed = FakeBrowser(page)
        borrowed.contexts[0].pages.append(FakePage(page.url, []))
        binding = {"browser": {"transport": "cdp", "endpoint": "http://127.0.0.1:9222", "target_url": page.url, "allowed_origins": ["https://teams.microsoft.com"]}}
        with self.assertRaisesRegex(ChatLiveError, "missing or ambiguous"):
            read("teams", binding, {"mode": "live-dom", "conversationId": "19%3Aexample"}, playwright_factory=lambda: FakePlaywright(borrowed))
        self.assertEqual(page.scripts, [])
        self.assertFalse(borrowed.closed)

    def test_catalog_id_roundtrips_and_other_conversation_is_rejected(self):
        selected = [{"name": "Work", "selected": True}, {"name": "Other", "selected": False}]
        page = FakePage("https://feishu.cn/next/messenger", [selected])
        entries = catalog("feishu", self.feishu_binding(page), {"mode": "live-dom"})["entries"]
        page.values = [selected, [{"id": "m1", "text": "bounded message"}]]
        result = read("feishu", self.feishu_binding(page), {"mode": "live-dom", "conversationId": entries[0]["conversationId"]})
        self.assertEqual(result["content"]["conversation_id"], entries[0]["conversationId"])
        page.values = [selected, [{"id": "m1", "text": "do not relabel this"}]]
        with self.assertRaises(ChatLiveError):
            read("feishu", self.feishu_binding(page), {"mode": "live-dom", "conversationId": entries[1]["conversationId"]})

    def test_teams_catalog_never_labels_all_entries_with_current_url(self):
        page = FakePage("https://teams.microsoft.com/l/chat/19%3Aexample", [[{"id": "19:one", "name": "One"}, {"id": "19:two", "name": "Two"}]])
        entries = catalog("teams", self.teams_binding(page), {"mode": "live-dom"})["entries"]
        self.assertTrue(all("conversationUrl" not in entry for entry in entries))

    def test_edge_active_tab_rejects_mismatched_current_url(self):
        binding = {"browser": {"transport": "edge-active-tab", "target_url": "https://teams.microsoft.com/l/chat/19%3Aexample", "allowed_origins": ["https://teams.microsoft.com"]}}
        response = '{"page_url":"https://teams.microsoft.com/l/chat/other","value":[]}'
        with patch("chat.live._edge_javascript", return_value=response):
            with self.assertRaisesRegex(ChatLiveError, "no longer matches"):
                catalog("teams", binding, {"mode": "live-dom"})

    def test_chrome_active_tab_uses_fixed_script_and_checks_selection(self):
        binding = {"browser": {"transport": "chrome-active-tab", "target_url": "https://teams.microsoft.com/l/chat/19%3Aexample", "allowed_origins": ["https://teams.microsoft.com"]}}
        response = json.dumps({"page_url": binding["browser"]["target_url"], "value": [{"page_url": binding["browser"]["target_url"], "evidence": [{"id": "19%3Aexample", "name": "Example", "selected": True}], "messages": [{"id": "m1", "sender": "Ada", "time": "", "text": "hello", "is_self": False}]}]})
        with patch("chat.live._chrome_javascript", return_value=response) as invoke:
            result = read("teams", binding, {"mode": "live-dom", "conversationId": "19%3Aexample"})
        self.assertEqual(result["item_count"], 1)
        self.assertIn("chat-pane-message", invoke.call_args.args[0])

    def test_chrome_active_tab_rejects_mismatched_current_url_without_fallback(self):
        binding = {"browser": {"transport": "chrome-active-tab", "target_url": "https://teams.microsoft.com/l/chat/19%3Aexample", "allowed_origins": ["https://teams.microsoft.com"]}}
        response = '{"page_url":"https://teams.microsoft.com/l/chat/other","value":[]}'
        with patch("chat.live._chrome_javascript", return_value=response) as invoke:
            with self.assertRaisesRegex(ChatLiveError, "no longer matches"):
                catalog("teams", binding, {"mode": "live-dom"})
        self.assertEqual(invoke.call_count, 1)

    def test_chrome_active_tab_reports_permission_unavailable_app_and_missing_tab(self):
        binding = {"browser": {"transport": "chrome-active-tab", "target_url": "https://teams.microsoft.com/l/chat/19%3Aexample", "allowed_origins": ["https://teams.microsoft.com"]}}
        denied = type("Result", (), {"returncode": 1, "stdout": "", "stderr": "Not authorized to send Apple events to Google Chrome. (-1743)"})()
        unavailable = type("Result", (), {"returncode": 1, "stdout": "", "stderr": "Application isn’t running. (-600)"})()
        missing = type("Result", (), {"returncode": 1, "stdout": "", "stderr": "Google Chrome got an error: Can’t get window 1. Invalid index."})()
        with patch("chat.live.subprocess.run", return_value=denied):
            with self.assertRaisesRegex(ChatLiveError, "denied permission"):
                catalog("teams", binding, {"mode": "live-dom"})
        with patch("chat.live.subprocess.run", return_value=unavailable):
            with self.assertRaisesRegex(ChatLiveError, "is unavailable"):
                catalog("teams", binding, {"mode": "live-dom"})
        with patch("chat.live.subprocess.run", return_value=missing):
            with self.assertRaisesRegex(ChatLiveError, "no active browser tab"):
                catalog("teams", binding, {"mode": "live-dom"})

    def test_chrome_active_tab_keeps_unknown_apple_event_code_without_stderr(self):
        binding = {"browser": {"transport": "chrome-active-tab", "target_url": "https://teams.microsoft.com/l/chat/19%3Aexample", "allowed_origins": ["https://teams.microsoft.com"]}}
        unknown = type("Result", (), {"returncode": 1, "stdout": "", "stderr": "Google Chrome returned an internal error. (-1728) private path omitted"})()
        with patch("chat.live.subprocess.run", return_value=unknown):
            with self.assertRaisesRegex(ChatLiveError, r"AppleScript error -1728") as raised:
                catalog("teams", binding, {"mode": "live-dom"})
        self.assertNotIn("private path", str(raised.exception))

    def test_config_accepts_chrome_active_tab_without_cdp_endpoint(self):
        binding = {"browser": {"transport": "chrome-active-tab", "target_url": "https://fixture-org.feishu.cn/next/messenger/", "allowed_origins": ["https://fixture-org.feishu.cn"]}}
        self.assertEqual(validate_binding("feishu", binding, "live-dom")["browser"]["transport"], "chrome-active-tab")

    def test_config_accepts_background_chrome_tab_without_cdp_endpoint(self):
        binding = {"browser": {"transport": "chrome-tab", "target_url": "https://fixture-org.feishu.cn/next/messenger/", "allowed_origins": ["https://fixture-org.feishu.cn"]}}
        self.assertEqual(validate_binding("feishu", binding, "live-dom")["browser"]["transport"], "chrome-tab")

    def test_background_chrome_tab_reads_exact_existing_tab_without_focus_or_navigation(self):
        binding = {"browser": {"transport": "chrome-tab", "target_url": "https://teams.microsoft.com/l/chat/19%3Aexample", "allowed_origins": ["https://teams.microsoft.com"]}}
        response = json.dumps({"page_url": binding["browser"]["target_url"], "value": [{"page_url": binding["browser"]["target_url"], "evidence": [{"id": "19%3Aexample", "name": "Example", "selected": True}], "messages": [{"id": "m1", "sender": "Ada", "time": "", "text": "background", "is_self": False}]}]})
        with patch("chat.live._chrome_tab_javascript", return_value=response) as invoke:
            result = read("teams", binding, {"mode": "live-dom", "conversationId": "19%3Aexample"})
        self.assertEqual(result["content"]["capture"]["messages"][0]["text"], "background")
        self.assertEqual(invoke.call_args.args[0], binding["browser"]["target_url"])
        self.assertIn("chat-pane-message", invoke.call_args.args[1])

    def test_background_chrome_tab_script_counts_before_execution_and_never_focuses(self):
        completed = type("Result", (), {"returncode": 0, "stdout": "[]", "stderr": ""})()
        with patch("chat.live.subprocess.run", return_value=completed) as invoke:
            _chrome_tab_javascript("https://teams.microsoft.com/l/chat/19%3Aexample", "() => []")
        command = invoke.call_args.args[0][2]
        self.assertIn("set matchCount to 0", command)
        self.assertLess(command.index("if matchCount is not 1"), command.index("execute theTab javascript"))
        self.assertNotIn("activate", command.lower())
        self.assertNotIn("set active tab", command.lower())
        self.assertNotIn("open location", command.lower())

    @unittest.skipUnless(sys.platform == "darwin" and shutil.which("osacompile"), "requires macOS AppleScript compiler")
    def test_background_chrome_tab_generated_applescript_compiles(self):
        command: list[str] = []
        with patch("chat.live._run_osascript", side_effect=lambda _app, source: command.append(source) or "[]"):
            combined = "() => [{page_url:location.href, evidence:(" + _FEISHU_CATALOG + ")(), messages:(" + _FEISHU_MESSAGES + ")().slice(-50)}]"
            _chrome_tab_javascript("https://feishu.cn/next/messenger/", "(() => JSON.stringify({page_url: location.href, value: (" + combined + ")()}))()")
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "chrome-tab.applescript"
            compiled = Path(directory) / "chrome-tab.scpt"
            source.write_text(command[0], encoding="utf-8")
            checked = subprocess.run(["osacompile", "-o", str(compiled), str(source)], text=True, capture_output=True, check=False)
        # This sandbox cannot query the macOS scripting dictionary. A normal
        # macOS runner compiles the same generated source and catches grammar
        # changes; do not mistake the sandbox's dictionary-service failure for
        # a script grammar result.
        if "connection invalid error for service com.apple.hiservices-xpcservice" in checked.stderr.lower():
            self.skipTest("AppleScript dictionary service is unavailable")
        self.assertEqual(checked.returncode, 0, checked.stderr)

    def test_background_chrome_tab_rejects_duplicate_exact_targets_before_javascript(self):
        binding = {"browser": {"transport": "chrome-tab", "target_url": "https://teams.microsoft.com/l/chat/19%3Aexample", "allowed_origins": ["https://teams.microsoft.com"]}}
        duplicate = type("Result", (), {"returncode": 1, "stdout": "", "stderr": "OPSAIL_AMBIGUOUS_MATCHING_TAB"})()
        with patch("chat.live.subprocess.run", return_value=duplicate) as invoke:
            with self.assertRaisesRegex(ChatLiveError, "multiple tabs matching"):
                catalog("teams", binding, {"mode": "live-dom"})
        command = invoke.call_args.args[0][2]
        self.assertLess(command.index("if matchCount is not 1"), command.index("execute theTab javascript"))

    def test_background_chrome_tab_reports_disabled_apple_event_javascript(self):
        binding = {"browser": {"transport": "chrome-tab", "target_url": "https://teams.microsoft.com/l/chat/19%3Aexample", "allowed_origins": ["https://teams.microsoft.com"]}}
        disabled = type("Result", (), {"returncode": 1, "stdout": "", "stderr": "Executing JavaScript through AppleScript is turned off. (12)"})()
        with patch("chat.live.subprocess.run", return_value=disabled):
            with self.assertRaisesRegex(ChatLiveError, "disabled JavaScript from Apple events"):
                catalog("teams", binding, {"mode": "live-dom"})

    def test_chrome_classifies_execution_wording_for_disabled_apple_event_javascript(self):
        binding = {"browser": {"transport": "chrome-tab", "target_url": "https://teams.microsoft.com/l/chat/19%3Aexample", "allowed_origins": ["https://teams.microsoft.com"]}}
        disabled = type("Result", (), {"returncode": 1, "stdout": "", "stderr": "Google Chrome got an error: JavaScript execution through AppleScript is unavailable. error number 12"})()
        with patch("chat.live.subprocess.run", return_value=disabled):
            with self.assertRaisesRegex(ChatLiveError, "disabled JavaScript from Apple events"):
                catalog("teams", binding, {"mode": "live-dom"})

    def test_chrome_does_not_misclassify_non_12_javascript_applescript_error(self):
        binding = {"browser": {"transport": "chrome-tab", "target_url": "https://teams.microsoft.com/l/chat/19%3Aexample", "allowed_origins": ["https://teams.microsoft.com"]}}
        runtime = type("Result", (), {"returncode": 1, "stdout": "", "stderr": "Google Chrome got an error: JavaScript execution through AppleScript failed. (-1728)"})()
        with patch("chat.live.subprocess.run", return_value=runtime):
            with self.assertRaisesRegex(ChatLiveError, r"AppleScript error -1728") as raised:
                catalog("teams", binding, {"mode": "live-dom"})
        self.assertNotIn("disabled JavaScript", str(raised.exception))

    def test_visible_media_is_preserved_as_locator_only(self):
        result = build_visible_observation("feishu", "c1", "Fixture", "https://feishu.cn/next/messenger", [{"id": "m1", "text": "", "media": [{"type": "image", "locator": "dom://fixture/image/0", "alt_text": "diagram"}]}], 50)
        capture = result["content"]["capture"]
        self.assertEqual(capture["messages"][0]["media_refs"], [capture["media"][0]["media_id"]])
        self.assertEqual(capture["media"][0]["capture_status"], "locator-only")

    def test_real_local_cdp_fixture_reads_without_closing_browser(self):
        """Exercise Playwright's actual CDP Browser API against an owned fixture."""
        try:
            from playwright.sync_api import sync_playwright
        except ImportError:
            self.skipTest("Playwright is unavailable")
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        profile = tempfile.mkdtemp(prefix="opsail-chat-cdp-test-")
        with sync_playwright() as playwright:
            process = subprocess.Popen([playwright.chromium.executable_path, "--headless=new", "--no-first-run", "--remote-debugging-address=127.0.0.1", f"--remote-debugging-port={port}", f"--user-data-dir={profile}", "about:blank"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                endpoint = None
                for _ in range(30):
                    try:
                        payload = json.load(urllib.request.urlopen(f"http://127.0.0.1:{port}/json/version", timeout=1))
                        endpoint = payload.get("webSocketDebuggerUrl")
                        if isinstance(endpoint, str):
                            break
                    except OSError:
                        time.sleep(0.1)
                if endpoint is None:
                    self.fail("local CDP fixture did not become available")
                owner = playwright.chromium.connect_over_cdp(endpoint)
                page = owner.contexts[0].new_page()
                page.route("https://teams.microsoft.com/**", lambda route: route.fulfill(status=200, content_type="text/html", body='<div aria-selected="true"><div id="title-chat-list-item_19%3Afixture">Fixture</div></div><div data-tid="chat-pane-item"><span data-tid="message-author-name">Ada</span><time datetime="2026-01-01T00:00:00Z"></time><div data-tid="chat-pane-message" data-mid="m1">fixture message<img data-tid="lazy-image-2" alt="fixture diagram"></div></div>'))
                page.goto("https://teams.microsoft.com/l/chat/19%3Afixture")
                binding = {"browser": {"transport": "cdp", "endpoint": endpoint, "target_url": page.url, "allowed_origins": ["https://teams.microsoft.com"]}}
                result = read("teams", binding, {"mode": "live-dom", "conversationId": "19%3Afixture"}, playwright_factory=lambda: ExistingPlaywright(playwright))
                self.assertEqual(result["content"]["capture"]["messages"][0]["text"], "fixture message")
                self.assertEqual(result["content"]["capture"]["media"][0]["capture_status"], "locator-only")
                # Exercise the production Node -> CLI -> CDP path, including media
                # output validation. All page bytes are this local fixture.
                private = Path(profile) / "bindings.json"
                private.write_text(json.dumps({"schemaVersion": 1, "providers": {"teams": binding}}))
                private.chmod(0o600)
                module = ((Path(__file__).resolve().parent / '../../../src/chat/tools.js').resolve()).as_uri()
                script = f"import {{runChat}} from {json.dumps(module)}; console.log(JSON.stringify(await runChat('read', {{provider:'teams',mode:'live-dom',conversationId:'19%3Afixture'}})));"
                checked = subprocess.run(["node", "--input-type=module", "-e", script], capture_output=True, text=True, timeout=30, env={**os.environ, "OPSAIL_CHAT_BINDING_FILE": str(private), "OPSAIL_CHAT_PYTHON": sys.executable})
                self.assertEqual(checked.returncode, 0)
                envelope = json.loads(checked.stdout)
                self.assertEqual(envelope["exitCode"], 0, envelope.get("error"))
                self.assertEqual(envelope["data"]["content"]["capture"]["media"][0]["capture_status"], "locator-only")
                self.assertIsNone(process.poll(), "reader must not close the caller-owned CDP browser")
            finally:
                process.terminate()
                process.wait(timeout=10)
                shutil.rmtree(profile, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()
