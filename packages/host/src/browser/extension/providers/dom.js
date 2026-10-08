// This fixed function is serialized by chrome.scripting. Never accept code from IPC.
export async function inspectPage(provider, operation, args, expectedOrigin, expectedIdentity = {}, qualifiedBuilds = ['index.745e4057.js']) {
  let buildDiagnostics = {}
  const fail = code => ({error: {code, message: code}, diagnostics: buildDiagnostics})
  if (location.origin !== expectedOrigin) return fail('page-changed')
  const clean = v => String(v || '').trim()
  const text = (node, selector) => clean(node?.querySelector(selector)?.textContent)
  const stableAttr = (node, names) => names.map(name => node?.getAttribute(name)).find(Boolean) || ''
  const feishu = provider === 'feishu'
  const ownValue = (object, key) => object && Object.getOwnPropertyDescriptor(object, key)?.value
  // BEGIN CURRENT FIBER (kept inside the serialized page function)
  // Attached DOM fibers can retain their mount branch after a React commit.
  // Membership in the committed child/sibling tree selects the current copy,
  // including shared bailout children whose return pointers can be stale.
  const currentTrees = new WeakMap()
  function findCurrentFiber(fiber, {maxSteps = 512, maxTreeNodes = 50000} = {}) {
    if (!fiber || typeof fiber !== 'object' || !Number.isInteger(maxSteps) || maxSteps < 1 || maxSteps > 4096 || !Number.isInteger(maxTreeNodes) || maxTreeNodes < 1 || maxTreeNodes > 50000) return null
    if (!fiber.alternate) return fiber
    let root = fiber
    const ancestors = new Set()
    while (root.return) {
      if (ancestors.has(root) || ancestors.size >= maxSteps) return null
      ancestors.add(root); root = root.return
    }
    const owner = root.stateNode, current = owner?.current
    if (!current || current.stateNode !== owner || ![root, root.alternate].includes(current)) return null
    let members = currentTrees.get(current)
    if (!members) {
      members = new Set()
      const pending = [current]
      while (pending.length) {
        const node = pending.pop()
        if (!node || typeof node !== 'object' || members.has(node) || members.size >= maxTreeNodes) return null
        members.add(node)
        if (node.sibling) pending.push(node.sibling)
        if (node.child) pending.push(node.child)
      }
      currentTrees.set(current, members)
    }
    return members.has(fiber) ? fiber : members.has(fiber.alternate) ? fiber.alternate : null
  }
  // END CURRENT FIBER
  const fiberProps = (node, limit = 7) => {
    if (!node) return []
    const key = Object.getOwnPropertyNames(node).find(key => /^__react(?:Fiber|InternalInstance)\$/.test(key))
    const result = []
    const visited = new Set()
    for (let fiber = ownValue(node, key), depth = 0; fiber && depth < limit; depth++, fiber = ownValue(fiber, 'return')) {
      fiber = findCurrentFiber(fiber)
      if (!fiber || visited.has(fiber)) return []
      visited.add(fiber)
      const props = ownValue(fiber, 'memoizedProps')
      if (props) result.push(props)
    }
    return result
  }
  // Read only fixed fields of the current message's Immutable Record (the same
  // MessageItem contract used by the selected-message probe). Never walk stores.
  const field = (record, key) => {
    const value = ownValue(record, key)
    if (value !== undefined) return value
    for (let object = record, depth = 0; object && depth < 8; object = Object.getPrototypeOf(object), depth++) {
      const get = ownValue(object, 'get')
      if (typeof get === 'function') { try { return get.call(record, key) } catch { return undefined } }
    }
  }
  // Only IDs explicitly exposed as identity metadata qualify. A display name is not an account ID.
  const identityNode = document.querySelector('[data-user-id][data-tenant-id], [data-account-id][data-tenant-id]')
  let accountId = stableAttr(identityNode, ['data-user-id', 'data-account-id'])
  let tenantId = stableAttr(identityNode, ['data-tenant-id'])
  let identityKind = 'dom-metadata'
  // The deployed Teams UI exposes current (not home) identity on attached
  // userContext.authenticationUser.profile. Read only these three own values.
  const teamsIdentity = () => {
    const roots = [...document.querySelectorAll('[data-tid="chat-pane-message"]')].slice(0, 2)
    const contexts = roots.flatMap(node => fiberProps(node, 100)).map(p => ownValue(p, 'userContext')).filter(Boolean)
    if (contexts.length < 2) return null
    const identities = contexts.map(context => {
      const auth = ownValue(context, 'authenticationUser'), profile = ownValue(auth, 'profile')
      return {authenticated: ownValue(auth, 'isAuthenticated'), accountId: ownValue(profile, 'objectId'), tenantId: ownValue(profile, 'tenantId')}
    })
    if (!identities.every(x => x.authenticated === true && typeof x.accountId === 'string' && x.accountId && typeof x.tenantId === 'string' && x.tenantId)) return null
    const first = identities[0]
    if (!identities.every(x => x.accountId === first.accountId && x.tenantId === first.tenantId)) return null
    return first
  }
  if (!feishu && (!accountId || !tenantId)) {
    const current = teamsIdentity()
    if (current) { accountId = current.accountId; tenantId = current.tenantId; identityKind = 'teams-authenticated-context' }
  }
  // Only this qualified session method may establish Feishu identity. DOM
  // attributes must not bypass either the build gate or its behavioral contract.
  if (feishu) {
    accountId = ''; tenantId = ''
    if (/\/(?:next\/)?messenger\/degraded(?:\/|$)/i.test(location.pathname)) {
      buildDiagnostics = {providerDegraded: true}
      return fail('provider-degraded')
    }
    const names = new Set()
    for (const resource of performance.getEntriesByType('resource')) {
      try {
        const url = new URL(resource.name)
        const match = url.pathname.match(/\/static\/js\/(index\.[a-f0-9]{6,64}\.js)$/)
        if (resource.initiatorType === 'script' && url.protocol === 'https:' && !url.username && !url.password && url.hostname === 'sf1-scmcdn-cn.feishucdn.com' && !url.port && match) names.add(match[1])
      } catch { /* resource URLs never leave this function */ }
    }
    const bundleNames = [...names].sort()
    const supported = bundleNames.length > 0 && bundleNames.length <= 16 && Array.isArray(qualifiedBuilds) && bundleNames.every(name => qualifiedBuilds.includes(name))
    buildDiagnostics = {bundleNames: bundleNames.slice(0, 16), bundleNamesTruncated: bundleNames.length > 16, buildQualified: supported, behavioralContractPassed: false}
    if (!supported) return fail('unqualified-build')
    const passport = window.configurationAdapter?.passport
    if (typeof passport?.getCurUserInfo === 'function') {
      let timer
      try {
        const info = await Promise.race([passport.getCurUserInfo(), new Promise((_, reject) => {timer = setTimeout(() => reject(new Error('identity-read-timeout')), 8000)})])
        const user = info?.user
        if (typeof user?.id === 'string' && typeof user?.tenant?.id === 'string' && user.id && user.tenant.id && user.id === passport.userId && user.id === window.userId) {
          accountId = user.id; tenantId = user.tenant.id; identityKind = 'feishu-session-user'
          buildDiagnostics.behavioralContractPassed = true
        }
      } catch { return fail('identity-read-failed') }
      finally { clearTimeout(timer) }
    }
    if (!buildDiagnostics.behavioralContractPassed) return fail('identity-unverified')
  }
  const identity = {accountId: clean(accountId), tenantId: clean(tenantId)}
  const hash = async value => [...new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(value)))].map(x => x.toString(16).padStart(2, '0')).join('')
  // Check the bound identity inside the same script, before any selection click.
  if ((expectedIdentity.accountHash || expectedIdentity.tenantHash) && (!accountId || !tenantId)) return fail(document.querySelector('input[type="password"]') ? 'login-required' : 'identity-unverified')
  if (expectedIdentity.accountHash && (!accountId || await hash(provider + '\0' + location.origin + '\0' + accountId) !== expectedIdentity.accountHash)) return fail('account-changed')
  if (expectedIdentity.tenantHash && (!tenantId || await hash(provider + '\0' + location.origin + '\0' + tenantId) !== expectedIdentity.tenantHash)) return fail('tenant-changed')
  if (identityKind === 'feishu-session-user' && (window.userId !== accountId || window.configurationAdapter?.passport?.userId !== accountId)) return fail('identity-changed')
  if (identityKind === 'teams-authenticated-context') {
    const current = teamsIdentity()
    if (!current || current.accountId !== accountId || current.tenantId !== tenantId) return fail('identity-changed')
  }
  if (identityKind === 'dom-metadata' && (stableAttr(identityNode, ['data-user-id', 'data-account-id']) !== accountId || stableAttr(identityNode, ['data-tenant-id']) !== tenantId || !identityNode?.isConnected)) {
    if (accountId || tenantId) return fail('identity-changed')
  }
  // Actual Feishu feed cards expose the same source ID in the feed context and
  // row props. Corroborate both; virtual row positions and labels are not IDs.
  let activeFeedId = null, feedPreviews = null
  if (feishu && typeof window.__feedStore?.getState === 'function') {
    try { const state = window.__feedStore.getState(); activeFeedId = state?.status?.activeFeedId; feedPreviews = state?.previews } catch { /* no state evidence */ }
  }
  // Read-only change marker: the feed preview's last message position for the same feed ID.
  const lastPosition = id => {
    const preview = ownValue(feedPreviews, id), position = ownValue(preview, 'lastMessagePosition')
    return ownValue(preview, 'feedId') === id && Number.isSafeInteger(position) && position > 0 ? position : undefined
  }
  const cards = [...document.querySelectorAll(feishu ? '.a11y_feed_card_item' : '[id^="title-chat-list-item_"]')]
  const entries = cards.map(node => {
    let conversationId = feishu ? stableAttr(node, ['data-chat-id', 'data-conversation-id', 'data-feed-id']) : node.id.slice('title-chat-list-item_'.length)
    let sourceSelected = false
    if (feishu && !conversationId) {
      const props = fiberProps(node)
      const directIds = props.map(p => ownValue(p, 'feedId')).filter(id => typeof id === 'string' && id)
      const contextIds = props.map(p => ownValue(ownValue(p, 'value'), 'feedId')).filter(id => typeof id === 'string' && id)
      const ids = [...directIds, ...contextIds]
      if (directIds.length && contextIds.length && new Set(ids).size === 1) {
        conversationId = ids[0]
        sourceSelected = activeFeedId === conversationId && props.some(p => ownValue(p, 'isActive') === true || ownValue(ownValue(p, 'value'), 'isActive') === true)
      }
    } else if (!feishu) {
      const conversations = fiberProps(node).map(p => ownValue(p, 'conversation')).filter(Boolean)
      if (conversations.length) {
        if (!conversations.every(c => ownValue(c, 'internalId') === conversationId)) conversationId = ''
        sourceSelected = conversations.some(c => ownValue(c, 'isSelected') === true)
      }
    }
    const conversationName = feishu ? text(node, '.a11y_feed_card_main [style*="min-width"]') : clean(node.textContent)
    const selected = sourceSelected || node.matches('[aria-selected="true"],.active') || !!node.closest('[aria-selected="true"],[data-is-selected="true"],[data-selected="true"]')
    return {node, conversationId, conversationName, selected}
  }).filter(item => item.conversationName)
  const selected = entries.filter(row => row.selected)
  const catalogIds = entries.map(x => x.conversationId)
  const catalogIdentityVerified = !!accountId && !!tenantId && entries.length > 0 && catalogIds.every(Boolean) && new Set(catalogIds).size === catalogIds.length
  const messageScope = document.querySelector('[data-message-conversation-id]')
  const messageScopeId = messageScope?.getAttribute('data-message-conversation-id')
  const messageSelector = feishu ? '.messageItem-wrapper[data-id]' : '[data-tid="chat-pane-message"]'
  const nodes = [...(messageScope || document).querySelectorAll(messageSelector)]
  const messageEvidence = nodes.map(node => {
    const props = fiberProps(node, feishu ? 45 : 100)
    const sourceMessages = feishu ? [] : props.slice(0, 15).map(p => ownValue(p, 'message')).filter(Boolean)
    const ids = feishu ? props.slice(0, 3).map(p => ownValue(p, 'messageId')).filter(v => typeof v === 'string') : sourceMessages.map(m => ownValue(m, 'id')).filter(v => typeof v === 'string')
    const rowScopeIds = props.map(p => ownValue(p, feishu ? 'chatId' : 'convId')).filter(v => typeof v === 'string' && v)
    const paneScopeIds = feishu ? props.filter(p => Array.isArray(ownValue(p, 'messages'))).map(p => ownValue(p, 'id')).filter(v => typeof v === 'string' && v) : props.map(p => ownValue(ownValue(p, 'chat'), 'id')).filter(v => typeof v === 'string' && v)
    const resolvedIds = feishu ? [] : props.map(p => ownValue(p, 'resolvedConvId')).filter(v => typeof v === 'string' && v)
    const domId = stableAttr(node, feishu ? ['data-id'] : ['data-mid']) || (!feishu ? stableAttr(node.closest('[data-mid]'), ['data-mid']) : '')
    const stable = ids.length > 0 && new Set(ids).size === 1 && domId === ids[0]
    const scopes = [...rowScopeIds, ...paneScopeIds, ...resolvedIds]
    const records = feishu ? props.map(p => field(ownValue(p, 'messageItem'), 'message') || ownValue(p, 'message')).filter(Boolean) : []
    const matched = records.filter(m => field(m, 'id') === domId && field(m, 'chatId') === selected[0]?.conversationId)
    const fromIds = matched.map(m => field(m, 'fromId')).filter(v => typeof v === 'string' && v)
    const senderRef = fromIds.length && new Set(fromIds).size === 1 ? fromIds[0] : null
    // A p2p chat record names its peer: only when it is the selected chat and
    // its chatterId is exactly this message's fromId.
    const names = senderRef ? [
      ...props.flatMap(p => [ownValue(p, 'chatter'), field(ownValue(p, 'messageItem'), 'chatter')]).filter(c => field(c, 'id') === senderRef),
      ...props.map(p => ownValue(p, 'chat')).filter(c => c && field(c, 'id') === selected[0]?.conversationId && field(c, 'chatterId') === senderRef),
    ].map(c => field(c, 'name')).filter(v => typeof v === 'string' && v.trim()) : []
    const mine = props.filter(p => ownValue(p, 'messageId') === domId || matched.includes(field(ownValue(p, 'messageItem'), 'message')))
      .map(p => ownValue(p, 'mine')).filter(v => typeof v === 'boolean')
    return {id: domId, source: sourceMessages[0], records: matched, senderRef, senderConflict: new Set(fromIds).size > 1 || new Set(mine).size > 1, senderName: new Set(names).size === 1 ? names[0] : '', hasSourceId: ids.length > 0,
      mine: feishu ? (mine.length && new Set(mine).size === 1 ? mine[0] : null) : props.some(p => ownValue(p, 'mine') === true), stable,
      displayed: typeof node.checkVisibility === 'function' ? node.checkVisibility({checkOpacity: true, checkVisibilityCSS: true}) : node.getClientRects().length > 0,
      scopeShape: {rowCount: rowScopeIds.length, rowDistinct: new Set(rowScopeIds).size, rowMatched: rowScopeIds.filter(id => id === selected[0]?.conversationId).length,
        paneCount: paneScopeIds.length, paneDistinct: new Set(paneScopeIds).size, paneMatched: paneScopeIds.filter(id => id === selected[0]?.conversationId).length},
      hasRowScope: rowScopeIds.length > 0, hasPaneScope: paneScopeIds.length > 0,
      rowMatches: selected.length === 1 && rowScopeIds.every(id => id === selected[0].conversationId),
      paneMatches: selected.length === 1 && paneScopeIds.every(id => id === selected[0].conversationId),
      resolvedMatches: selected.length === 1 && resolvedIds.every(id => id === selected[0].conversationId),
      scopeVerified: stable && rowScopeIds.length > 0 && paneScopeIds.length > 0 && selected.length === 1 && scopes.every(id => id === selected[0].conversationId)}
  })
  const explicitScopeVerified = !!messageScopeId && selected.length === 1 && messageScopeId === selected[0].conversationId
  const sourceScopeVerified = nodes.length > 0 && messageEvidence.every(e => e.scopeVerified)
  const info = {
    pageUrl: location.href, origin: location.origin, identity,
    diagnostics: {...buildDiagnostics, catalogCount: entries.length, stableIdCount: entries.filter(x => x.conversationId).length,
      selectedCount: selected.length, identityPresent: !!(accountId && tenantId), identityKind,
      catalogIdentityVerified, messageScopeVerified: explicitScopeVerified || sourceScopeVerified,
      messageNodeCount: nodes.length, verifiedMessageScopeCount: messageEvidence.filter(e => e.scopeVerified).length,
      stableMessageIdCount: messageEvidence.filter(e => e.stable).length,
      messageRowScopeCount: messageEvidence.filter(e => e.hasRowScope).length, messagePaneScopeCount: messageEvidence.filter(e => e.hasPaneScope).length,
      rowScopeMatchCount: messageEvidence.filter(e => e.rowMatches).length, paneScopeMatchCount: messageEvidence.filter(e => e.paneMatches).length,
      resolvedScopeMatchCount: messageEvidence.filter(e => e.resolvedMatches).length,
      displayedMessageCount: messageEvidence.filter(e => e.displayed).length, firstMessageScopeShape: messageEvidence[0]?.scopeShape || null,
      catalogAttributes: [...new Set(cards.flatMap(x => x.getAttributeNames()))].filter(x => /^(data-|aria-)/.test(x)).slice(0, 30),
      loginFormPresent: !!document.querySelector('input[type="password"]')},
  }
  if (operation === 'status' || operation === 'qualify') return info
  if (!accountId || !tenantId) return fail('identity-unverified')
  if (operation === 'catalog') {
    if (!catalogIdentityVerified) return fail('conversation-identity-unverified')
    return {...info, entries: entries.slice(0, args.limit || 50).map(({conversationId, conversationName}) => {
      const position = feishu ? lastPosition(conversationId) : undefined
      return {conversationId, conversationName, identityKind: 'provider-dom', ...(position ? {lastMessagePosition: position} : {})}
    }), nextCursor: null, complete: false}
  }
  const matches = entries.filter(row => (!args.conversationId || row.conversationId === args.conversationId) && (!args.conversationName || row.conversationName === args.conversationName))
  if (!args.conversationId && !args.conversationName) return fail('conversation-selector-required')
  if (matches.length !== 1 || !matches[0].conversationId) return fail('conversation-not-unique')
  const chosen = matches[0]
  if (operation === 'select') {
    chosen.node.click()
    return {...info, conversationId: chosen.conversationId, conversationName: chosen.conversationName, action: 'selection-requested', contentValidated: false}
  }
  if (!['read', 'selectionReady', 'scroll'].includes(operation)) return fail('unsupported-operation')
  if (!chosen.selected || selected.length !== 1) return fail('conversation-not-selected')
  // A selected sidebar row can precede the message-pane update in a SPA.
  // Require an independent stable pane ID so that transition cannot mix chats.
  if (!nodes.length || !explicitScopeVerified && !sourceScopeVerified) return fail('message-scope-unverified')
  // Explicit pane metadata cannot override contradictory mounted row evidence.
  if (messageScopeId && !explicitScopeVerified || messageEvidence.some(e =>
    e.hasSourceId && !e.stable || e.hasRowScope && !e.rowMatches || e.hasPaneScope && !e.paneMatches || !e.resolvedMatches)) return fail('message-scope-unverified')
  if (operation === 'scroll') {
    // Only the scrollable ancestor that holds every verified row of this pane moves;
    // nothing is clicked. A column-reverse list scrolls up with negative scrollTop.
    const scrollable = element => /(auto|scroll)/.test(getComputedStyle(element).overflowY) && element.scrollHeight > element.clientHeight
    let box = nodes[0].parentElement
    while (box && !(scrollable(box) && nodes.every(node => box.contains(node)))) box = box.parentElement
    if (!box || box === document.body || box === document.documentElement) return fail('scroll-container-unverified')
    const reversed = getComputedStyle(box).flexDirection === 'column-reverse'
    const range = box.scrollHeight - box.clientHeight, before = box.scrollTop
    if (args.direction === 'bottom') box.scrollTop = reversed ? 0 : box.scrollHeight
    else box.scrollTop = before - Math.max(200, Math.round(box.clientHeight * 0.8))
    const atTop = reversed ? box.scrollTop <= -range + 1 : box.scrollTop <= 1
    return {...info, conversationId: chosen.conversationId, direction: args.direction === 'bottom' ? 'bottom' : 'up', moved: box.scrollTop !== before, atTop}
  }
  if (operation === 'selectionReady') {
    const messageIds = messageEvidence.map(e => e.id)
    if (messageIds.some(id => !id)) return fail('message-identity-unverified')
    if (new Set(messageIds).size !== messageIds.length) return fail('message-identity-duplicate')
    return {...info, conversationId: chosen.conversationId, messageIds}
  }
  const pad = value => String(value).padStart(2, '0')
  const calendar = (year, month, day) => {
    const date = new Date(Date.UTC(year, month - 1, day))
    return year >= 1000 && date.getUTCFullYear() === year && date.getUTCMonth() === month - 1 && date.getUTCDate() === day ? `${year}-${pad(month)}-${pad(day)}` : null
  }
  const today = new Date()
  const dayLabel = value => {
    const label = clean(value)
    const full = label.match(/^(\d{4})[-/年](\d{1,2})[-/月](\d{1,2})(?:日)?(?:\s+(?:星期[一二三四五六日天]|周[一二三四五六日天]|Monday|Tuesday|Wednesday|Thursday|Friday|Saturday|Sunday))?$/i)
    if (full) return calendar(...full.slice(1).map(Number))
    const offset = /^(今天|今日|today)$/i.test(label) ? 0 : /^(昨天|昨日|yesterday)$/i.test(label) ? 1 : null
    if (offset === null) return null // In particular, do not invent a year.
    const date = new Date(today.getFullYear(), today.getMonth(), today.getDate() - offset)
    return calendar(date.getFullYear(), date.getMonth() + 1, date.getDate())
  }
  const iso = value => {
    if (typeof value !== 'string') return null
    const parts = value.match(/^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::(\d{2})(?:\.\d{1,3})?)?(Z|[+-]\d{2}:\d{2})?$/)
    if (!parts || !calendar(...parts.slice(1, 4).map(Number)) || +parts[4] > 23 || +parts[5] > 59 || +(parts[6] || 0) > 59) return null
    if (parts[7] && !Number.isFinite(Date.parse(value))) return null
    return {time: value, time_zone: parts[7] ? 'explicit-offset' : 'unknown'}
  }
  const dateSelector = '.message-date-separator,.message-time-separator,.message-date-divider,.message-time-divider,.message-date,.message-time,[data-message-date]'
  const rowDays = new Map()
  if (feishu && nodes.length) {
    let root = messageScope || nodes[0].closest('[role="log"],.message-list,.message-list-container') || nodes[0].parentElement
    for (let depth = 0; root && !nodes.every(node => root.contains(node)) && depth < 45; depth++) root = root.parentElement
    // Only separators in the same message pane; never dates from the sidebar,
    // message bodies, quoted messages or another conversation.
    if (root && nodes.every(node => root.contains(node))) {
      let day = null
      for (const node of root.querySelectorAll(`${messageSelector},${dateSelector}`)) {
        if (nodes.includes(node)) rowDays.set(node, day)
        else if (!node.closest(`${messageSelector},.message-text,.message-post,blockquote`)) day = dayLabel(node.getAttribute('data-message-date') || node.textContent)
      }
    }
  }
  const feishuTime = (node, evidence) => {
    const raw_time = text(node, '.message-layout-time-tip') || text(node, 'time')
    const seconds = evidence.records.map(m => field(m, 'createTime')).filter(v => typeof v === 'number' || typeof v === 'string' && /^\d+(?:\.\d+)?$/.test(v)).map(Number)
    if (seconds.length && new Set(seconds).size === 1 && seconds[0] > 0 && seconds[0] <= 253402300799) {
      return {time: new Date(seconds[0] * 1000).toISOString(), raw_time, date_status: 'known', time_source: 'message-metadata', time_zone: 'explicit-offset'}
    }
    const datetime = iso(node.querySelector('time')?.getAttribute('datetime'))
    if (datetime) return {...datetime, raw_time, date_status: 'known', time_source: 'time-element'}
    const full = iso(raw_time)
    if (full) return {...full, raw_time, date_status: 'known', time_source: 'visible-time'}
    const labelled = raw_time.match(/^(.+?)\s+(\d{1,2}:\d{2}(?::\d{2})?)$/)
    const day = labelled ? dayLabel(labelled[1]) : rowDays.get(node)
    const clock = (labelled ? labelled[2] : raw_time).match(/^(\d{1,2}):(\d{2})(?::(\d{2}))?$/)
    if (day && clock && +clock[1] < 24 && +clock[2] < 60 && +(clock[3] || 0) < 60) {
      return {time: `${day}T${pad(clock[1])}:${clock[2]}:${clock[3] || '00'}`, raw_time, date_status: 'known', time_source: labelled ? 'visible-time' : 'date-separator', time_zone: 'unknown'}
    }
    return {time: raw_time, raw_time, date_status: 'unknown', time_source: 'raw', time_zone: 'unknown'}
  }
  // Use only dedicated author nodes owned by this message. Quoted author
  // names and a preceding row's author must never become this row's sender.
  const ownName = (item, evidence) => [...item.querySelectorAll('.message-info-name,.message-info__name,.message-sender-name,[data-sender-name]')]
    .filter(n => n.closest(messageSelector) === item && !n.closest('.message-text,.message-post,blockquote'))
    .map(n => clean(n.getAttribute('data-sender-name')) || clean(n.textContent) || clean(n.getAttribute('title'))).find(Boolean) || evidence.senderName
  // Feishu shows the author only on the first row of a run. Other rows reuse a
  // name by exact verified fromId, never by position; ambiguous names are dropped.
  const namesByRef = new Map()
  if (feishu) nodes.forEach((node, index) => {
    const evidence = messageEvidence[index], name = evidence.senderRef && !evidence.senderConflict && ownName(node, evidence)
    if (name) namesByRef.set(evidence.senderRef, (namesByRef.get(evidence.senderRef) || new Set()).add(name))
  })
  const refName = ref => namesByRef.get(ref)?.size === 1 ? [...namesByRef.get(ref)][0] : ''
  const messages = nodes.slice(-(args.limit || 50)).map(node => {
    const item = feishu ? node : node.closest('[data-tid="chat-pane-item"]')
    const evidence = messageEvidence[nodes.indexOf(node)]
    const id = evidence.id
    const sender = feishu ? ownName(item, evidence) || (evidence.senderRef && !evidence.senderConflict ? refName(evidence.senderRef) : '') || evidence.senderRef || '' : text(item, '[data-tid="message-author-name"]')
    const time = ownValue(evidence.source, 'originalArrivalTime') || item?.querySelector('time')?.getAttribute('datetime') || text(item, '.message-layout-time-tip')
    const body = feishu ? text(item, '.message-text,.message-post').replace(/展开$/, '').trim() : clean(node.innerText)
    const media = []
    for (const [type, selector] of [['image', feishu ? '.im-image-message img,.message-image img,.chat-image__wrapper img' : 'img[data-tid="lazy-image-2"]'], ['file', '[data-tid*="attachment"],[data-tid*="file-card"],a[download],[class*="file-message"]'], ['video', 'video'], ['voice', 'audio']]) {
      [...node.querySelectorAll(selector)].slice(0, 30).forEach((m, i) => media.push({type, locator: `dom://${provider}/message/${encodeURIComponent(id)}/${type}/${i}`, alt_text: clean(m.alt || m.getAttribute('aria-label'))}))
    }
    const selfMarker = !!item?.matches('.message-self') || (feishu ? [...item.querySelectorAll('.message-self')].some(n => n.closest(messageSelector) === item && !n.closest('.message-text,.message-post,blockquote')) : !!item?.querySelector('.message-self'))
    const selfEvidence = feishu ? [evidence.senderRef ? evidence.senderRef === accountId : null, evidence.mine, selfMarker ? true : null].filter(v => typeof v === 'boolean') : []
    const is_self = feishu ? (!evidence.senderConflict && selfEvidence.length && new Set(selfEvidence).size === 1 ? selfEvidence[0] : null) : evidence.mine || selfMarker
    return {id, sender, time, text: body, is_self, media, ...(feishu ? {sender_ref: evidence.senderRef, ...feishuTime(item, evidence)} : {})}
  }).filter(row => row.text || row.media.length)
  if (messages.some(row => !row.id)) return fail('message-identity-unverified')
  if (new Set(messages.map(row => row.id)).size !== messages.length) return fail('message-identity-duplicate')
  return {...info, conversationId: chosen.conversationId, conversationName: chosen.conversationName, messages}
}
