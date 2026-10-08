// Fixed, metadata-only diagnostics for qualifying a site's actual bootstrap.
// It never reads cookies/storage, scalar values or getters. Only two fixed,
// already-observed Redux getState methods may be called to describe state shapes.
export function inspectBootstrap(expectedOrigin) {
  if (location.origin !== expectedOrigin) return {error: 'page-changed'}
  const sensitive = /token|secret|password|credential|cookie|authorization/i
  const safeKey = key => /^[A-Za-z_$][A-Za-z0-9_$]{0,79}$/.test(key) && !sensitive.test(key)
  const candidate = /user|tenant|account|lark|feishu|teams|state|store|preload|initial|config|boot|sdk|context|environment|services/i
  const own = object => {
    try { return Object.getOwnPropertyDescriptors(object) } catch { return {} }
  }
  const shape = object => Object.entries(own(object)).filter(([key]) => safeKey(key)).slice(0, 60).map(([key, d]) => ({key, type: 'value' in d ? (d.value === null ? 'null' : typeof d.value) : 'accessor'}))
  const globals = Object.entries(own(window)).filter(([key, d]) => candidate.test(key) && safeKey(key) && 'value' in d && d.value && typeof d.value === 'object').slice(0, 30).map(([key, d]) => ({name: key, fields: shape(d.value), children: Object.entries(own(d.value)).filter(([name, value]) => safeKey(name) && 'value' in value && value.value && typeof value.value === 'object').slice(0, 10).map(([name, value]) => ({name, fields: shape(value.value)}))}))
  const cards = ['.a11y_feed_card_item,[id^="title-chat-list-item_"]', '.messageItem-wrapper[data-id],[data-tid="chat-pane-message"]'].map(selector => document.querySelector(selector)).filter(Boolean)
  const componentShapes = []
  const conversationComparisons = []
  const identityShapes = []
  const clientShapes = []
  const seenClients = new Set()
  const currentTreeShapes = cards.map(card => {
    const fiber = Object.entries(own(card)).find(([key]) => /^__react(?:Fiber|InternalInstance)\$/.test(key))?.[1]?.value
    let root = fiber, depth = 0
    const parents = new Set()
    while (root?.return && depth < 512 && !parents.has(root)) { parents.add(root); root = root.return; depth++ }
    const current = root?.stateNode?.current
    const members = new Set(), pending = current ? [current] : []
    let repeated = false
    while (pending.length && members.size < 50000) {
      const node = pending.pop()
      if (members.has(node)) { repeated = true; break }
      members.add(node)
      if (node.sibling) pending.push(node.sibling)
      if (node.child) pending.push(node.child)
    }
    return {target: card.matches('.messageItem-wrapper,[data-tid="chat-pane-message"]') ? 'message' : 'catalog', depth, hasCurrent: !!current, rootMatchesCurrent: current === root, alternateRootMatchesCurrent: current === root?.alternate, repeated, count: members.size, truncated: pending.length > 0, primaryCurrent: members.has(fiber), alternateCurrent: members.has(fiber?.alternate)}
  })
  // Bounded selection evidence only: never export conversation IDs or labels.
  const selectionShapes = [...document.querySelectorAll('[id^="title-chat-list-item_"]')].slice(0, 100).map(node => {
    const id = node.id.slice('title-chat-list-item_'.length)
    const root = Object.entries(own(node)).find(([key]) => /^__react(?:Fiber|InternalInstance)\$/.test(key))?.[1]?.value
    const branches = [root, own(root || {}).alternate?.value].map(start => {
      const contexts = []
      for (let fiber = start, depth = 0; fiber && depth < 12; depth++, fiber = own(fiber).return?.value) {
        const props = own(fiber).memoizedProps?.value, conversation = own(props || {}).conversation?.value
        if (conversation) contexts.push({depth, matchesDomId: own(conversation).internalId?.value === id, selected: own(conversation).isSelected?.value === true})
      }
      return {present: !!start, contexts}
    })
    return {branches, domSelected: !!node.closest('[aria-selected="true"],[data-is-selected="true"],[data-selected="true"]'), treeRowPresent: !!node.closest('[role="treeitem"]')}
  })
  for (const card of cards) {
    for (const [key, descriptor] of Object.entries(own(card)).filter(([key]) => /^__react(?:Props|Fiber|InternalInstance)\$/.test(key))) {
      const root = descriptor.value
      if (!root || typeof root !== 'object') continue
      let fiber = root
      for (let depth = 0; depth < (location.hostname.endsWith('.feishu.cn') ? 45 : 100) && fiber; depth++) {
        const props = key.startsWith('__reactProps') ? fiber : own(fiber).memoizedProps?.value
        const userContext = props && own(props).userContext?.value
        const client = props && own(props).client?.value
        if (client && !seenClients.has(client) && clientShapes.length < 3) {
          seenClients.add(client)
          clientShapes.push({depth, fields: shape(client), prototype: shape(Object.getPrototypeOf(client) || {}), parentPrototype: shape(Object.getPrototypeOf(Object.getPrototypeOf(client) || {}) || {}), innerClient: shape(own(client)._client?.value || {}), context: shape(own(client).ctx?.value || {})})
        }
        if (userContext && identityShapes.length < 8) {
          const user = own(userContext).user?.value
          const auth = own(userContext).authenticationUser?.value
          const tenant = own(user || {}).tenant?.value, profile = own(auth || {}).profile?.value
          const userId = own(user || {}).id?.value, tenantId = own(tenant || {}).id?.value, oid = own(profile || {}).oid?.value, tid = own(profile || {}).tid?.value
          identityShapes.push({depth, user: shape(user || {}), tenant: shape(tenant || {}), authenticationUser: shape(auth || {}), profile: shape(profile || {}), comparisons: {
            userIdPresent: typeof userId === 'string' && !!userId, tenantIdPresent: typeof tenantId === 'string' && !!tenantId,
            oidPresent: typeof oid === 'string' && !!oid, tidPresent: typeof tid === 'string' && !!tid,
            userMatchesOid: typeof userId === 'string' && !!userId && userId === oid,
            userMatchesAccountId: typeof userId === 'string' && !!userId && userId === own(auth || {}).id?.value,
            tenantMatchesTid: typeof tenantId === 'string' && !!tenantId && tenantId === tid,
          }})
        }
        if (card.id?.startsWith('title-chat-list-item_') && props) {
          const conversation = own(props).conversation?.value
          if (conversation) {
            const id = card.id.slice('title-chat-list-item_'.length)
            const fields = own(conversation)
            conversationComparisons.push({depth, domMatchesId: fields.id?.value === id, domMatchesInternalId: fields.internalId?.value === id, selected: fields.isSelected?.value === true})
          }
        }
        if (props && typeof props === 'object') componentShapes.push({target: card.matches('.messageItem-wrapper,[data-tid="chat-pane-message"]') ? 'message' : 'catalog', depth, fields: shape(props), children: Object.entries(own(props)).filter(([name, d]) => safeKey(name) && !['children', 'style'].includes(name) && 'value' in d && d.value && typeof d.value === 'object').slice(0, 8).map(([name, d]) => ({name, fields: shape(d.value), children: Object.entries(own(d.value)).filter(([name, d]) => safeKey(name) && /^(value|user|currentUser|account|profile|tenant|context|environment|variables|__fragmentOwner)$/.test(name) && 'value' in d && d.value && typeof d.value === 'object').slice(0, 8).map(([name, d]) => ({name, fields: shape(d.value)}))}))})
        if (key.startsWith('__reactProps')) break
        fiber = own(fiber).return?.value
      }
    }
  }
  const scriptResources = [...new Set(performance.getEntriesByType('resource').filter(r => r.initiatorType === 'script').map(r => {try { const u = new URL(r.name); return u.protocol === 'https:' ? u.origin + u.pathname : null } catch { return null }}).filter(Boolean))].slice(0, 100)
  const stateShapes = []
  if (location.hostname.endsWith('.feishu.cn')) {
    for (const name of ['__coreAppStore', '__feedStore']) {
      const store = own(window)[name]?.value
      const method = store && own(store).getState?.value
      if (typeof method !== 'function') continue
      try {
        const state = method.call(store)
        if (!state || Object.getPrototypeOf(state) !== Object.prototype) continue
        const slices = Object.entries(own(state)).filter(([key, d]) => safeKey(key) && 'value' in d && d.value && typeof d.value === 'object').slice(0, 30).map(([key, d]) => ({name: key, fields: shape(d.value), children: Object.entries(own(d.value)).filter(([name, value]) => safeKey(name) && 'value' in value && value.value && typeof value.value === 'object').slice(0, 12).map(([name, value]) => ({name, fields: shape(value.value), sampleFields: shape(Object.values(own(value.value)).find(x => 'value' in x && x.value && typeof x.value === 'object')?.value || {})}))}))
        stateShapes.push({name, fields: shape(state), slices})
      } catch { stateShapes.push({name, error: 'state-shape-unavailable'}) }
    }
  }
  return {globals, componentShapes, scriptResources, stateShapes, conversationComparisons, identityShapes, clientShapes, selectionShapes, currentTreeShapes}
}
