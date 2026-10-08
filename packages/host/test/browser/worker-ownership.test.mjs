import assert from 'node:assert/strict'
import {randomUUID} from 'node:crypto'
import test from 'node:test'
import {cpSync, mkdtempSync, rmSync, writeFileSync} from 'node:fs'
import {tmpdir} from 'node:os'
import path from 'node:path'
import {pathToFileURL} from 'node:url'

const binding = {targetUrl: 'https://tenant.feishu.cn/next/messenger/', allowedOrigins: ['https://tenant.feishu.cn']}
const event = () => { const listeners = []; return {addListener: fn => listeners.push(fn), clear: () => { listeners.length = 0 }, fire: async (...args) => Promise.all(listeners.map(fn => fn(...args)))} }

function chromeFixture(tabStatus = 'complete') {
  const local = {}; const session = {}; const tabs = new Map(); const documents = new Map(); let nextTab = 10; const windows = new Set([1]); let nextWindow = 2; const minimized = new Set()
  const createTab = ({windowId, url, active}) => { const id = nextTab++; tabs.set(id, {id, windowId, url, active, discarded: false, status: tabStatus}); documents.set(id, `doc-${id}`); return {...tabs.get(id)} }
  const activated = event(); const removed = event(); const nativeMessage = event(); const responses = []; let reloads = 0
  const storage = area => ({
    get: async keys => {
      const source = area === 'local' ? local : session
      if (keys == null) return structuredClone(source)
      if (typeof keys === 'string') return {[keys]: structuredClone(source[keys])}
      if (Array.isArray(keys)) return Object.fromEntries(keys.map(key => [key, structuredClone(source[key])]))
      return Object.fromEntries(Object.entries(keys).map(([key, value]) => [key, structuredClone(source[key] ?? value)]))
    },
    set: async value => Object.assign(area === 'local' ? local : session, value),
    remove: async key => { delete (area === 'local' ? local : session)[key] },
  })
  globalThis.chrome = {
    runtime: {getURL: file => `chrome-extension://test/${file}`, getManifest: () => ({version: '0.1.0'}), connectNative: () => ({onMessage: nativeMessage, onDisconnect: event(), postMessage: value => responses.push(value)}), reload: () => { reloads++ }, onMessage: event(), onInstalled: event(), onStartup: event(), lastError: null},
    storage: {local: storage('local'), session: storage('session')},
    permissions: {contains: async () => true}, alarms: {create: () => {}, onAlarm: event()},
    windows: {getAll: async () => [{id: 1, focused: true}], get: async id => { if (!windows.has(id)) throw new Error('missing'); return {id, type: 'normal', state: minimized.has(id) ? 'minimized' : 'normal'} },
      update: async (id, {state, focused}) => { assert.equal(focused, false); if (state === 'normal') minimized.delete(id); return {id} },
      create: async ({url, tabId} = {}) => {
        const id = nextWindow++; windows.add(id)
        if (tabId !== undefined) { Object.assign(tabs.get(tabId), {windowId: id, active: true}); return {id, tabs: [{...tabs.get(tabId)}]} }
        return {id, tabs: [createTab({windowId: id, url, active: true})]}
      }},
    tabs: {
      query: async ({url}) => [...tabs.values()].filter(tab => tab.url.startsWith(url.slice(0, -1))),
      get: async id => { if (!tabs.has(id)) throw new Error('missing'); return {...tabs.get(id)} },
      create: async args => createTab(args),
      remove: async id => { tabs.delete(id); documents.delete(id); await removed.fire(id) },
      move: async (id, {windowId}) => { tabs.get(id).windowId = windowId; return {...tabs.get(id)} },
      update: async (id, {active}) => { tabs.get(id).active = active; return {...tabs.get(id)} },
      onActivated: activated, onRemoved: removed,
    },
    scripting: {executeScript: async ({target}) => [{documentId: documents.get(target.tabId), result: {origin: new URL(tabs.get(target.tabId).url).origin, readyState: 'interactive'}}]},
  }
  globalThis.fetch = async () => ({json: async () => ({protocolVersion: 1, buildId: 'a'.repeat(64)})})
  return {local, session, tabs, documents, activated, removed, nativeMessage, responses, minimized, reloads: () => reloads}
}

// Like an installed release: the worker imports a generated build.js identity.
const installed = mkdtempSync(path.join(tmpdir(), 'opsail-ext-'))
cpSync(new URL('../../src/browser/extension/', import.meta.url), installed, {recursive: true})
writeFileSync(path.join(installed, 'build.js'), `export const BUILD = Object.freeze(${JSON.stringify({protocolVersion: 1, extensionVersion: '0.1.0', buildId: 'a'.repeat(64)})})\n`)
process.on('exit', () => rmSync(installed, {recursive: true, force: true}))
async function worker() { return import(`${pathToFileURL(path.join(installed, 'worker.js')).href}?ownership=${randomUUID()}`) }

test('created tab is persisted only after final bound document is identified', async () => {
  const fixture = chromeFixture(); const {ownedTab} = await worker()
  const tab = await ownedTab('feishu', binding, true)
  const record = fixture.local.ownedTabs.feishu
  assert.equal(record.tabId, tab.id); assert.equal(record.documentId, `doc-${tab.id}`)
  assert.deepEqual(record, {tabId: tab.id, documentId: `doc-${tab.id}`, ...binding})
  // Owned tabs open in one unfocused Opsail window, never in the user's window 1.
  assert.notEqual(tab.windowId, 1); assert.equal(fixture.session.opsailWindowId, tab.windowId)
  const second = await ownedTab('teams', {targetUrl: 'https://teams.microsoft.com/v2/', allowedOrigins: ['https://teams.microsoft.com']}, true)
  assert.equal(second.windowId, tab.windowId); assert.equal(second.active, false)
})

test('loading tab with a final in-page origin can be owned, but initial document origin cannot', async () => {
  const loading = chromeFixture('loading'); const loadingWorker = await worker()
  await loadingWorker.ownedTab('feishu', binding, true)
  assert.equal(loading.local.ownedTabs.feishu.documentId, 'doc-10')
  const initial = chromeFixture(); initial.tabs.set(44, {id: 44, url: binding.targetUrl, status: 'loading', active: false, discarded: false}); initial.documents.set(44, 'initial-doc')
  chrome.scripting.executeScript = async () => [{documentId: 'initial-doc', result: {origin: 'about:blank', readyState: 'loading'}}]
  const initialWorker = await worker()
  assert.deepEqual(await initialWorker.documentIdFor(44, binding), {documentId: null, reason: 'origin-mismatch'})
})

test('reload recovery accepts same document and rejects document or binding changes', async () => {
  const fixture = chromeFixture(); const first = await worker(); const tab = await first.ownedTab('feishu', binding, true)
  const reloaded = await worker(); assert.equal((await reloaded.ownedTab('feishu', binding)).id, tab.id)
  delete fixture.session[`createdOwnedTab:${tab.id}`] // extension/browser restart clears session creation proof
  fixture.documents.set(tab.id, 'different-document')
  await assert.rejects(reloaded.ownedTab('feishu', binding), {code: 'tab-not-prepared'})
  assert.equal(fixture.local.ownedTabs.feishu, undefined)
  // A document mismatch loses ownership; matching origin must not authorize
  // adoption or deletion. The operator must close that now-unowned tab.
  await assert.rejects(reloaded.ownedTab('feishu', binding, true), {code: 'target-tab-conflict'})
  assert.equal(fixture.tabs.has(tab.id), true)
  await chrome.tabs.remove(tab.id)
  const second = await reloaded.ownedTab('feishu', binding, true)
  await assert.rejects(reloaded.ownedTab('feishu', {...binding, targetUrl: 'https://tenant.feishu.cn/other/'}), {code: 'tab-not-prepared'})
  assert.equal(fixture.local.ownedTabs.feishu, undefined)
  assert.notEqual(second.id, tab.id)
})

test('active owned tabs in a focused window still support reads after worker reload', async () => {
  for (const provider of ['feishu', 'teams']) {
    const f = chromeFixture(); const target = providerBinding(provider)
    const {ownedTab} = await worker(); const tab = await ownedTab(provider, target, true)
    f.tabs.get(tab.id).active = true
    const before = structuredClone(f.local.ownedTabs)
    await f.activated.fire({tabId: tab.id, windowId: 1})
    assert.deepEqual(f.local.ownedTabs, before)
    f.nativeMessage.clear() // the old worker's native connection ends on reload
    const reloaded = await worker()
    assert.equal((await reloaded.ownedTab(provider, target)).id, tab.id)
    await assertReadable(f, provider, target, tab.id)
    await chrome.tabs.remove(tab.id)
    assert.equal(f.local.ownedTabs[provider], undefined)
  }
})

test('activation during initial document capture still establishes usable ownership', async () => {
  const fixture = chromeFixture(); const original = chrome.scripting.executeScript
  let release; const gate = new Promise(resolve => { release = resolve })
  chrome.scripting.executeScript = async value => { await gate; return original(value) }
  const {ownedTab} = await worker(); const creating = ownedTab('feishu', binding, true)
  await new Promise(resolve => setImmediate(resolve))
  fixture.tabs.get(10).active = true
  await fixture.activated.fire({tabId: 10, windowId: 1}); release()
  assert.equal((await creating).id, 10)
  assert.equal(fixture.tabs.has(10), true)
  assert.equal(fixture.local.ownedTabs.feishu.documentId, 'doc-10')
  await assertReadable(fixture, 'feishu', binding, 10)
})

test('legacy takenOver records and session flags do not block owned reads or doctor', async () => {
  for (const provider of ['feishu', 'teams']) {
    const f = chromeFixture(); const target = providerBinding(provider)
    const tab = {id: 9, windowId: 1, url: target.targetUrl, active: true, discarded: false}
    f.tabs.set(tab.id, tab); f.documents.set(tab.id, 'legacy-document')
    f.local.ownedTabs = {[provider]: {...target, tabId: tab.id, documentId: 'legacy-document', takenOver: true}}
    f.session[`createdOwnedTab:${tab.id}`] = {provider, ...target, takenOver: true}
    f.session[`creatingTakenOver:${provider}`] = true
    const {ownedTab} = await worker()
    assert.equal((await ownedTab(provider, target)).id, tab.id)
    await assertReadable(f, provider, target, tab.id)
    // Ignoring an obsolete flag never grants ownership to a changed document.
    delete f.session[`createdOwnedTab:${tab.id}`]
    f.documents.set(tab.id, 'different-document')
    await assert.rejects(ownedTab(provider, target), {code: 'tab-not-prepared'})
    assert.equal(f.tabs.has(tab.id), true)
  }
})

test('legacy creatingTakenOver session flags do not block a newly created active tab', async () => {
  const f = chromeFixture(); const original = chrome.tabs.create
  f.session['creatingTakenOver:feishu'] = true
  chrome.tabs.create = async args => {
    assert.equal(args.active, false)
    const tab = await original(args)
    f.tabs.get(tab.id).active = true
    return {...tab, active: true}
  }
  const {ownedTab} = await worker(); const tab = await ownedTab('feishu', binding, true)
  await assertReadable(f, 'feishu', binding, tab.id)
})

test('simultaneous provider preparation keeps both persistent ownership records', async () => {
  const fixture = chromeFixture(); const {ownedTab} = await worker()
  await Promise.all([ownedTab('feishu', binding, true), ownedTab('teams', {targetUrl: 'https://teams.microsoft.com/v2/', allowedOrigins: ['https://teams.microsoft.com']}, true)])
  assert.deepEqual(Object.keys(fixture.local.ownedTabs).sort(), ['feishu', 'teams'])
})


test('build-changed reloads new stable files once and refuses an old unpacked path', async () => {
  const fixture = chromeFixture(); await worker(); await new Promise(resolve => setImmediate(resolve));
  const profile = fixture.local.profileId;
  await fixture.nativeMessage.fire({type: 'build-changed', buildId: 'b'.repeat(64)});
  assert.equal(fixture.reloads(), 0); assert.equal(fixture.session.diagnostic, 'load-stable-extension');
  globalThis.fetch = async () => ({json: async () => ({protocolVersion: 1, buildId: 'b'.repeat(64)})});
  await fixture.nativeMessage.fire({type: 'build-changed', buildId: 'b'.repeat(64)});
  assert.equal(fixture.reloads(), 1); assert.equal(fixture.local.profileId, profile);
  await fixture.nativeMessage.fire({type: 'build-changed', buildId: 'b'.repeat(64)});
  assert.equal(fixture.reloads(), 1);
  await fixture.nativeMessage.fire({type: 'build-changed', buildId: 'raw-provider-text'});
  assert.equal(fixture.reloads(), 1);
});


test('doctor diagnoses permission and explicit pause without creating tabs or changing storage', async () => {
  const fixture = chromeFixture(); await worker(); await new Promise(resolve => setImmediate(resolve));
  chrome.permissions.contains = async () => false;
  const before = structuredClone(fixture.local);
  await fixture.nativeMessage.fire({type: 'request', protocolVersion: 1, requestId: 'doctor-permission', operation: 'diagnose', provider: 'feishu', args: {}, binding});
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(fixture.responses.at(-1).data.sitePermission, false);
  assert.deepEqual(fixture.local, before); assert.equal(fixture.tabs.size, 0);
  chrome.permissions.contains = async () => true;
  fixture.local.paused = true;
  const paused = structuredClone(fixture.local);
  await fixture.nativeMessage.fire({type: 'request', protocolVersion: 1, requestId: 'doctor-paused', operation: 'diagnose', provider: 'feishu', args: {}, binding});
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(fixture.responses.at(-1).data, {sitePermission: true, paused: true, identityVerified: false, identityCode: 'not-probed'});
  assert.deepEqual(fixture.local, paused); assert.equal(fixture.tabs.size, 0);
});

async function dispatchFixture(f, operation, extra = {}) {
  await new Promise(resolve => setImmediate(resolve))
  const requestId = randomUUID()
  await f.nativeMessage.fire({type: 'request', protocolVersion: 1, requestId, operation, provider: 'feishu', args: {}, binding, ...extra})
  for (let i = 0; i < 2000; i++) {
    const response = f.responses.find(value => value.requestId === requestId)
    if (response) return response
    await new Promise(resolve => setTimeout(resolve, 1))
  }
  assert.fail('fixture did not respond')
}

function providerBinding(provider) {
  return provider === 'feishu' ? binding : {targetUrl: 'https://teams.microsoft.com/v2/', allowedOrigins: ['https://teams.microsoft.com']}
}

async function assertReadable(f, provider, target, tabId) {
  const original = chrome.scripting.executeScript
  const {sha256} = await import('../../src/browser/extension/protocol.js')
  const origin = new URL(target.targetUrl).origin
  const bound = {...target, accountHash: await sha256(provider + '\0' + origin + '\0user'), tenantHash: await sha256(provider + '\0' + origin + '\0tenant')}
  chrome.scripting.executeScript = async value => {
    assert.equal(value.target.tabId, tabId)
    if (value.func.name !== 'inspectPage') return original(value)
    assert.equal(value.args[0], provider)
    return [{result: {origin, identity: {accountId: 'user', tenantId: 'tenant'}, conversationId: 'c1', conversationName: 'Fixture', pageUrl: target.targetUrl, messages: [{id: 'm1', text: 'fixture'}]}}]
  }
  const response = await dispatchFixture(f, 'read', {provider, binding: bound, args: {conversationId: 'c1'}})
  assert.equal(response.ok, true, JSON.stringify(response))
  assert.deepEqual(response.data.messages, [{id: 'm1', text: 'fixture'}])
  const before = structuredClone(f.local)
  const doctor = await dispatchFixture(f, 'diagnose', {provider, binding: bound})
  assert.deepEqual(doctor.data, {sitePermission: true, paused: false, identityVerified: true, identityCode: 'identity-verified', diagnostics: {}})
  assert.deepEqual(f.local, before)
  assert.equal(f.tabs.size, 1)
  assert.equal(f.local.ownedTabs[provider].tabId, tabId)
}

test('page reads move an owned tab into the Opsail window and activate it there, never in the user window', async () => {
  const f = chromeFixture(); const {ownedTab} = await worker()
  const teams = {targetUrl: 'https://teams.microsoft.com/v2/', allowedOrigins: ['https://teams.microsoft.com']}
  f.session.opsailWindowId = 1
  const legacy = await ownedTab('teams', teams, true) // an earlier tab left in the user's window
  delete f.session.opsailWindowId
  assert.deepEqual([legacy.windowId, legacy.active], [1, false])
  const read = async (provider, target) => {
    const {sha256} = await import('../../src/browser/extension/protocol.js')
    const origin = new URL(target.targetUrl).origin
    const bound = {...target, accountHash: await sha256(provider + '\0' + origin + '\0user'), tenantHash: await sha256(provider + '\0' + origin + '\0tenant')}
    const original = chrome.scripting.executeScript
    chrome.scripting.executeScript = async value => value.func.name !== 'inspectPage' ? original(value)
      : [{result: {origin, identity: {accountId: 'user', tenantId: 'tenant'}, conversationId: 'c1', conversationName: 'Fixture', pageUrl: target.targetUrl, messages: []}}]
    try { return await dispatchFixture(f, 'read', {provider, binding: bound, args: {conversationId: 'c1'}}) } finally { chrome.scripting.executeScript = original }
  }
  assert.equal((await read('teams', teams)).ok, true)
  const moved = f.tabs.get(legacy.id)
  assert.notEqual(moved.windowId, 1); assert.equal(moved.active, true); assert.equal(f.session.opsailWindowId, moved.windowId)
  const feishu = await ownedTab('feishu', binding, true)
  assert.equal(feishu.windowId, moved.windowId)
  f.minimized.add(moved.windowId) // the user minimized the Opsail window: restore it unfocused

  assert.equal((await read('feishu', binding)).ok, true)
  assert.equal(f.tabs.get(feishu.id).active, true); assert.equal(f.tabs.get(feishu.id).windowId, moved.windowId)
  assert.equal(f.minimized.has(moved.windowId), false)
})

test('non-owned origin tab conflicts before create with zero page reads, navigation or deletion', async () => {
  const f = chromeFixture(); f.tabs.set(4, {id: 4, url: binding.targetUrl, active: true})
  chrome.tabs.create = chrome.tabs.remove = chrome.scripting.executeScript = async () => assert.fail('must not touch a user tab')
  const {ownedTab} = await worker()
  await assert.rejects(ownedTab('feishu', binding, true), error => error.code === 'target-tab-conflict' && error.diagnostics.conflictingTabCount === 1)
  const doctor = await dispatchFixture(f, 'diagnose')
  assert.equal(doctor.data.identityCode, 'target-tab-conflict'); assert.equal(f.tabs.size, 1)
})

test('created degraded tab is closed and repeated attempts leave no stray tabs', async () => {
  const f = chromeFixture(); const original = chrome.tabs.create
  f.session.opsailWindowId = 1 // an existing Opsail window: creation goes through tabs.create
  chrome.tabs.create = async args => { const tab = await original(args); f.tabs.get(tab.id).url += 'degraded'; return f.tabs.get(tab.id) }
  const {ownedTab} = await worker()
  for (let i = 0; i < 2; i++) await assert.rejects(ownedTab('feishu', binding, true), error => {
    assert.equal(error.code, 'provider-degraded'); assert.equal(error.diagnostics.ownedTabClosed, true); return true
  })
  assert.equal(f.tabs.size, 0); assert.equal(f.local.ownedTabs.feishu, undefined)
})

test('owned degraded document is reported by read-only doctor then closed by prepare; reused IDs are preserved', async () => {
  const f = chromeFixture(); const {ownedTab} = await worker(); const tab = await ownedTab('feishu', binding, true)
  f.tabs.get(tab.id).url += 'degraded'
  const before = structuredClone(f.local)
  const doctor = await dispatchFixture(f, 'diagnose')
  assert.equal(doctor.data.identityCode, 'provider-degraded'); assert.equal(f.tabs.size, 1); assert.deepEqual(f.local, before)
  await assert.rejects(ownedTab('feishu', binding, true), {code: 'provider-degraded'})
  assert.equal(f.tabs.size, 0)
  const next = await ownedTab('feishu', binding, true)
  f.tabs.get(next.id).url += 'degraded'; f.documents.set(next.id, 'reused-document')
  delete f.session[`createdOwnedTab:${next.id}`] // new browser session, no creation proof
  await assert.rejects(ownedTab('feishu', binding, true), error => error.code === 'provider-degraded' && error.diagnostics.ownedTabClosed === false)
  assert.equal(f.tabs.has(next.id), true)
})

test('bind reports build diagnostics before Native Messaging and closes only its unusable owned tab', async () => {
  const f = chromeFixture(); const original = chrome.scripting.executeScript
  chrome.scripting.executeScript = async value => {
    if (value.func.name === 'inspectPage') return [{result: {error: {code: 'unqualified-build'}, diagnostics: {bundleNames: ['index.abcdef01.js', 'https://PRIVATE/'], buildQualified: false, accountId: 'PRIVATE'}}}]
    if (value.func.name === 'inspectBootstrap') return [{result: {globals: [{name: 'PRIVATE', fields: [{key: 'PRIVATE', type: 'string'}]}], scriptResources: ['https://PRIVATE/']}}]
    return original(value)
  }
  await worker()
  const result = await dispatchFixture(f, 'verifyBinding')
  assert.equal(result.error.code, 'unqualified-build'); assert.equal(result.error.diagnostics.ownedTabClosed, true)
  assert.deepEqual(result.error.diagnostics.bundleNames, ['index.abcdef01.js'])
  assert.doesNotMatch(JSON.stringify(result), /PRIVATE/); assert.equal(f.tabs.size, 0)
})

test('doctor preserves an unqualified owned tab and exposes only bounded diagnostics', async () => {
  const f = chromeFixture(); const {ownedTab} = await worker(); await ownedTab('feishu', binding, true)
  const original = chrome.scripting.executeScript
  chrome.scripting.executeScript = async value => value.func.name === 'inspectPage' ? [{result: {error: {code: 'unqualified-build'}, diagnostics: {bundleNames: ['index.abcdef01.js'], token: 'PRIVATE'}}}] : original(value)
  const result = await dispatchFixture(f, 'diagnose')
  assert.equal(result.data.identityCode, 'unqualified-build'); assert.equal(f.tabs.size, 1)
  assert.deepEqual(result.data.diagnostics.bundleNames, ['index.abcdef01.js']); assert.doesNotMatch(JSON.stringify(result), /PRIVATE/)
})

test('build verification probes only the explicit candidate, with identity hashes and no message bodies', async () => {
  const f = chromeFixture(); const original = chrome.scripting.executeScript
  const {sha256} = await import('../../src/browser/extension/protocol.js')
  const accountHash = await sha256('feishu\0https://tenant.feishu.cn\0private-user')
  const tenantHash = await sha256('feishu\0https://tenant.feishu.cn\0private-tenant')
  chrome.scripting.executeScript = async value => {
    if (value.func.name !== 'inspectPage') return original(value)
    assert.equal(value.args[1], 'status'); assert.deepEqual(value.args[5], ['index.abcdef01.js'])
    assert.deepEqual(value.args[4], {accountHash, tenantHash})
    return [{result: {origin: 'https://tenant.feishu.cn', identity: {accountId: 'private-user', tenantId: 'private-tenant'}, diagnostics: {bundleNames: ['index.abcdef01.js'], behavioralContractPassed: true}}}]
  }
  await worker()
  const result = await dispatchFixture(f, 'verifyBuild', {args: {build: 'index.abcdef01.js'}, binding: {...binding, accountHash, tenantHash, identityBuilds: ['index.745e4057.js']}})
  assert.equal(result.data.ready, true); assert.equal(result.data.accountHash, accountHash)
  assert.doesNotMatch(JSON.stringify(result), /private-user|private-tenant/)
})

test('full degraded navigation closes a failed session-created tab even while active', async () => {
  const f = chromeFixture(); const {ownedTab} = await worker(); const tab = await ownedTab('feishu', binding, true)
  f.tabs.get(tab.id).url += 'degraded'; f.documents.set(tab.id, 'new-degraded-document')
  await assert.rejects(ownedTab('feishu', binding, true), error => error.code === 'provider-degraded' && error.diagnostics.ownedTabClosed)
  assert.equal(f.tabs.size, 0)
  const next = await ownedTab('feishu', binding, true)
  f.tabs.get(next.id).active = true
  await f.activated.fire({tabId: next.id}); f.tabs.get(next.id).url += 'degraded'
  f.documents.set(next.id, 'new-active-degraded-document')
  await assert.rejects(ownedTab('feishu', binding, true), error => error.code === 'provider-degraded' && error.diagnostics.ownedTabClosed)
  assert.equal(f.tabs.size, 0)
})

test('injection failure closes a session-created unusable tab without leaking exception text', async () => {
  const f = chromeFixture(); const {ownedTab} = await worker(); await ownedTab('feishu', binding, true)
  const original = chrome.scripting.executeScript
  chrome.scripting.executeScript = async value => { if (value.func.name === 'inspectPage') throw new Error('PRIVATE'); return original(value) }
  const response = await dispatchFixture(f, 'verifyBinding')
  assert.equal(response.error.code, 'injection-unavailable'); assert.equal(response.error.diagnostics.ownedTabClosed, true)
  assert.equal(f.tabs.size, 0); assert.doesNotMatch(JSON.stringify(response), /PRIVATE/)
})

test('conversation selection timeout preserves a healthy owned tab for a subsequent select', async () => {
  const f = chromeFixture(); const {ownedTab} = await worker(); const tab = await ownedTab('feishu', binding, true)
  const original = chrome.scripting.executeScript
  const realNow = Date.now; let now = realNow(); Date.now = () => now
  chrome.scripting.executeScript = async value => {
    if (value.func.name !== 'inspectPage') return original(value)
    now += 55_000
    return [{result: {error: {code: 'conversation-not-selected'}}}]
  }
  let response
  try { response = await dispatchFixture(f, 'read', {args: {conversationId: 'c1'}}) }
  finally { Date.now = realNow }
  assert.equal(response.error.code, 'conversation-not-selected'); assert.equal(f.tabs.has(tab.id), true)
  assert.equal(f.local.ownedTabs.feishu.tabId, tab.id)
})

test('select waits for the exact conversation pane, then the immediate first read succeeds without another click', async () => {
  const f = chromeFixture(); const {ownedTab} = await worker(); await ownedTab('feishu', binding, true)
  const original = chrome.scripting.executeScript
  let samples = 0, clicks = 0, reads = 0
  chrome.scripting.executeScript = async value => {
    if (value.func.name !== 'inspectPage') return original(value)
    const [, operation, args] = value.args
    const common = {origin: new URL(binding.targetUrl).origin, identity: {accountId: 'user', tenantId: 'tenant'}, conversationId: 'c1', conversationName: 'Fixture', pageUrl: binding.targetUrl}
    if (operation === 'select') { clicks++; return [{result: {...common, action: 'selection-requested'}}] }
    assert.equal(args.conversationId, 'c1')
    if (operation === 'selectionReady') {
      samples++
      if (samples < 3) return [{result: {error: {code: 'message-scope-unverified'}, identity: common.identity}}]
      return [{result: {...common, messageIds: ['m1']}}]
    }
    assert.equal(operation, 'read'); reads++
    return [{result: {...common, messages: [{id: 'm1', text: 'fixture'}]}}]
  }
  const selected = await dispatchFixture(f, 'select', {args: {conversationName: 'Fixture'}})
  assert.equal(selected.ok, true, JSON.stringify(selected)); assert.equal(selected.data.contentValidated, true)
  assert.equal(samples, 4); assert.equal(clicks, 1); assert.equal(reads, 0)
  const read = await dispatchFixture(f, 'read', {args: {conversationId: 'c1', limit: 1}})
  assert.equal(read.ok, true, JSON.stringify(read)); assert.equal(read.data.messages[0].id, 'm1'); assert.equal(clicks, 1)
})

test('read retries only pane transitions and preserves ownership failures as terminal', async () => {
  const f = chromeFixture(); const {ownedTab} = await worker(); const tab = await ownedTab('feishu', binding, true)
  const original = chrome.scripting.executeScript
  let calls = 0
  chrome.scripting.executeScript = async value => {
    if (value.func.name !== 'inspectPage') return original(value)
    calls++
    if (calls === 1) return [{result: {error: {code: 'message-scope-unverified'}, identity: {accountId: 'user'}}}]
    f.documents.set(tab.id, 'changed-document')
    return [{result: {origin: new URL(binding.targetUrl).origin, identity: {accountId: 'user', tenantId: 'tenant'}, conversationId: 'c1', messages: []}}]
  }
  const result = await dispatchFixture(f, 'read', {args: {conversationId: 'c1'}})
  assert.equal(result.ok, false); assert.equal(result.error.code, 'tab-not-prepared'); assert.equal(calls, 2)
})
