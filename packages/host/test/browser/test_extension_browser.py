"""Real Chromium MV3 checks using synthetic pages and an isolated profile only."""
import hashlib
import json
from pathlib import Path
import shutil
import tempfile
import unittest

from playwright.sync_api import expect, sync_playwright


class ExtensionBrowserTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix='opsail-extension-test-')
        root = Path(cls.temp.name)
        cls.extension = root / 'extension'
        shutil.copytree((Path(__file__).resolve().parent / '../../src/browser/extension').resolve(), cls.extension)
        manifest = json.loads((cls.extension / 'manifest.json').read_text())
        # The copied fixture must have no path to a user's registered native
        # host, even on machines that also have the production extension.
        manifest['permissions'].remove('nativeMessaging')
        manifest.pop('key', None)
        # Explicitly test-only site grant. Production retains optional exact-origin grants.
        manifest['host_permissions'] = ['https://fixture.feishu.cn/*', 'https://teams.microsoft.com/*']
        (cls.extension / 'manifest.json').write_text(json.dumps(manifest))
        (cls.extension / 'build.json').write_text(json.dumps({'protocolVersion': 1, 'extensionVersion': '0.1.0', 'buildId': hashlib.sha256(b'fixture').hexdigest()}))
        cls.pw = sync_playwright().start()
        cls.context = cls.pw.chromium.launch_persistent_context(str(root / 'profile'), channel='chromium', headless=True,
            args=[f'--disable-extensions-except={cls.extension}', f'--load-extension={cls.extension}'])
        cls.worker = cls.context.service_workers[0] if cls.context.service_workers else cls.context.wait_for_event('serviceworker')
        cls.control = cls.context.new_page()
        cls.control.goto(cls.worker.url.rsplit('/', 1)[0] + '/options.html')

    @classmethod
    def tearDownClass(cls):
        cls.context.close()
        cls.pw.stop()
        cls.temp.cleanup()

    def evaluate(self, html, provider='feishu', operation='read', args=None, expected_identity=None, settle=False):
        url = 'https://fixture.feishu.cn/messenger' if provider == 'feishu' else 'https://teams.microsoft.com/v2/'
        page = self.context.new_page()
        if provider == 'feishu':
            # Synthetic contract fixture: DOM identity alone no longer bypasses
            # the Feishu build and session-user verification gates.
            html += '''<script src="https://sf1-scmcdn-cn.feishucdn.com/static/js/index.745e4057.js"></script>
            <script>
            const identity=document.querySelector('[data-user-id][data-tenant-id]');
            window.userId=identity?.getAttribute('data-user-id')||'';
            window.configurationAdapter={passport:{userId:window.userId,getCurUserInfo:async()=>({user:{id:window.userId,tenant:{id:identity?.getAttribute('data-tenant-id')||''}}})}};
            </script>'''
        page.route('**/*', lambda route: route.fulfill(content_type='text/html; charset=utf-8', body=html))
        page.route('https://sf1-scmcdn-cn.feishucdn.com/**', lambda route: route.fulfill(content_type='application/javascript', body='/* synthetic build */'))
        page.goto(url)
        packet = self.control.evaluate('''async ({url,provider,operation,args,expectedIdentity,settle}) => {
            const tab=(await chrome.tabs.query({})).find(t => t.url===url);
            const {inspectPage}=await import(chrome.runtime.getURL('providers/dom.js'));
            const read = async (op=operation, selected=args) => {
                const result = (await chrome.scripting.executeScript({target:{tabId:tab.id},world:'MAIN',func:inspectPage,args:[provider,op,selected,new URL(url).origin,expectedIdentity]}))[0].result;
                if (settle && result.error) throw Object.assign(new Error(result.error.code), {code:result.error.code});
                return result;
            };
            if (!settle) return read();
            const {settledPane}=await import(chrome.runtime.getURL('providers/settled-pane.js'));
            if (operation==='select') {
                const selected=await read();
                await settledPane(()=>read('selectionReady',{conversationId:selected.conversationId}),{deadline:Date.now()+2000});
                return {...selected,contentValidated:true};
            }
            return settledPane(()=>read(),{deadline:Date.now()+2000});
        }''', {'url': url, 'provider': provider, 'operation': operation, 'args': args or {'conversationId': 'c1'}, 'expectedIdentity': expected_identity or {}, 'settle': settle})
        if operation == 'select':
            packet['clicked'] = page.locator('body').get_attribute('data-clicked')
        page.close()
        return packet

    def test_feishu_identity_and_selected_messages_atomic(self):
        html = '''<div data-user-id="u1" data-tenant-id="t1"></div>
        <div class="a11y_feed_card_item active" data-chat-id="c1"><div class="a11y_feed_card_main"><span style="min-width:0">Work</span></div></div>
        <main data-message-conversation-id="c1"><div class="messageItem-wrapper" data-id="m1"><span class="message-info-name">A</span><time datetime="2026-09-15T02:00:00Z"></time><div class="message-text">fixture body</div></div></main>'''
        result = self.evaluate(html)
        self.assertEqual(result['conversationId'], 'c1')
        self.assertEqual(result['messages'][0]['text'], 'fixture body')
        self.assertEqual(self.evaluate(html.replace('item active', 'item'))['error']['code'], 'conversation-not-selected')
        self.assertEqual(self.evaluate(html.replace('data-user-id="u1"', ''))['error']['code'], 'identity-unverified')
        self.assertEqual(self.evaluate(html.replace('data-chat-id="c1"', ''))['error']['code'], 'conversation-not-unique')
        self.assertEqual(self.evaluate(html.replace('data-message-conversation-id="c1"', 'data-message-conversation-id="previous-chat"'))['error']['code'], 'message-scope-unverified')

    def test_feishu_rendered_author_nodes_and_multiple_day_separators(self):
        html = '''<div data-user-id="u1" data-tenant-id="t1"></div>
        <div class="a11y_feed_card_item active" data-chat-id="c1"><div class="a11y_feed_card_main"><span style="min-width:0">Work</span></div></div>
        <main data-message-conversation-id="c1">
          <div class="message-time-separator">2026年9月23日</div>
          <div class="messageItem-wrapper" data-id="m1"><span class="message-info__name">Ada</span><span class="message-layout-time-tip">09:55</span><div class="message-text">first</div></div>
          <div class="message-date-separator">2026-09-24</div>
          <div class="messageItem-wrapper message-self" data-id="m2"><span class="message-sender-name">Operator</span><span class="message-layout-time-tip">10:16</span><div class="message-text">second</div></div>
          <div class="message-date-separator">September</div>
          <div class="messageItem-wrapper" data-id="m3"><span class="message-info-name" title="Third"> </span><span class="message-layout-time-tip">10:17</span><div class="message-text">third<blockquote><span class="message-info-name">Quoted</span></blockquote></div></div>
        </main>'''
        result = self.evaluate(html)
        self.assertEqual([row['sender'] for row in result['messages']], ['Ada', 'Operator', 'Third'])
        self.assertEqual([row['time'] for row in result['messages']], ['2026-09-23T09:55:00', '2026-09-24T10:16:00', '10:17'])
        self.assertEqual(result['messages'][1]['is_self'], True)
        self.assertEqual(result['messages'][2]['is_self'], None)
        self.assertEqual(result['messages'][2]['date_status'], 'unknown')
        self.assertNotIn('Ada', json.dumps(result['diagnostics']))
        limited = self.evaluate(html, args={'conversationId': 'c1', 'limit': 2})
        self.assertEqual(limited['messages'][0]['time'], '2026-09-24T10:16:00')

    def test_feishu_delayed_render_waits_for_verified_pane_in_real_dom(self):
        html = '''<div data-user-id="u1" data-tenant-id="t1"></div>
        <div class="a11y_feed_card_item active" data-chat-id="c1"><div class="a11y_feed_card_main"><span style="min-width:0">Work</span></div></div>
        <main data-message-conversation-id="old"><div class="messageItem-wrapper" data-id="old"><div class="message-text">OLD BODY</div></div></main>
        <script>setTimeout(()=>{const pane=document.querySelector('main');pane.dataset.messageConversationId='c1';pane.innerHTML='<div class="message-date-separator">2026-09-24</div><div class="messageItem-wrapper" data-id="m1"><span class="message-info__name">Ada</span><span class="message-layout-time-tip">09:55</span><div class="message-text">READY BODY</div></div>';},350);</script>'''
        result = self.evaluate(html, settle=True)
        self.assertEqual(result['messages'][0]['id'], 'm1')
        self.assertEqual(result['messages'][0]['text'], 'READY BODY')
        selected = self.evaluate(html, operation='select', settle=True)
        self.assertTrue(selected['contentValidated'])
        self.assertNotIn('messages', selected)

    def test_teams_media_and_duplicate_conversation_rejected(self):
        card = '<div aria-selected="true"><span id="title-chat-list-item_c1">Work</span></div>'
        html = '<div data-user-id="u1" data-tenant-id="t1"></div>' + card + '''<main data-message-conversation-id="c1"><div data-tid="chat-pane-item"><span data-tid="message-author-name">A</span><time datetime="2026-09-15T03:00:00Z"></time><div data-tid="chat-pane-message" data-mid="m1">fixture<img data-tid="lazy-image-2" alt="image" /></div></div></main>'''
        result = self.evaluate(html, 'teams')
        self.assertEqual(result['messages'][0]['media'][0]['type'], 'image')
        self.assertEqual(set(result['messages'][0]['media'][0]), {'type', 'locator', 'alt_text'})
        self.assertEqual(self.evaluate(html + card, 'teams')['error']['code'], 'conversation-not-unique')

    def test_account_change_rejected_before_selection(self):
        html = '''<div data-user-id="u1" data-tenant-id="t1"></div>
        <div class="a11y_feed_card_item" data-chat-id="c1" onclick="document.body.dataset.clicked='yes'"><div class="a11y_feed_card_main"><span style="min-width:0">Work</span></div></div>'''
        changed = self.evaluate(html, operation='select', expected_identity={'accountHash': 'a' * 64})
        self.assertEqual(changed['error']['code'], 'account-changed')
        self.assertIsNone(changed['clicked'])
        actual = hashlib.sha256(b'feishu\0https://fixture.feishu.cn\0u1').hexdigest()
        bound = self.evaluate(html, operation='select', expected_identity={'accountHash': actual})
        self.assertEqual(bound['action'], 'selection-requested')
        self.assertEqual(bound['clicked'], 'yes')

    def test_feishu_source_feed_ids_require_independent_agreement(self):
        html = '''<div data-user-id="u1" data-tenant-id="t1"></div>
        <div class="a11y_feed_card_item"><div class="a11y_feed_card_main"><span style="min-width:0">Same name</span></div></div>
        <div class="a11y_feed_card_item"><div class="a11y_feed_card_main"><span style="min-width:0">Same name</span></div></div>
        <script>
        window.__feedStore={getState:()=>({status:{activeFeedId:'c1'}})};
        document.querySelectorAll('.a11y_feed_card_item').forEach((node,i)=>{
          node.__reactInternalInstance$fixture={memoizedProps:{},return:{memoizedProps:{value:{isActive:i===0}},return:{memoizedProps:{value:{feedId:'c'+(i+1)}},return:{memoizedProps:{feedId:'c'+(i+1)}}}}};
        });
        </script>'''
        result = self.evaluate(html, operation='catalog')
        self.assertEqual([entry['conversationId'] for entry in result['entries']], ['c1', 'c2'])
        self.assertFalse(result['complete'])
        self.assertEqual(result['diagnostics']['selectedCount'], 1)
        conflicting = html.replace("memoizedProps:{feedId:'c'+(i+1)}", "memoizedProps:{feedId:'wrong'}")
        self.assertEqual(self.evaluate(conflicting, operation='catalog')['error']['code'], 'conversation-identity-unverified')
        duplicate = html.replace("'c'+(i+1)", "'c1'")
        self.assertEqual(self.evaluate(duplicate, operation='catalog')['error']['code'], 'conversation-identity-unverified')
        self.assertEqual(self.evaluate(html, args={'conversationName': 'Same name'})['error']['code'], 'conversation-not-unique')

    def test_teams_uses_committed_alternate_after_selection(self):
        html = """<div data-user-id="u1" data-tenant-id="t1"></div>
        <span id="title-chat-list-item_c1">Work</span>
        <main data-message-conversation-id="c1"><div data-tid="chat-pane-item"><div data-tid="chat-pane-message" data-mid="m1">current body</div></div></main>
        <script>
        const oldRoot={},newRoot={},root={current:newRoot};
        oldRoot.alternate=newRoot;newRoot.alternate=oldRoot;oldRoot.stateNode=newRoot.stateNode=root;
        const oldRow={memoizedProps:{conversation:{internalId:'c1',isSelected:false}},return:oldRoot};
        const newRow={memoizedProps:{conversation:{internalId:'c1',isSelected:true}},return:newRoot};
        oldRow.alternate=newRow;newRow.alternate=oldRow;oldRoot.child=oldRow;newRoot.child=newRow;
        document.querySelector('span').__reactFiber$fixture=oldRow;
        </script>"""
        result = self.evaluate(html, 'teams')
        self.assertEqual(result['messages'][0]['text'], 'current body')
        stale = self.evaluate(html.replace('current:newRoot', 'current:oldRoot'), 'teams')
        self.assertEqual(stale['error']['code'], 'conversation-not-selected')

    def test_options_runs_in_real_extension(self):
        extension_id = self.worker.url.split('/')[2]
        page = self.context.new_page()
        page.goto(f'chrome-extension://{extension_id}/options.html')
        page.locator('#pause').click()
        expect(page.locator('#state')).to_have_text('已暂停')
        page.locator('#pause').click()
        expect(page.locator('#state')).to_have_text('等待 CLI 请求')
        page.close()

    def test_actual_feishu_message_props_reject_recycled_or_stale_pane(self):
        html = '''<div data-user-id="u1" data-tenant-id="t1"></div>
        <div class="a11y_feed_card_item active" data-chat-id="c1"><div class="a11y_feed_card_main"><span style="min-width:0">Work</span></div></div>
        <div class="messageItem-wrapper" data-id="m1"><div class="message-text">bounded text</div></div>
        <script>document.querySelector('.messageItem-wrapper').__reactInternalInstance$fixture={memoizedProps:{},return:{memoizedProps:{messageId:'m1'},return:{memoizedProps:{chatId:'c1'},return:{memoizedProps:{id:'c1',messages:[]}}}}};</script>'''
        self.assertEqual(self.evaluate(html)['messages'][0]['id'], 'm1')
        self.assertEqual(self.evaluate(html.replace("id:'c1',messages", "id:'previous',messages"))['error']['code'], 'message-scope-unverified')
        self.assertEqual(self.evaluate(html.replace("messageId:'m1'", "messageId:'recycled'"))['error']['code'], 'message-scope-unverified')

    def test_actual_teams_message_props_bind_internal_conversation_id(self):
        html = '''<div data-user-id="u1" data-tenant-id="t1"></div>
        <span id="title-chat-list-item_c1">Work</span>
        <div data-tid="chat-pane-item" data-mid="m1"><div data-tid="chat-pane-message">bounded text</div></div>
        <script>
        document.querySelector('span').__reactFiber$fixture={memoizedProps:{conversation:{id:'Conversation:c1',internalId:'c1',isSelected:true}}};
        document.querySelector('[data-tid="chat-pane-message"]').__reactFiber$fixture={memoizedProps:{convId:'c1',mine:true,message:{id:'m1',originalArrivalTime:'2026-09-15T03:00:00Z'}},return:{memoizedProps:{chat:{id:'c1'},resolvedConvId:'c1'}}};
        </script>'''
        result = self.evaluate(html, 'teams')
        self.assertEqual(result['messages'][0]['id'], 'm1')
        self.assertTrue(result['messages'][0]['is_self'])
        self.assertEqual(result['messages'][0]['time'], '2026-09-15T03:00:00Z')
        self.assertEqual(self.evaluate(html.replace("resolvedConvId:'c1'", "resolvedConvId:'previous'"), 'teams')['error']['code'], 'message-scope-unverified')
        self.assertEqual(self.evaluate(html.replace("internalId:'c1'", "internalId:'other'"), 'teams')['error']['code'], 'conversation-not-unique')

    def test_teams_authenticated_context_uses_current_tenant_and_rejects_conflicts(self):
        html = '''<div data-tid="chat-pane-message"></div><script>
        const first={user:{id:'opaque-context-id'},authenticationUser:{isAuthenticated:true,homeTenantId:'home-not-current',profile:{objectId:'current-object',tenantId:'current-tenant'}}};
        const second={user:{id:'opaque-context-id'},authenticationUser:{isAuthenticated:true,profile:{objectId:'current-object',tenantId:'current-tenant'}}};
        document.querySelector('div').__reactFiber$fixture={memoizedProps:{userContext:first,conversationTenantId:'external-conversation-tenant'},return:{memoizedProps:{userContext:second}}};
        </script>'''
        result = self.evaluate(html, 'teams', operation='status')
        self.assertEqual(result['identity'], {'accountId': 'current-object', 'tenantId': 'current-tenant'})
        self.assertEqual(result['diagnostics']['identityKind'], 'teams-authenticated-context')
        self.assertNotIn('home-not-current', json.dumps(result))
        self.assertNotIn('opaque-context-id', json.dumps(result))
        changed = html.replace("objectId:'current-object',tenantId:'current-tenant'}}};\n        document", "objectId:'other',tenantId:'current-tenant'}}};\n        document")
        self.assertFalse(self.evaluate(changed, 'teams', operation='status')['diagnostics']['identityPresent'])
        self.assertFalse(self.evaluate(html.replace('isAuthenticated:true', 'isAuthenticated:false'), 'teams', operation='status')['diagnostics']['identityPresent'])

    def test_teams_selection_clicks_the_navigation_title_after_identity_check(self):
        html = '''<div data-user-id="u1" data-tenant-id="t1"></div>
        <div role="treeitem" ><span id="title-chat-list-item_c1" onclick="document.body.dataset.clicked='yes'">Work</span></div>'''
        selected = self.evaluate(html, 'teams', operation='select')
        self.assertEqual(selected['clicked'], 'yes')
        blocked = self.evaluate(html, 'teams', operation='select', expected_identity={'accountHash': 'a' * 64})
        self.assertIsNone(blocked['clicked'])
        self.assertEqual(blocked['error']['code'], 'account-changed')

    def test_bootstrap_diagnostics_only_export_shapes(self):
        page = self.context.new_page()
        page.route('**/*', lambda route: route.fulfill(content_type='text/html', body='''<script>
        window.initialState={user:{id:'private-account-value'},accessToken:'never-export', 'someone@example.com':{secret:'hidden'}};
        Object.defineProperty(window.initialState,'getter',{get(){throw new Error('getter must not run')}});
        </script>'''))
        page.goto('https://fixture.feishu.cn/probe')
        result = self.control.evaluate('''async () => {
          const tab=(await chrome.tabs.query({})).find(t=>t.url==='https://fixture.feishu.cn/probe');
          const {inspectBootstrap}=await import(chrome.runtime.getURL('providers/probe.js'));
          return (await chrome.scripting.executeScript({target:{tabId:tab.id},world:'MAIN',func:inspectBootstrap,args:['https://fixture.feishu.cn']}))[0].result;
        }''')
        serialized = json.dumps(result)
        self.assertIn('initialState', serialized)
        self.assertIn('accessor', serialized)
        for private in ['private-account-value', 'never-export', 'accessToken', 'someone@example.com']:
            self.assertNotIn(private, serialized)
        page.close()

    def test_feishu_verified_session_identity_does_not_export_auth_fields(self):
        page = self.context.new_page()
        page.route('**/*', lambda route: route.fulfill(content_type='text/html', body='''
        <script src="https://sf1-scmcdn-cn.feishucdn.com/obj/feishu-static/ee/web-client-next/p/static/js/index.745e4057.js"></script>
        <script>window.userId='fixture-user';window.configurationAdapter={passport:{userId:'fixture-user',getCurUserInfo:async()=>({user:{id:'fixture-user',tenant:{id:'fixture-tenant'}},accessToken:'never-export'})}};</script>'''))
        page.route('https://sf1-scmcdn-cn.feishucdn.com/**', lambda route: route.fulfill(content_type='application/javascript', body='/* public app fixture */'))
        page.goto('https://fixture.feishu.cn/session')
        result = self.control.evaluate('''async () => {
          const tab=(await chrome.tabs.query({})).find(t=>t.url==='https://fixture.feishu.cn/session');
          const {inspectPage}=await import(chrome.runtime.getURL('providers/dom.js'));
          return (await chrome.scripting.executeScript({target:{tabId:tab.id},world:'MAIN',func:inspectPage,args:['feishu','status',{},'https://fixture.feishu.cn',{}]}))[0].result;
        }''')
        self.assertEqual(result['identity'], {'accountId': 'fixture-user', 'tenantId': 'fixture-tenant'})
        self.assertEqual(result['diagnostics']['identityKind'], 'feishu-session-user')
        self.assertNotIn('never-export', json.dumps(result))
        page.evaluate("window.userId='changed'")
        changed = self.control.evaluate('''async () => {
          const tab=(await chrome.tabs.query({})).find(t=>t.url==='https://fixture.feishu.cn/session');
          const {inspectPage}=await import(chrome.runtime.getURL('providers/dom.js'));
          return (await chrome.scripting.executeScript({target:{tabId:tab.id},world:'MAIN',func:inspectPage,args:['feishu','status',{},'https://fixture.feishu.cn',{}]}))[0].result;
        }''')
        self.assertEqual(changed['error']['code'], 'identity-unverified')
        self.assertFalse(changed['diagnostics']['behavioralContractPassed'])
        page.close()


if __name__ == '__main__':
    unittest.main()
