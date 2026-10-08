import {BUILD} from './build.js'
import {DEFAULT_FEISHU_BUILDS} from './identity-builds.js'
import {boundedDiagnostics} from './diagnostics.js'
import {PROTOCOL_VERSION, MAX_FRAME_BYTES, BridgeError, assert, validateRequest, validateBinding, sha256, safeError} from './protocol.js'
import {inspectPage} from './providers/dom.js'
import {settledPane} from './providers/settled-pane.js'
import {inspectBootstrap} from './providers/probe.js'
import {inspectFeishuFeed} from './providers/feishu-api.js'
import {inspectFeishuSelectedMessage} from './providers/feishu-message-probe.js'
import {continuation, resumeContinuation} from './providers/selected-chat-cursor.js'
import {ADAPTER_VERSION, qualification, apiPage} from './providers/index.js'

const NATIVE_HOST = 'com.opsail.chrome'
let port = null
let connecting = false
let build
let profileId
const busy = new Set()
let reconnectDelay = 1
const OWNED_TABS_KEY = 'ownedTabs'
const createdKey = tabId => `createdOwnedTab:${tabId}`
let ownedMutation = Promise.resolve()
const PREPARE_BUDGET_MS = 55_000
async function initialize() {
  if (!build) {
    // Only the installed build.js identifies the running code; build.json is disk state.
    assert(BUILD && BUILD.protocolVersion === PROTOCOL_VERSION && /^[a-f0-9]{64}$/.test(BUILD.buildId), 'invalid-build')
    build = BUILD
  }
  const stored = await chrome.storage.local.get(['profileId'])
  profileId = stored.profileId || [...crypto.getRandomValues(new Uint8Array(16))].map(x => x.toString(16).padStart(2, '0')).join('')
  assert(/^[a-f0-9]{32}$/.test(profileId), 'invalid-profile')
  if (!stored.profileId) await chrome.storage.local.set({profileId})
}
async function reloadChangedBuild(message) {
  if (!/^[a-f0-9]{64}$/.test(message.buildId) || message.buildId === build?.buildId) return
  const installed = await (await fetch(chrome.runtime.getURL('build.json'), {cache: 'no-store'})).json()
  const {reloadBuildId} = await chrome.storage.local.get('reloadBuildId')
  if (installed.buildId !== message.buildId || installed.protocolVersion !== PROTOCOL_VERSION || reloadBuildId === message.buildId) {
    await chrome.storage.session.set({connectionState: 'unavailable', diagnostic: 'load-stable-extension'})
    return
  }
  await chrome.storage.local.set({reloadBuildId: message.buildId})
  chrome.runtime.reload()
}
async function connect() {
  if (port || connecting) return
  connecting = true
  try {
    await initialize()
    const connection = chrome.runtime.connectNative(NATIVE_HOST)
    port = connection
    connection.onMessage.addListener(async message => {
      if (message.type === 'build-changed') {
        try { await reloadChangedBuild(message) } catch { await chrome.storage.session.set({connectionState: 'unavailable', diagnostic: 'load-stable-extension'}) }
        return
      }
      if (message.type === 'hello-ok') { reconnectDelay = 1; await chrome.storage.local.set({reloadBuildId: null}); return }
      if (message.type !== 'request') return
      dispatch(message).then(data => respond(connection, message.requestId, {ok: true, data}), error => respond(connection, message.requestId, {ok: false, error: safeError(error)}))
    })
    connection.onDisconnect.addListener(() => {
      void chrome.runtime.lastError
      if (port === connection) port = null
      chrome.storage.session.set({connectionState: 'disconnected'})
      chrome.alarms.create('reconnect', {when: Date.now() + reconnectDelay * 1000})
      reconnectDelay = Math.min(reconnectDelay * 2, 60)
    })
    connection.postMessage({type: 'hello', protocolVersion: PROTOCOL_VERSION, extensionVersion: chrome.runtime.getManifest().version, buildId: build.buildId, profileId})
    await chrome.storage.session.set({connectionState: 'connected'})
  } catch (error) { await chrome.storage.session.set({connectionState: 'unavailable', diagnostic: safeError(error).code}) }
  finally { connecting = false }
}
function respond(connection, requestId, result) {
  let response = {type: 'response', requestId, ...result}
  if (new TextEncoder().encode(JSON.stringify(response)).byteLength > MAX_FRAME_BYTES) response = {type: 'response', requestId, ok: false, error: {code: 'output-too-large', message: 'Request a smaller bounded page.'}}
  try { connection.postMessage(response) } catch { /* native side owns request timeout */ }
}
function sameBinding(record, binding) {
  return record && record.targetUrl === binding.targetUrl &&
    Array.isArray(record.allowedOrigins) && record.allowedOrigins.length === binding.allowedOrigins.length &&
    record.allowedOrigins.every((origin, index) => origin === binding.allowedOrigins[index])
}
async function ownedRecords() { return (await chrome.storage.local.get(OWNED_TABS_KEY))[OWNED_TABS_KEY] || {} }
function mutateOwnedRecords(change) {
  const mutation = ownedMutation.then(async () => {
    const records = await ownedRecords()
    const result = await change(records)
    await chrome.storage.local.set({[OWNED_TABS_KEY]: records})
    return result
  })
  ownedMutation = mutation.catch(() => {})
  return mutation
}
async function storeOwnedRecord(provider, record) {
  await mutateOwnedRecords(records => { records[provider] = record })
}
async function forgetOwnedRecord(provider) {
  await mutateOwnedRecords(records => { delete records[provider] })
  const {tabs = {}} = await chrome.storage.session.get('tabs'); delete tabs[provider]
  await chrome.storage.session.set({tabs})
}
export async function documentIdFor(tabId, binding) {
  try {
    const result = await chrome.scripting.executeScript({target: {tabId}, world: 'MAIN', func: () => ({origin: location.origin, readyState: document.readyState})})
    const entry = result?.[0]
    if (typeof entry?.documentId !== 'string') return {documentId: null, reason: 'injection-unavailable'}
    if (entry.result?.origin !== new URL(binding.targetUrl).origin || !binding.allowedOrigins.includes(entry.result.origin)) return {documentId: null, reason: 'origin-mismatch'}
    return {documentId: entry.documentId, reason: null}
  } catch { return {documentId: null, reason: 'injection-unavailable'} }
}
function degradedTab(provider, tab) {
  if (provider !== 'feishu') return false
  try { return /\/(?:next\/)?messenger\/degraded(?:\/|$)/i.test(new URL(tab.url).pathname) } catch { return false }
}
async function conflictingTabs(provider, binding) {
  if (provider !== 'feishu') return 0
  // Host permission scopes this metadata query. Never inject into, navigate,
  // close, or adopt any result; only a successfully verified owned tab is reused.
  const tabs = await chrome.tabs.query({url: new URL(binding.targetUrl).origin + '/*'})
  return Math.min(tabs.length, 1_000_000)
}
async function closeCreatedTab(provider, tabId) {
  const current = await chrome.tabs.get(tabId).catch(() => null)
  try {
    if (current) await chrome.tabs.remove(tabId)
    await forgetOwnedRecord(provider)
    await chrome.storage.session.set({[`pendingTab:${provider}`]: null})
    return {ownedTabClosed: !!current}
  } catch { return {ownedTabClosed: false, ownedTabCloseFailed: true} }
}
async function closeSessionCreatedTab(provider, binding, tabId) {
  if (!Number.isInteger(tabId)) return {ownedTabClosed: false}
  const proof = (await chrome.storage.session.get(createdKey(tabId)))[createdKey(tabId)]
  // Chrome tab IDs are unique within one browser session. Session storage
  // clears on browser/extension restart; it never grants read/adoption authority.
  if (proof?.provider !== provider || !sameBinding(proof, binding)) return {ownedTabClosed: false}
  return closeCreatedTab(provider, tabId)
}
async function closeVerifiedTab(provider, binding) {
  const verified = await verifyOwnedTab(provider, binding)
  // A stored tab ID alone is never deletion authority after restart/navigation.
  return verified.tab ? closeCreatedTab(provider, verified.tab.id) : closeSessionCreatedTab(provider, binding, verified.record?.tabId)
}
async function verifyOwnedTab(provider, binding) {
  const record = (await ownedRecords())[provider]
  if (!sameBinding(record, binding)) return {record, tab: null, reason: 'binding-changed'}
  // Legacy takenOver fields are ignored; only binding and document identity prove ownership.
  try {
    const tab = await chrome.tabs.get(record.tabId)
    if (!tab.url || tab.discarded || !binding.allowedOrigins.includes(new URL(tab.url).origin)) return {record, tab: null, reason: 'page-changed'}
    const observed = await documentIdFor(tab.id, binding)
    if (!observed.documentId || observed.documentId !== record.documentId) return {record, tab: null, reason: degradedTab(provider, tab) ? 'provider-degraded' : 'document-changed'}
    return {record, tab, reason: degradedTab(provider, tab) ? 'provider-degraded' : null}
  } catch { return {record, tab: null, reason: 'tab-missing'} }
}
async function waitForDocumentId(provider, tabId, binding) {
  const deadline = Date.now() + PREPARE_BUDGET_MS
  let lastReason = 'load-timeout'
  while (Date.now() < deadline) {
    try {
      const tab = await chrome.tabs.get(tabId)
      if (degradedTab(provider, tab)) return {documentId: null, reason: 'provider-degraded'}
      const expectedOrigin = new URL(binding.targetUrl).origin
      if (tab.url && new URL(tab.url).origin === expectedOrigin && binding.allowedOrigins.includes(expectedOrigin)) {
        const observed = await documentIdFor(tabId, binding)
        if (observed.documentId) return observed
        lastReason = observed.reason
      } else {
        lastReason = 'origin-mismatch'
      }
    } catch { return {documentId: null, reason: 'tab-missing'} }
    await new Promise(resolve => setTimeout(resolve, 150))
  }
  return {documentId: null, reason: lastReason === 'injection-unavailable' ? 'injection-unavailable' : 'load-timeout'}
}
async function ownCreatedTab(provider, binding, tab) {
  const observed = await waitForDocumentId(provider, tab.id, binding)
  if (!observed.documentId) {
    if (provider === 'feishu') {
      const diagnostics = await closeCreatedTab(provider, tab.id)
      throw new BridgeError(observed.reason, observed.reason, {...diagnostics, providerDegraded: observed.reason === 'provider-degraded'})
    }
    await chrome.storage.session.set({[`pendingTab:${provider}`]: {tabId: tab.id, targetUrl: binding.targetUrl, allowedOrigins: [...binding.allowedOrigins], reason: observed.reason}})
    throw new BridgeError(observed.reason)
  }
  await storeOwnedRecord(provider, {tabId: tab.id, documentId: observed.documentId, targetUrl: binding.targetUrl, allowedOrigins: [...binding.allowedOrigins]})
  await chrome.storage.session.set({[`pendingTab:${provider}`]: null})
  await chrome.storage.session.set({tabs: {...(await chrome.storage.session.get('tabs')).tabs, [provider]: tab.id}})
  return tab
}
// Owned tabs live in one unfocused Opsail window, so activating one never
// changes the tab the user is looking at in their own windows.
const OPSAIL_WINDOW_KEY = 'opsailWindowId'
async function opsailWindowId() {
  const {[OPSAIL_WINDOW_KEY]: id} = await chrome.storage.session.get(OPSAIL_WINDOW_KEY)
  if (!Number.isInteger(id)) return null
  try { return (await chrome.windows.get(id)).type === 'normal' ? id : null } catch { return null }
}
async function rememberWindow(window) {
  await chrome.storage.session.set({[OPSAIL_WINDOW_KEY]: window.id})
  return window
}
// A freshly loaded Feishu or Teams page paints its lists only in a window's active
// tab, so each page operation first activates its owned tab inside the Opsail window.
async function renderable(provider, tab) {
  let windowId = await opsailWindowId()
  if (windowId === null) windowId = (await rememberWindow(await chrome.windows.create({focused: false, tabId: tab.id}))).id
  else if (tab.windowId !== windowId) await chrome.tabs.move(tab.id, {windowId, index: -1})
  // A minimized window paints nothing either; restore it without focusing.
  if ((await chrome.windows.get(windowId)).state === 'minimized') await chrome.windows.update(windowId, {state: 'normal', focused: false})
  const current = await chrome.tabs.get(tab.id)
  if (current.active) return current
  await chrome.tabs.update(tab.id, {active: true})
  await new Promise(resolve => setTimeout(resolve, 1000))
  return chrome.tabs.get(tab.id)
}
export async function ownedTab(provider, binding, create = false) {
  const verified = await verifyOwnedTab(provider, binding)
  if (verified.reason === 'provider-degraded') {
    const diagnostics = verified.tab ? await closeCreatedTab(provider, verified.tab.id) : await closeSessionCreatedTab(provider, binding, verified.record?.tabId)
    throw new BridgeError('provider-degraded', 'provider-degraded', {...diagnostics, providerDegraded: true})
  }
  if (verified.tab) return verified.tab
  if (verified.record) {
    if (provider === 'feishu') {
      const closed = await closeSessionCreatedTab(provider, binding, verified.record.tabId)
      if (closed.ownedTabCloseFailed) throw new BridgeError('page-changed', 'page-changed', closed)
    }
    await forgetOwnedRecord(provider)
  }
  assert(create, 'tab-not-prepared')
  const { [`pendingTab:${provider}`]: pending } = await chrome.storage.session.get(`pendingTab:${provider}`)
  if (pending?.targetUrl === binding.targetUrl && Array.isArray(pending.allowedOrigins) && pending.allowedOrigins.every((origin, index) => origin === binding.allowedOrigins[index])) {
    let pendingTab
    try { pendingTab = await chrome.tabs.get(pending.tabId) } catch { await chrome.storage.session.set({[`pendingTab:${provider}`]: null}); throw new BridgeError('tab-missing') }
    return ownCreatedTab(provider, binding, pendingTab)
  }
  const conflictingTabCount = await conflictingTabs(provider, binding)
  if (conflictingTabCount) throw new BridgeError('target-tab-conflict', 'target-tab-conflict', {conflictingTabCount})
  const windowId = await opsailWindowId()
  const tab = windowId === null
    ? (await rememberWindow(await chrome.windows.create({focused: false, url: binding.targetUrl}))).tabs[0]
    : await chrome.tabs.create({windowId, url: binding.targetUrl, active: false})
  await chrome.storage.session.set({[createdKey(tab.id)]: {provider, targetUrl: binding.targetUrl, allowedOrigins: [...binding.allowedOrigins]}})
  return ownCreatedTab(provider, binding, tab)
}
async function page(provider, binding, operation, args, identityBuilds = binding.identityBuilds ?? DEFAULT_FEISHU_BUILDS) {
  const tab = await renderable(provider, await ownedTab(provider, binding))
  let result
  try {
    const [entry] = await chrome.scripting.executeScript({target: {tabId: tab.id}, world: 'MAIN', func: inspectPage, args: [provider, operation, args, new URL(binding.targetUrl).origin, {accountHash: binding.accountHash, tenantHash: binding.tenantHash}, identityBuilds]})
    result = entry?.result
  } catch (error) {
    if (provider !== 'feishu') throw error
    throw new BridgeError('injection-unavailable', 'injection-unavailable', await closeVerifiedTab(provider, binding))
  }
  assert((await ownedTab(provider, binding)).id === tab.id, 'page-changed')
  assert(result && typeof result === 'object', 'invalid-page-output')
  if (result.error) {
    const diagnostics = await pageDiagnostics(tab, provider, binding, result)
    if (provider === 'feishu' && ['unqualified-build', 'provider-degraded', 'identity-unverified', 'identity-read-failed', 'login-required', 'account-changed', 'tenant-changed', 'identity-changed', 'page-changed'].includes(result.error.code)) Object.assign(diagnostics, await closeVerifiedTab(provider, binding))
    throw new BridgeError(result.error.code, result.error.code, diagnostics)
  }
  const identity = {
    accountHash: result.identity?.accountId ? await sha256(provider + '\0' + result.origin + '\0' + result.identity.accountId) : null,
    tenantHash: result.identity?.tenantId ? await sha256(provider + '\0' + result.origin + '\0' + result.identity.tenantId) : null,
  }
  if (binding.accountHash) assert(identity.accountHash === binding.accountHash, 'account-changed')
  if (binding.tenantHash) assert(identity.tenantHash === binding.tenantHash, 'tenant-changed')
  delete result.identity
  return {...result, ...identity, diagnostics: boundedDiagnostics(result.diagnostics)}
}
async function pageDiagnostics(tab, provider, binding, observed) {
  const diagnostics = boundedDiagnostics(observed?.diagnostics)
  if (['identity-unverified', 'identity-read-failed'].includes(observed?.error?.code) || !observed?.identity?.accountId) {
    try {
      const [{result} = {}] = await chrome.scripting.executeScript({target: {tabId: tab.id}, world: 'MAIN', func: inspectBootstrap, args: [new URL(binding.targetUrl).origin]})
      const after = await verifyOwnedTab(provider, binding)
      if (after.tab?.id === tab.id && !after.reason) Object.assign(diagnostics, boundedDiagnostics({bootstrap: result}))
    } catch { /* Keep the original failure; diagnostic failure never enables a read. */ }
  }
  return diagnostics
}
// Doctor never prepares, adopts or mutates a tab/binding.
async function diagnose(provider, binding) {
  const sitePermission = await chrome.permissions.contains({origins: binding.allowedOrigins.map(origin => origin + '/*')})
  const paused = (await chrome.storage.local.get('paused')).paused === true
  const result = {sitePermission, paused, identityVerified: false, identityCode: 'not-probed'}
  if (!sitePermission) return result
  if (paused) return result
  const before = await verifyOwnedTab(provider, binding)
  if (before.reason === 'provider-degraded') return {...result, identityCode: 'provider-degraded', diagnostics: {providerDegraded: true}}
  if (!before.tab) {
    const conflictingTabCount = await conflictingTabs(provider, binding)
    if (conflictingTabCount) return {...result, identityCode: 'target-tab-conflict', diagnostics: {conflictingTabCount}}
    return {...result, identityCode: ['binding-changed', 'tab-missing', 'document-changed', 'page-changed'].includes(before.reason) ? before.reason : 'tab-not-prepared'}
  }
  try {
    const [{result: observed} = {}] = await chrome.scripting.executeScript({target: {tabId: before.tab.id}, world: 'MAIN', func: inspectPage, args: [provider, 'status', {}, new URL(binding.targetUrl).origin, {accountHash: binding.accountHash, tenantHash: binding.tenantHash}, binding.identityBuilds ?? DEFAULT_FEISHU_BUILDS]})
    const after = await verifyOwnedTab(provider, binding)
    if (after.reason === 'provider-degraded') return {...result, identityCode: 'provider-degraded', diagnostics: {providerDegraded: true}}
    if (!after.tab || after.tab.id !== before.tab.id) return {...result, identityCode: 'page-changed'}
    result.diagnostics = await pageDiagnostics(before.tab, provider, binding, observed)
    if (observed?.error) return {...result, identityCode: observed.error.code}
    if (!observed?.identity?.accountId || !observed?.identity?.tenantId) return {...result, identityCode: 'identity-unverified'}
    const accountHash = await sha256(provider + '\0' + observed.origin + '\0' + observed.identity.accountId)
    const tenantHash = await sha256(provider + '\0' + observed.origin + '\0' + observed.identity.tenantId)
    if (!binding.accountHash || !binding.tenantHash) return {...result, identityCode: 'not-bound'}
    if (accountHash !== binding.accountHash) return {...result, identityCode: 'account-changed'}
    if (tenantHash !== binding.tenantHash) return {...result, identityCode: 'tenant-changed'}
    return {...result, identityVerified: true, identityCode: 'identity-verified'}
  } catch { return {...result, identityCode: 'identity-unverified'} }
}
async function dispatch(raw) {
  const deadline = Date.now() + PREPARE_BUDGET_MS
  const request = validateRequest(raw)
  const {operation, provider, args} = request
  if (operation === 'ping') return {profileId, buildId: build.buildId, protocolVersion: PROTOCOL_VERSION, extensionVersion: chrome.runtime.getManifest().version}
  if (operation === 'pause') { await chrome.storage.local.set({paused: args.paused}); return {paused: args.paused} }
  const binding = validateBinding(provider, request.binding)
  if (operation === 'diagnose') return diagnose(provider, binding)
  const origins = binding.allowedOrigins.map(origin => origin + '/*')
  await chrome.storage.local.set({['binding:' + provider]: {targetUrl: binding.targetUrl, allowedOrigins: binding.allowedOrigins}})
  const granted = await chrome.permissions.contains({origins})
  if (!granted) throw new BridgeError('site-permission-required')
  if (operation === 'configure') return {configured: true, profileId}
  const paused = (await chrome.storage.local.get('paused')).paused
  assert(!paused || operation === 'status', 'paused')
  assert(!busy.has(provider), 'source-busy')
  busy.add(provider)
  try {
    if (operation === 'verifyBinding' || operation === 'verifyBuild') {
      await ownedTab(provider, binding, true)
      if (operation === 'verifyBuild') assert(binding.accountHash && binding.tenantHash, 'not-bound')
      const observed = await page(provider, binding, 'status', {}, operation === 'verifyBuild' ? [args.build] : binding.identityBuilds)
      if (operation === 'verifyBuild') assert(observed.diagnostics.behavioralContractPassed && observed.diagnostics.bundleNames?.includes(args.build), 'build-not-observed')
      if (!observed.accountHash || !observed.tenantHash) {
        const tab = await ownedTab(provider, binding)
        throw new BridgeError('identity-unverified', 'identity-unverified', await pageDiagnostics(tab, provider, binding, observed))
      }
      return {ready: true, profileId, accountHash: observed.accountHash, tenantHash: observed.tenantHash, diagnostics: observed.diagnostics}
    }
    if (operation === 'prepare') {
      const tab = await ownedTab(provider, binding, true)
      return {action: 'prepared', contentValidated: false, tabId: tab.id}
    }
    if (operation === 'status' || operation === 'qualify') {
      let observed
      try { observed = await page(provider, binding, 'status', {}) }
      catch (error) {
        if (operation === 'qualify') throw error
        const diagnostics = boundedDiagnostics(error.diagnostics)
        return {ready: false, reason: safeError(error).code, profileId, accountHash: null, tenantHash: null, adapterVersion: ADAPTER_VERSION, ...(diagnostics ? {diagnostics} : {})}
      }
      const q = qualification(provider, observed)
      if (operation === 'qualify') {
        if (provider === 'feishu' && observed.accountHash && observed.tenantHash) {
          const tab = await ownedTab(provider, binding)
          const [{result} = {}] = await chrome.scripting.executeScript({target: {tabId: tab.id}, world: 'MAIN', func: inspectFeishuFeed, args: [new URL(binding.targetUrl).origin, {accountHash: observed.accountHash, tenantHash: observed.tenantHash}]})
          assert((await ownedTab(provider, binding)).id === tab.id, 'page-changed')
          const after = await page(provider, binding, 'status', {})
          assert(after.accountHash === observed.accountHash && after.tenantHash === observed.tenantHash, 'identity-changed')
          const [{result: messageResult} = {}] = await chrome.scripting.executeScript({target: {tabId: tab.id}, world: 'MAIN', func: inspectFeishuSelectedMessage, args: [new URL(binding.targetUrl).origin, {accountHash: observed.accountHash, tenantHash: observed.tenantHash}]})
          assert((await ownedTab(provider, binding)).id === tab.id, 'page-changed')
          const final = await page(provider, binding, 'status', {})
          assert(final.accountHash === observed.accountHash && final.tenantHash === observed.tenantHash, 'identity-changed')
          // One successful fixed read is evidence only; full paging, hidden
          // branches and reply coverage remain separate qualification gates.
          const failure = (await chrome.storage.session.get('selectedChatPageFailure')).selectedChatPageFailure
          const selectedPageFailure = failure?.accountHash === observed.accountHash && failure?.tenantHash === observed.tenantHash ? failure : null
          return {...q, diagnostics: {fixedFeedProbe: result, fixedMessageProbe: messageResult, ...(selectedPageFailure ? {selectedPageFailure} : {})}}
        }
        return q
      }
      if (!q.qualified || !observed.accountHash || !observed.tenantHash || !observed.diagnostics.catalogIdentityVerified || !observed.diagnostics.messageScopeVerified) {
        const tab = await ownedTab(provider, binding)
        const [{result} = {}] = await chrome.scripting.executeScript({target: {tabId: tab.id}, world: 'MAIN', func: inspectBootstrap, args: [new URL(binding.targetUrl).origin]})
        observed.diagnostics = boundedDiagnostics({...observed.diagnostics, bootstrap: result})
      }
      return {ready: !!observed.accountHash && !!observed.tenantHash, reason: observed.accountHash ? 'identity-verified' : 'identity-unverified', profileId, accountHash: observed.accountHash, tenantHash: observed.tenantHash, adapterVersion: ADAPTER_VERSION, capabilities: {...q.capabilities, catalog: observed.diagnostics.catalogIdentityVerified, read: observed.diagnostics.messageScopeVerified}, diagnostics: observed.diagnostics}
    }
    if (provider === 'feishu' && operation === 'messagesPage') {
      assert(typeof args.conversationId === 'string' && /^\d{1,30}$/.test(args.conversationId) && !args.rangeStart && !args.rangeEnd, 'invalid-request')
      const observed = await page(provider, binding, 'status', {})
      assert(observed.accountHash && observed.tenantHash, 'identity-unverified')
      const tab = await ownedTab(provider, binding)
      const record = (await ownedRecords())[provider]
      const context = {conversationId: args.conversationId, accountHash: observed.accountHash, tenantHash: observed.tenantHash, documentId: record.documentId, buildId: build.buildId}
      const options = {mode: args.cursor ? 'previous' : 'initial', expectedChatId: args.conversationId, limit: args.limit ?? 100}
      if (args.cursor) {
        assert(/^selected:[a-f0-9-]{36}$/.test(args.cursor), 'invalid-cursor')
        const stored = (await chrome.storage.session.get('selectedChatCursors')).selectedChatCursors || {}
        options.priorPosition = resumeContinuation(stored[args.cursor], context)
      }
      const [{result} = {}] = await chrome.scripting.executeScript({target: {tabId: tab.id}, world: 'MAIN', func: inspectFeishuSelectedMessage, args: [new URL(binding.targetUrl).origin, {accountHash: observed.accountHash, tenantHash: observed.tenantHash}, options]})
      assert((await ownedTab(provider, binding)).id === tab.id, 'page-changed')
      if (result?.reason !== 'selected-chat-page-observed') {
        // The probe's diagnostics contain bounded field types/booleans only.
        // Persist no message array or page-owned entity on a rejected read.
        await chrome.storage.session.set({selectedChatPageFailure: {...context, reason: result?.reason || 'invalid-page-output', diagnostics: result?.diagnostics || null, observedAt: new Date().toISOString()}})
        throw new BridgeError(result?.reason || 'invalid-page-output')
      }
      await chrome.storage.session.set({selectedChatPageFailure: null})
      const after = await page(provider, binding, 'status', {})
      assert(after.accountHash === observed.accountHash && after.tenantHash === observed.tenantHash, 'identity-changed')
      let nextCursor = null
      if (result.messages?.length) {
        const stored = (await chrome.storage.session.get('selectedChatCursors')).selectedChatCursors || {}
        const recent = Object.fromEntries(Object.entries(stored).filter(([, value]) => Date.now() - value.issuedAt < 30 * 60_000).slice(-99))
        nextCursor = 'selected:' + crypto.randomUUID()
        recent[nextCursor] = continuation(result, context)
        await chrome.storage.session.set({selectedChatCursors: recent})
      }
      return {...result, nextCursor, scope: 'selected-conversation', adapterVersion: ADAPTER_VERSION, providerCatalogComplete: false}
    }
    if (operation === 'catalogPage' || operation === 'messagesPage') {
      const observed = await page(provider, binding, 'status', {})
      assert(qualification(provider, observed).qualified, 'browser-api-unqualified')
      return apiPage(provider, operation, args, {observed, tab: await ownedTab(provider, binding)})
    }
    if (operation === 'scrollBack') {
      // One bounded step inside the verified pane; the caller reads the settled window next.
      const moved = await page(provider, binding, 'scroll', args)
      return {conversationId: moved.conversationId, direction: moved.direction, moved: moved.moved, atTop: moved.atTop}
    }
    if (operation === 'select') {
      const selected = await page(provider, binding, 'select', args)
      await settledPane(() => page(provider, binding, 'selectionReady', {conversationId: selected.conversationId}), {deadline})
      return {action: selected.action, contentValidated: true}
    }
    const observed = operation === 'read'
      ? await settledPane(() => page(provider, binding, 'read', args), {deadline})
      : await page(provider, binding, operation, args)
    if (operation === 'catalog') return {entries: observed.entries, nextCursor: null, complete: false}
    if (operation === 'read') return {conversationId: observed.conversationId, conversationName: observed.conversationName, pageUrl: observed.pageUrl, messages: observed.messages}
    throw new BridgeError('unsupported-operation')
  } finally { busy.delete(provider) }
}
chrome.tabs.onRemoved.addListener(async tabId => {
  await chrome.storage.session.remove(createdKey(tabId))
  const records = await ownedRecords()
  for (const [provider, record] of Object.entries(records)) if (record.tabId === tabId) await forgetOwnedRecord(provider)
})
chrome.runtime.onMessage.addListener((message, sender, respond) => {
  if (sender.id !== chrome.runtime.id || sender.url !== chrome.runtime.getURL('options.html')) return false
  if (message?.action === 'status') {
    Promise.all([chrome.storage.local.get(null), chrome.storage.session.get(['connectionState', 'diagnostic'])]).then(([local, session]) => respond({profileId, buildId: build?.buildId, paused: !!local.paused, bindings: Object.fromEntries(['feishu', 'teams'].map(p => [p, local['binding:' + p] || null])), ...session}))
  } else if (message?.action === 'pause' && typeof message.paused === 'boolean') {
    chrome.storage.local.set({paused: message.paused}).then(() => respond({ok: true}))
  } else if (message?.action === 'reconnect') { connect().then(() => respond({ok: true})) }
  else return false
  return true
})
chrome.runtime.onInstalled.addListener(connect)
chrome.runtime.onStartup.addListener(connect)
chrome.alarms.onAlarm.addListener(alarm => { if (alarm.name === 'reconnect') connect() })
connect()
