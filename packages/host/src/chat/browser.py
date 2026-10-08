#!/usr/bin/env python3
"""Explicit browser preparation; chat status/catalog/read remain read-only.

Only an existing private Chrome binding can be opened. Selection addresses one
visible conversation; it never sends, scrolls, or collects message bodies.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from chat.config import ChatError, load_bindings, validate_binding
from chat.extension import ExtensionError, request as extension_request
from chat.live import _apple_script_string, _chrome_tab_javascript, _run_osascript, catalog


def prepare_script(target: str) -> str:
    return '\n'.join([
        'tell application "Google Chrome"',
        'set matchCount to 0',
        'repeat with theWindow in windows',
        'repeat with theTab in tabs of theWindow',
        f'if URL of theTab is {_apple_script_string(target)} then set matchCount to matchCount + 1',
        'end repeat', 'end repeat',
        'if matchCount > 1 then error "OPSAIL_AMBIGUOUS_MATCHING_TAB"',
        'if matchCount is 1 then return "already-open"',
        'if (count of windows) is 0 then',
        'set targetWindow to make new window',
        f'set URL of active tab of targetWindow to {_apple_script_string(target)}',
        'else',
        'set targetWindow to front window',
        'set previousIndex to active tab index of targetWindow',
        f'make new tab at end of tabs of targetWindow with properties {{URL:{_apple_script_string(target)}}}',
        'set active tab index of targetWindow to previousIndex',
        'end if',
        'return "opened"', 'end tell',
    ])


def selection_script(provider: str, target: str, name: str | None, identity: str | None) -> str:
    request = json.dumps({'provider': provider, 'target': target, 'name': name, 'id': identity}, ensure_ascii=False)
    return "(() => { const request = " + request + ";" + r"""
        if (location.href !== request.target) throw new Error('OPSAIL_PAGE_CHANGED');
        let entries;
        if (request.provider === 'feishu') {
            entries = Array.from(document.querySelectorAll('.a11y_feed_card_item')).map(item => ({
                item, name:(item.querySelector('.a11y_feed_card_main [style*="min-width"]')?.textContent||'').trim()
            }));
        } else {
            entries = Array.from(document.querySelectorAll('[id^="title-chat-list-item_"]')).map(item => ({
                item, name:(item.textContent||'').trim(), id:item.id.slice('title-chat-list-item_'.length)
            }));
        }
        const matches = entries.filter(entry => (!request.name || entry.name === request.name)
            && (!request.id || entry.id === request.id));
        if (matches.length !== 1) return JSON.stringify({matched:matches.length, requested:false});
        matches[0].item.click();
        return JSON.stringify({matched:1, requested:true});
    })()"""


def execute(operation: str, provider: str, name: str | None = None, identity: str | None = None) -> dict:
    if operation not in {'prepare', 'select'} or provider not in {'feishu', 'teams'}:
        raise ChatError('invalid-request', 'Choose prepare/select and feishu/teams.')
    if operation == 'prepare' and (name is not None or identity is not None):
        raise ChatError('invalid-request', 'Browser preparation does not accept a conversation selector.')
    if operation == 'select' and ((name is None) == (identity is None)):
        raise ChatError('invalid-request', 'Select exactly one visible conversation by name or catalog ID.')
    for value in (name, identity):
        if value is not None and (not isinstance(value, str) or not value.strip() or len(value) > 2048 or any(ord(c) < 32 for c in value)):
            raise ChatError('invalid-request', 'Invalid conversation selector.')
    binding = validate_binding(provider, load_bindings().get(provider, {}), 'live-dom')
    browser = binding['browser']
    if browser['transport'] == 'chrome-extension':
        request_args = {}
        if operation == 'select':
            if name is not None:
                request_args['conversationName'] = name
            if identity is not None:
                request_args['conversationId'] = identity
        try:
            data = extension_request({**binding, '_provider': provider}, operation, request_args)
        except ExtensionError as exc:
            raise ChatError(exc.code, exc.public_message) from exc
        if not isinstance(data, dict) or set(data) - {'action', 'contentValidated', 'tabId'} or not isinstance(data.get('action'), str) or not isinstance(data.get('contentValidated'), bool):
            raise ChatError('browser-preparation-failed', 'Chrome extension returned an invalid browser operation result.')
        if 'tabId' in data and (type(data['tabId']) is not int or data['tabId'] < 0):
            raise ChatError('browser-preparation-failed', 'Chrome extension returned an invalid owned tab ID.')
        return {'action': data['action'], 'contentValidated': data['contentValidated']}
    if browser['transport'] != 'chrome-tab':
        raise ChatError('browser-preparation-unavailable', 'This helper requires an exact chrome-tab binding.')
    if operation == 'prepare':
        # -g launches the existing app without bringing it to the foreground.
        result = subprocess.run(['/usr/bin/open', '-g', '-a', 'Google Chrome'], stdin=subprocess.DEVNULL,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10, check=False)
        if result.returncode != 0:
            raise ChatError('browser-unavailable', 'Google Chrome could not be opened in the background.')
        action = _run_osascript('Google Chrome', prepare_script(browser['target_url']))
        if action not in {'opened', 'already-open'}:
            raise ChatError('browser-preparation-failed', 'Browser preparation returned an invalid result.')
        return {'action': action, 'contentValidated': False}
    if provider == 'feishu' and identity is not None:
        entries = catalog(provider, binding, {'mode': 'live-dom', 'limit': 100})['entries']
        matches = [entry for entry in entries if entry['conversationId'] == identity]
        if len(matches) != 1:
            raise ChatError('conversation-not-found', 'Catalog ID no longer matches one visible conversation.')
        name, identity = matches[0]['conversationName'], None
    raw = _chrome_tab_javascript(browser['target_url'], selection_script(provider, browser['target_url'], name, identity))
    try:
        selected = json.loads(raw)
    except ValueError:
        raise ChatError('browser-selection-failed', 'Browser selection returned an invalid result.') from None
    if selected != {'matched': 1, 'requested': True}:
        raise ChatError('conversation-not-unique', 'The requested conversation is missing or ambiguous in the visible list.')
    return {'action': 'selection-requested', 'contentValidated': False}


class _Parser(argparse.ArgumentParser):
    def error(self, message):
        raise ChatError('invalid-request', 'Invalid browser preparation arguments. Use --help.')


def main() -> int:
    parser = _Parser(description=__doc__)
    parser.add_argument('operation', choices=['prepare', 'select'])
    parser.add_argument('--provider', required=True, choices=['feishu', 'teams'])
    parser.add_argument('--conversation-name')
    parser.add_argument('--conversation-id')
    parser.add_argument('--quiet', action='store_true')
    quiet = False
    try:
        args = parser.parse_args()
        quiet = args.quiet
        data = execute(args.operation, args.provider, args.conversation_name, args.conversation_id)
        result = {'schemaVersion': 1, 'operation': 'browser-' + args.operation, 'provider': args.provider, 'exitCode': 0, 'data': data}
    except Exception as error:
        result = {'schemaVersion': 1, 'exitCode': 2, 'error': {
            'code': error.code if isinstance(error, ChatError) else 'browser-preparation-failed',
            'message': error.public_message if isinstance(error, ChatError) else 'Browser preparation could not complete.'}}
    if result['exitCode'] or not quiet:
        print(json.dumps(result, ensure_ascii=False), file=sys.stderr if result['exitCode'] else sys.stdout)
    return result['exitCode']


if __name__ == '__main__':
    raise SystemExit(main())
