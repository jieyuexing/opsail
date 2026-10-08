import contextlib
import io
import json
import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str((Path(__file__).resolve().parent / '../../../src').resolve()))
from chat import browser
from chat.config import ChatError

TARGET = 'https://fixture.feishu.cn/next/messenger/'
BINDING = {'browser': {'transport': 'chrome-tab', 'target_url': TARGET, 'allowed_origins': ['https://fixture.feishu.cn']}}


class BrowserPreparationTests(unittest.TestCase):
    def evaluate_selection(self, names, *, current_url=TARGET, provider='feishu', identity=None):
        source = browser.selection_script(provider, TARGET, None if identity else '中文群', identity)
        harness = r"""
const vm = require('node:vm');
const request = JSON.parse(require('node:fs').readFileSync(0, 'utf8'));
let clicks = 0;
const items = request.names.map((name, i) => ({
  textContent:name, id:'title-chat-list-item_19:'+i,
  querySelector:()=>({textContent:name}), click:()=>clicks++
}));
let result, error;
try { result = JSON.parse(vm.runInNewContext(request.source, {
  location:{href:request.url}, document:{querySelectorAll:()=>items}
})); } catch(e) { error = e.message; }
process.stdout.write(JSON.stringify({result,error,clicks}));
"""
        result = subprocess.run(['node', '-e', harness], input=json.dumps({'names': names, 'source': source, 'url': current_url}),
                                capture_output=True, text=True, timeout=5, check=True)
        return json.loads(result.stdout)

    def test_exact_visible_name_selects_one_and_duplicate_or_missing_never_clicks(self):
        self.assertEqual(self.evaluate_selection(['其他', '中文群'])['clicks'], 1)
        for names in (['中文群', '中文群'], ['其他']):
            result = self.evaluate_selection(names)
            self.assertEqual(result['clicks'], 0)
            self.assertFalse(result['result']['requested'])

    def test_navigation_race_is_rejected_before_clicking(self):
        result = self.evaluate_selection(['中文群'], current_url='https://fixture.feishu.cn/other')
        self.assertEqual(result['clicks'], 0)
        self.assertEqual(result['error'], 'OPSAIL_PAGE_CHANGED')

    def test_teams_uses_exact_catalog_id(self):
        result = self.evaluate_selection(['one', 'two'], provider='teams', identity='19:1')
        self.assertEqual(result['clicks'], 1)
        self.assertEqual(result['result'], {'matched': 1, 'requested': True})

    def test_invalid_selectors_do_not_launch_a_browser(self):
        with patch('chat.browser.load_bindings', side_effect=AssertionError('must not read config')):
            for args in [('prepare', 'feishu', 'name', None), ('select', 'feishu', None, None), ('select', 'feishu', 'name', 'id'), ('prepare', 'wechat', None, None)]:
                with self.assertRaises(ChatError):
                    browser.execute(*args)

    def test_prepare_is_explicit_and_does_not_claim_content_readiness(self):
        with patch('chat.browser.load_bindings', return_value={'feishu': BINDING}), \
             patch('chat.browser.subprocess.run', return_value=subprocess.CompletedProcess([], 0)) as launch, \
             patch('chat.browser._run_osascript', return_value='already-open'):
            result = browser.execute('prepare', 'feishu')
        self.assertEqual(result, {'action': 'already-open', 'contentValidated': False})
        self.assertEqual(launch.call_args.args[0], ['/usr/bin/open', '-g', '-a', 'Google Chrome'])

    def test_selector_failure_is_typed_and_never_opens_browser(self):
        with patch('chat.browser.load_bindings', return_value={'feishu': BINDING}), \
             patch('chat.browser.subprocess.run', side_effect=AssertionError('select must not open')), \
             patch('chat.browser._chrome_tab_javascript', return_value='{"matched":2,"requested":false}'):
            with self.assertRaises(ChatError) as failure:
                browser.execute('select', 'feishu', '中文群')
        self.assertEqual(failure.exception.code, 'conversation-not-unique')

    def test_extension_prepare_accepts_owned_tab_metadata_without_exposing_it(self):
        binding = {'browser': {**BINDING['browser'], 'transport': 'chrome-extension', 'profile_id': 'profile', 'account_hash': 'a' * 64, 'tenant_hash': 'b' * 64, 'build_id': 'c' * 64}}
        with patch('chat.browser.load_bindings', return_value={'feishu': binding}), patch('chat.browser.extension_request', return_value={'action': 'prepared', 'contentValidated': False, 'tabId': 123}):
            self.assertEqual(browser.execute('prepare', 'feishu'), {'action': 'prepared', 'contentValidated': False})
        with patch('chat.browser.load_bindings', return_value={'feishu': binding}), patch('chat.browser.extension_request', return_value={'action': 'prepared', 'contentValidated': False, 'tabId': True}):
            with self.assertRaises(ChatError): browser.execute('prepare', 'feishu')

    def test_quiet_success_is_silent_and_failure_keeps_diagnostic(self):
        for outcome in ({'action': 'already-open', 'contentValidated': False}, ChatError('browser-blocked', 'Script is blocked.')):
            output, error = io.StringIO(), io.StringIO()
            with patch('sys.argv', ['chat', 'prepare', '--provider', 'feishu', '--quiet']), \
                 patch('chat.browser.execute', side_effect=outcome if isinstance(outcome, Exception) else None, return_value=outcome), \
                 contextlib.redirect_stdout(output), contextlib.redirect_stderr(error):
                code = browser.main()
            self.assertEqual(output.getvalue(), '')
            if isinstance(outcome, Exception):
                self.assertEqual(code, 2)
                self.assertEqual(json.loads(error.getvalue())['error']['code'], 'browser-blocked')
            else:
                self.assertEqual(code, 0)
                self.assertEqual(error.getvalue(), '')

    def test_invalid_cli_arguments_never_echo_unknown_sensitive_values(self):
        output, error = io.StringIO(), io.StringIO()
        with patch('sys.argv', ['chat', 'prepare', '--provider', 'feishu', '--token', 'do-not-echo']), \
             contextlib.redirect_stdout(output), contextlib.redirect_stderr(error):
            code = browser.main()
        self.assertEqual(code, 2)
        self.assertEqual(json.loads(error.getvalue())['error']['code'], 'invalid-request')
        self.assertNotIn('do-not-echo', error.getvalue())


if __name__ == '__main__':
    unittest.main()
