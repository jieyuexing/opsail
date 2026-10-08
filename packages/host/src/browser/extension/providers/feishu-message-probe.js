// Self-contained by design: Chrome serializes scripting.executeScript `func`
// values and does not retain imported module bindings in the target page.
// This probe never accepts a chat ID, message position, descriptor, or SDK args
// from IPC. It only reads the currently selected Feishu feed/chat evidence.
export async function inspectFeishuSelectedMessage(expectedOrigin, expectedIdentity, historyRequest = null) {
  const descriptor = '1020|im.v1.GetChatMessagesRequest|im.v1.GetChatMessagesResponse|1|GET_CHAT_MESSAGES'
  const FIRST_SCREEN = 1
  const PREVIOUS_PAGE = 3
  const SYNC_SERVER_DATA = 3
  const sensitive = /token|cookie|secret|password|credential|authorization|auth/i
  const safeKey = /^[A-Za-z_$][A-Za-z0-9_$]{0,79}$/
  const own = (value, key) => {
    if (!value || (typeof value !== 'object' && typeof value !== 'function')) return undefined
    const property = Object.getOwnPropertyDescriptor(value, key)
    return property && 'value' in property ? property.value : undefined
  }
  const typeOf = value => value === null ? 'null' : Array.isArray(value) ? 'array' : typeof value
  const numeric = value => typeof value === 'number' ? value : typeof value === 'string' && /^(?:0|[1-9][0-9]*)(?:\.[0-9]+)?$/.test(value) ? Number(value) : NaN
  const safeKeys = value => !value || typeof value !== 'object' || Array.isArray(value) ? [] : Object.keys(value).filter(key => safeKey.test(key) && !sensitive.test(key)).slice(0, 32)
  const shape = value => ({type: typeOf(value), keys: safeKeys(value).map(key => ({key, type: typeOf(own(value, key))}))})
  const structure = (value, {maxDepth = 3, maxFields = 64} = {}) => {
    let remaining = maxFields
    const visit = (current, depth) => {
      const type = typeOf(current)
      if (type === 'string') return {type, length: current.length}
      if (type !== 'object' && type !== 'array') return {type}
      if (depth >= maxDepth) return {type}
      if (type === 'array') return {type, count: current.length, items: current.slice(0, 3).map(item => visit(item, depth + 1))}
      const fields = []
      for (const key of safeKeys(current)) {
        if (remaining < 1) break
        remaining -= 1
        fields.push({key, value: visit(own(current, key), depth + 1)})
      }
      return {type, fields}
    }
    return visit(value, 0)
  }
  const bodyStructure = content => {
    const richText = own(content, 'richText'), innerText = own(richText, 'innerText'), elementIds = own(richText, 'elementIds'), elements = own(richText, 'elements')
    const firstElements = Array.isArray(elementIds) && elements && typeof elements === 'object'
      ? elementIds.slice(0, 3).map(id => structure(own(elements, id))) : []
    const values = own(content, 'values')
    return {
      innerTextLength: typeof innerText === 'string' ? innerText.length : null,
      elementIdsCount: Array.isArray(elementIds) ? elementIds.length : null,
      elements: {type: typeOf(elements), count: elements && typeof elements === 'object' && !Array.isArray(elements) ? Object.keys(elements).length : null},
      firstElements,
      ...(values !== undefined ? {values: {type: typeOf(values), count: Array.isArray(values) ? values.length : values && typeof values === 'object' ? Object.keys(values).length : null, structure: structure(values)}} : {}),
    }
  }
  const renderRichText = richText => {
    const innerText = own(richText, 'innerText'), elementIds = own(richText, 'elementIds'), elements = own(richText, 'elements')
    const innerTextLength = typeof innerText === 'string' ? innerText.length : null
    if (!Array.isArray(elementIds) || !elements || typeof elements !== 'object' || Array.isArray(elements) || elementIds.length < 1) {
      return typeof innerText === 'string' && innerText ? {text: innerText, text_projection: 'richtext-innerText', renderedElementCount: 0, unknownElementCount: 0, inlineMediaCount: 0, complete: true} :
        {text: '', text_projection: 'unavailable', renderedElementCount: 0, unknownElementCount: 0, inlineMediaCount: 0, complete: false, innerTextLength}
    }
    let renderedElementCount = 0, unknownElementCount = 0, inlineMediaCount = 0, incomplete = false
    const unknownElementStructures = []
    const active = new Set()
    const string = value => typeof value === 'string' ? value : null
    const preferredString = values => values.find(value => typeof value === 'string' && value.length > 0) ?? values.find(value => typeof value === 'string') ?? null
    const unknown = element => {
      incomplete = true; unknownElementCount += 1
      if (unknownElementStructures.length >= 3) return
      const property = own(element, 'property'), childIds = own(element, 'childIds'), tag = own(element, 'tag')
      unknownElementStructures.push({tag: Number.isSafeInteger(tag) ? tag : null, tagType: typeOf(tag), property: structure(property, {maxDepth: 2, maxFields: 24}), childCount: Array.isArray(childIds) ? childIds.length : null})
    }
    const render = (id, depth, listType = null, listIndex = 0) => {
      if (depth > 32 || renderedElementCount >= 2048 || active.has(id)) { unknown(own(elements, id)); return '' }
      const element = own(elements, id)
      if (!element || typeof element !== 'object') { unknown(null); return '' }
      active.add(id); renderedElementCount += 1
      const property = own(element, 'property'), childIds = own(element, 'childIds')
      const children = Array.isArray(childIds) ? childIds : childIds === undefined ? [] : null
      if (!children) { unknown(element); active.delete(id); return '' }
      let text = '', known = false
      const codeBlockV2 = own(property, 'codeBlockV2'), textProperty = own(property, 'text'), anchor = own(property, 'anchor'), at = own(property, 'at'), mention = own(property, 'mention'), emotion = own(property, 'emotion')
      if (codeBlockV2 && typeof codeBlockV2 === 'object') {
        const lines = own(codeBlockV2, 'contents')
        if (Array.isArray(lines)) {
          const renderedLines = []
          let valid = true
          for (const line of lines) {
            const fragments = own(line, 'contents')
            if (!Array.isArray(fragments)) { valid = false; break }
            const rendered = []
            for (const fragment of fragments) {
              const value = own(fragment, 'content')
              if (typeof value !== 'string') { valid = false; break }
              rendered.push(value)
            }
            if (!valid) break
            renderedLines.push(rendered.join(''))
          }
          if (valid) { text = renderedLines.join('\n'); known = true }
        }
      } else if (textProperty && typeof textProperty === 'object') {
        const link = own(textProperty, 'type') === 'link'
        const value = preferredString(link ? [own(textProperty, 'title'), own(textProperty, 'textContent'), own(textProperty, 'content')] : [own(textProperty, 'content')])
        if (value !== null) { text = value; known = true }
      } else if (anchor && typeof anchor === 'object') {
        const preview = own(anchor, 'urlPreviewEntity')
        const value = preferredString([own(preview, 'serverTitle'), own(anchor, 'textContent'), own(anchor, 'content'), own(anchor, 'href')])
        if (value !== null) { text = value; known = true }
      } else if (at && typeof at === 'object') {
        const value = string(own(at, 'content'))
        text = value === null ? '' : (value.startsWith('@') ? value : '@' + value); known = value !== null
      } else if (mention && typeof mention === 'object') {
        const value = string(own(mention, 'content'))
        text = value === null ? '' : (value.startsWith('#') ? value : '#' + value); known = value !== null
      } else if (emotion && typeof emotion === 'object') {
        const key = string(own(emotion, 'key'))
        text = key === null ? '' : '[' + key + ']'; known = key !== null
      } else if (own(property, 'br') !== undefined || own(property, 'lineBreak') !== undefined) {
        text = '\n'; known = true
      } else if (own(property, 'image') !== undefined || own(property, 'media') !== undefined) {
        inlineMediaCount += 1; known = true
      }
      const isOrdered = own(property, 'ol') !== undefined, isUnordered = own(property, 'ul') !== undefined, isListItem = own(property, 'li') !== undefined
      if (!known && (own(property, 'paragraph') !== undefined || own(property, 'quote') !== undefined || isOrdered || isUnordered || isListItem || own(property, 'figure') !== undefined || own(property, 'docs') !== undefined)) known = true
      const childText = children.map((child, index) => render(child, depth + 1, isOrdered ? 'ol' : isUnordered ? 'ul' : null, index)).join('')
      if (!known && children.length === 0) unknown(element)
      if (isListItem && listType) text = (listType === 'ol' ? String(listIndex + 1) + '. ' : '- ') + (text + childText)
      else text += childText
      active.delete(id)
      return text
    }
    const text = elementIds.map((id, index) => render(id, 0, null, index)).reduce((joined, part) => joined && part ? joined + '\n' + part : joined + part, '')
    return {text: text.slice(0, 64_000), text_projection: !text && inlineMediaCount ? 'richtext-media-only' : 'richtext-elements', renderedElementCount, unknownElementCount, inlineMediaCount, unknownElementStructures, complete: !incomplete && text.length <= 64_000, innerTextLength}
  }
  // Immutable Record methods are inherited through the generated MessageItem
  // class chain. Search a bounded chain and accept only data-descriptor
  // functions; page accessors are never
  // invoked merely to discover a method.
  const method = (value, name) => {
    let cursor = value
    for (let depth = 0; cursor && depth < 8; depth += 1, cursor = Object.getPrototypeOf(cursor)) {
      const descriptor = Object.getOwnPropertyDescriptor(cursor, name)
      if (!descriptor) continue
      return 'value' in descriptor && typeof descriptor.value === 'function' ? {fn: descriptor.value, depth} : null
    }
    return null
  }
  // Same membership semantics as providers/dom.js: a DOM-attached fiber can
  // be stale after commit, so inspect the committed root's child/sibling tree.
  // The limits cover the observed deeply nested host tree without accepting an
  // unbounded page-owned traversal.
  const currentTrees = new WeakMap()
  const currentFiber = (fiber, {maxSteps = 512, maxTreeNodes = 50000} = {}) => {
    if (!fiber || typeof fiber !== 'object' || !Number.isInteger(maxSteps) || maxSteps < 1 || maxSteps > 4096 || !Number.isInteger(maxTreeNodes) || maxTreeNodes < 1 || maxTreeNodes > 50000) return null
    const alternate = own(fiber, 'alternate')
    if (!alternate) return fiber
    let root = fiber
    const ancestors = new Set()
    while (own(root, 'return')) {
      if (ancestors.has(root) || ancestors.size >= maxSteps) return null
      ancestors.add(root); root = own(root, 'return')
    }
    const owner = own(root, 'stateNode'), current = own(owner, 'current')
    if (!current || own(current, 'stateNode') !== owner || (current !== root && current !== own(root, 'alternate'))) return null
    let members = currentTrees.get(current)
    if (!members) {
      members = new Set()
      const pending = [current]
      while (pending.length) {
        const node = pending.pop()
        if (!node || typeof node !== 'object' || members.has(node) || members.size >= maxTreeNodes) return null
        members.add(node)
        const sibling = own(node, 'sibling'), child = own(node, 'child')
        if (sibling) pending.push(sibling)
        if (child) pending.push(child)
      }
      currentTrees.set(current, members)
    }
    return members.has(fiber) ? fiber : members.has(alternate) ? alternate : null
  }
  const origin = () => { try { return location.origin } catch { return '' } }
  const timeout = promise => {
    let timer
    const expiry = new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('timeout')), 15_000) })
    return Promise.race([Promise.resolve(promise), expiry]).finally(() => clearTimeout(timer))
  }
  const hash = async rawId => {
    const source = `feishu\u0000${expectedOrigin}\u0000${rawId}`
    const bytes = new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(source)))
    return [...bytes].map(item => item.toString(16).padStart(2, '0')).join('')
  }
  const base = {qualified: false, accountHash: expectedIdentity?.accountHash ?? null, tenantHash: expectedIdentity?.tenantHash ?? null}
  if (origin() !== expectedOrigin) return {...base, reason: 'page-changed', diagnostics: {originMatched: false}}

  const adapter = own(window, 'configurationAdapter')
  const passport = own(adapter, 'passport'), transport = own(adapter, 'transport')
  const getCurUserInfo = own(passport, 'getCurUserInfo'), callSdkApi = own(transport, 'callSdkApi')
  if (typeof getCurUserInfo !== 'function' || typeof callSdkApi !== 'function') return {...base, reason: 'fixed-api-unavailable', diagnostics: {originMatched: true}}
  const identityMatches = async value => {
    const user = own(value, 'user'), tenant = own(user, 'tenant')
    const userId = own(user, 'id'), tenantId = own(tenant, 'id')
    if (typeof userId !== 'string' || !userId || typeof tenantId !== 'string' || !tenantId) return false
    if (own(window, 'userId') !== userId || own(passport, 'userId') !== userId) return false
    if (!expectedIdentity || typeof expectedIdentity.accountHash !== 'string' || typeof expectedIdentity.tenantHash !== 'string') return false
    const [accountHash, tenantHash] = await Promise.all([hash(userId), hash(tenantId)])
    return accountHash === expectedIdentity.accountHash && tenantHash === expectedIdentity.tenantHash &&
      own(window, 'userId') === userId && own(own(own(window, 'configurationAdapter'), 'passport'), 'userId') === userId
  }
  let before
  try { before = await timeout(getCurUserInfo.call(passport)) } catch { return {...base, reason: 'identity-read-failed', diagnostics: {originMatched: true}} }
  if (!await identityMatches(before)) return {...base, reason: 'identity-unverified', diagnostics: {originMatched: true, identityBeforeMatched: false}}

  // Static source establishes __feedStore.status.activeFeedId and previews[*]
  // as the selected feed/chat state.  Require its preview to self-identify and
  // require a finite lastMessagePosition before making any SDK call.
  const selection = () => {
    const store = own(window, '__feedStore'), getState = own(store, 'getState')
    if (typeof getState !== 'function') return {reason: 'selected-chat-store-unavailable'}
    let state
    try { state = getState.call(store) } catch { return {reason: 'selected-chat-store-unavailable'} }
    const status = own(state, 'status'), previews = own(state, 'previews')
    const chatId = own(status, 'activeFeedId')
    const preview = own(previews, chatId)
    const previewFeedId = own(preview, 'feedId'), position = own(preview, 'lastMessagePosition')
    if (typeof chatId !== 'string' || !chatId) return {reason: 'selected-chat-unavailable'}
    if (!preview || previewFeedId !== chatId) return {reason: 'selected-chat-id-unverified'}
    if (!Number.isSafeInteger(position) || position < 1) return {reason: 'selected-chat-position-unverified'}
    return {chatId, position}
  }
  // Static assets use Immutable MessageItem Records. Its public `get` returns
  // the nested message Record, whose public `get` supplies id/chatId/position/
  // createTime; createTime is Unix seconds (f2ff961b.js @ 1188300).
  const messageModel = expected => {
    const nodes = [...document.querySelectorAll('.messageItem-wrapper[data-id]')].slice(-50)
    const diagnostics = {nodeCount: nodes.length, attachedFiberCount: 0, currentFiberResolvedCount: 0, currentFiberUnresolvedCount: 0, messageItemSeenCount: 0, recordGetMethodCount: 0, recordGetMethodMissingCount: 0, recordGetMaxDepth: null, wrappedMessageRecordCount: 0, messageGetFailureCount: 0, messageIdStringCount: 0, chatIdStringCount: 0, positionIntegerCount: 0, createTimeScalarCount: 0, stableIdMatchedCount: 0, chatMatchedCount: 0, positionMatchedCount: 0, canonicalTimeMatchedCount: 0, verifiedSameItemCount: 0}
    for (const node of nodes) {
      const domId = typeof node?.getAttribute === 'function' ? node.getAttribute('data-id') : null
      const fiberKey = Object.getOwnPropertyNames(node || {}).find(key => /^__react(?:Fiber|InternalInstance)\$/.test(key))
      const attached = own(node, fiberKey)
      if (attached) diagnostics.attachedFiberCount += 1
      let fiber = currentFiber(attached)
      if (!fiber) { if (attached) diagnostics.currentFiberUnresolvedCount += 1; continue }
      diagnostics.currentFiberResolvedCount += 1
      const seen = new Set()
      for (let depth = 0; fiber && depth < 45; depth += 1) {
        if (seen.has(fiber)) break
        seen.add(fiber)
        const props = own(fiber, 'memoizedProps')
        const item = own(props, 'messageItem')
        if (item) diagnostics.messageItemSeenCount += 1
        const get = method(item, 'get')
        if (!get) { if (item) diagnostics.recordGetMethodMissingCount += 1; fiber = currentFiber(own(fiber, 'return')); continue }
        diagnostics.recordGetMethodCount += 1
        diagnostics.recordGetMaxDepth = Math.max(diagnostics.recordGetMaxDepth ?? 0, get.depth)
        let messageId, chatId, position, createTime
        try {
          const message = get.fn.call(item, 'message')
          const getMessage = method(message, 'get')
          if (!getMessage) { diagnostics.recordGetMethodMissingCount += 1; continue }
          diagnostics.wrappedMessageRecordCount += 1
          diagnostics.recordGetMaxDepth = Math.max(diagnostics.recordGetMaxDepth ?? 0, getMessage.depth)
          messageId = getMessage.fn.call(message, 'id')
          chatId = getMessage.fn.call(message, 'chatId')
          position = getMessage.fn.call(message, 'position')
          createTime = getMessage.fn.call(message, 'createTime')
        } catch { diagnostics.messageGetFailureCount += 1; break }
        const stableId = typeof messageId === 'string' && messageId.length > 0
        const chatIdValid = typeof chatId === 'string' && chatId.length > 0
        const positionValid = Number.isSafeInteger(position) && position > 0
        const createTimeScalar = typeof createTime === 'string' || typeof createTime === 'number'
        const seconds = createTimeScalar ? Number(createTime) : NaN
        const stableIdMatched = stableId && domId === messageId
        const chatMatched = chatId === expected.chatId
        const positionMatched = positionValid && position === expected.position
        const canonicalTimeMatched = Number.isFinite(seconds) && seconds > 0
        if (stableId) diagnostics.messageIdStringCount += 1
        if (chatIdValid) diagnostics.chatIdStringCount += 1
        if (positionValid) diagnostics.positionIntegerCount += 1
        if (createTimeScalar) diagnostics.createTimeScalarCount += 1
        if (stableIdMatched) diagnostics.stableIdMatchedCount += 1
        if (chatMatched) diagnostics.chatMatchedCount += 1
        if (positionMatched) diagnostics.positionMatchedCount += 1
        if (canonicalTimeMatched) diagnostics.canonicalTimeMatchedCount += 1
        if (stableIdMatched && chatMatched && positionMatched && canonicalTimeMatched) diagnostics.verifiedSameItemCount += 1
        break
        fiber = currentFiber(own(fiber, 'return'))
      }
    }
    return {...diagnostics, valid: diagnostics.verifiedSameItemCount > 0}
  }
  const selected = selection()
  if (selected.reason) return {...base, reason: selected.reason, diagnostics: {originMatched: true, identityBeforeMatched: true}}
  let history
  if (historyRequest !== null) {
    if (!historyRequest || typeof historyRequest !== 'object' || Array.isArray(historyRequest) ||
      Object.keys(historyRequest).some(key => !['mode', 'expectedChatId', 'priorPosition', 'limit'].includes(key))) {
      return {...base, reason: 'history-request-invalid', diagnostics: {originMatched: true, identityBeforeMatched: true}}
    }
    const {mode, expectedChatId, priorPosition, limit = 50} = historyRequest
    if ((mode !== 'initial' && mode !== 'previous') || typeof expectedChatId !== 'string' || expectedChatId !== selected.chatId ||
      !Number.isSafeInteger(limit) || limit < 1 || limit > 200 ||
      (mode === 'initial' ? priorPosition !== undefined : !Number.isSafeInteger(priorPosition) || priorPosition < 1 || priorPosition > selected.position)) {
      return {...base, reason: 'history-request-invalid', diagnostics: {originMatched: true, identityBeforeMatched: true}}
    }
    history = {mode, position: mode === 'initial' ? selected.position : priorPosition, limit}
  }
  const model = messageModel(selected)
  if (!model.valid) return {...base, reason: 'selected-chat-anchor-unverified', diagnostics: {originMatched: true, identityBeforeMatched: true, messageModel: model}}
  if (origin() !== expectedOrigin) return {...base, reason: 'page-changed', diagnostics: {originMatched: false, identityBeforeMatched: true, messageModel: model}}

  const request = {chatId: selected.chatId, position: history?.position ?? selected.position, scene: history?.mode === 'previous' ? PREVIOUS_PAGE : FIRST_SCREEN, count: history?.limit ?? 1, strategy: SYNC_SERVER_DATA, redundancyCount: 0, subscribChatEvent: false, needResponse: true}
  let wrapper
  try {
    // Exactly two arguments: no caller options, request headers, trace IDs, or arbitrary descriptor.
    wrapper = await timeout(callSdkApi.call(transport, descriptor, request))
  } catch (error) {
    return {...base, reason: error?.message === 'timeout' ? 'fixed-message-probe-timeout' : 'fixed-message-probe-failed', diagnostics: {originMatched: true, identityBeforeMatched: true, messageModel: model}}
  }
  let after
  try { after = await timeout(getCurUserInfo.call(passport)) } catch { return {...base, reason: 'identity-read-failed', diagnostics: {originMatched: true, identityBeforeMatched: true, messageModel: model}} }
  const identityAfterMatched = await identityMatches(after)
  const originAfterMatched = origin() === expectedOrigin
  const selectedAfter = selection()
  const selectionStable = !selectedAfter.reason && selectedAfter.chatId === selected.chatId && selectedAfter.position === selected.position
  if (!originAfterMatched || !identityAfterMatched || !selectionStable) {
    const reason = !originAfterMatched ? 'page-changed' : !identityAfterMatched ? 'identity-changed' : 'selected-chat-changed'
    return {...base, reason, diagnostics: {originMatched: originAfterMatched, identityBeforeMatched: true, identityAfterMatched, selectionStable, messageModel: model}}
  }

  const data = own(wrapper, 'data'), messageItems = own(data, 'messageItems'), entity = own(data, 'entity')
  const messages = own(entity, 'messages')
  const itemEvidence = Array.isArray(messageItems) ? messageItems.map(item => {
    const itemId = own(item, 'itemId'), message = own(messages, itemId)
    const messageId = own(message, 'id'), chatId = own(message, 'chatId'), position = own(message, 'position'), createTime = own(message, 'createTime')
    const seconds = typeof createTime === 'string' || typeof createTime === 'number' ? Number(createTime) : NaN
    return {
      stableId: typeof itemId === 'string' && itemId === messageId,
      selectedChat: chatId === selected.chatId,
      validPosition: Number.isSafeInteger(position) && position > 0,
      validTimeSeconds: Number.isFinite(seconds) && seconds > 0,
    }
  }) : []
  const responseIdentityMatched = itemEvidence.length > 0 && itemEvidence.every(item =>
    item.stableId && item.selectedChat && item.validPosition && item.validTimeSeconds)
  let firstMessageMetadata
  let firstItemFields
  if (responseIdentityMatched) {
    const firstItemId = own(messageItems[0], 'itemId')
    const firstMessage = own(messages, firstItemId)
    // `shape` emits only bounded non-sensitive own key names and value types.
    // It is intentionally evaluated only after the response has been bound to
    // the selected chat through stable ID, chat ID, position, and timestamp.
    const content = own(firstMessage, 'content')
    firstMessageMetadata = {message: shape(firstMessage), content: shape(content), richText: shape(own(content, 'richText')), bodyStructure: bodyStructure(content)}
    firstItemFields = Object.fromEntries(['id', 'chatId', 'position', 'createTime', 'type', 'content', 'fromId', 'editVersion', 'isEdited', 'editTimeMs', 'isRecalled', 'isDeleted', 'rootId', 'parentId']
      .map(key => [key, typeOf(own(firstMessage, key))]))
  }
  const response = {
    wrapper: shape(wrapper),
    data: data && typeof data === 'object' && !Array.isArray(data) ? shape(data) : {present: false},
    messageItems: {present: messageItems !== undefined, type: typeOf(messageItems), count: Array.isArray(messageItems) ? messageItems.length : null},
    responseEvidence: {
      stableIds: itemEvidence.length > 0 && itemEvidence.every(item => item.stableId),
      selectedChat: itemEvidence.length > 0 && itemEvidence.every(item => item.selectedChat),
      validPositions: itemEvidence.length > 0 && itemEvidence.every(item => item.validPosition),
      validTimeSeconds: itemEvidence.length > 0 && itemEvidence.every(item => item.validTimeSeconds),
      dataCompleteBoolean: typeof own(data, 'dataComplete') === 'boolean',
    },
    pageSchema: {
      invalidPositionsArray: Array.isArray(own(data, 'invalidPositions')) && own(data, 'invalidPositions').every(item => Number.isSafeInteger(item) && item > 0),
      missingPositionsArray: Array.isArray(own(data, 'missingPositions')) && own(data, 'missingPositions').every(item => Number.isSafeInteger(item) && item > 0),
      dataCompleteBoolean: typeof own(data, 'dataComplete') === 'boolean',
      ...(firstItemFields ? {firstItemFields} : {}),
    },
    ...(firstMessageMetadata ? {firstMessageMetadata} : {}),
  }
  const schemaRecognized = Array.isArray(messageItems) && itemEvidence.length === messageItems.length
  if (history) {
    const positions = value => Array.isArray(value) && value.every(item => Number.isSafeInteger(item) && item > 0) ? value : null
    const invalidPositions = positions(own(data, 'invalidPositions'))
    const missingPositions = positions(own(data, 'missingPositions'))
    const dataComplete = own(data, 'dataComplete')
    if (!responseIdentityMatched || !invalidPositions || !missingPositions || typeof dataComplete !== 'boolean') {
      return {...base, reason: 'selected-chat-page-unrecognized', diagnostics: {originMatched: true, identityBeforeMatched: true, identityAfterMatched: true, selectionStable: true, messageModel: model, response}}
    }
    const project = raw => {
      const id = own(raw, 'id'), chatId = own(raw, 'chatId'), position = own(raw, 'position')
      const createTime = own(raw, 'createTime'), seconds = numeric(createTime)
      const type = own(raw, 'type'), content = own(raw, 'content')
      const failure = field => ({failure: {type: Number.isSafeInteger(type) ? type : null, field}})
      if (typeof id !== 'string' || !id || chatId !== selected.chatId || !Number.isSafeInteger(position) || position < 1 || !Number.isFinite(seconds) || seconds <= 0 ||
        typeof type !== 'number') return failure('message')
      const sentAt = new Date(seconds * 1000)
      if (!Number.isFinite(sentAt.getTime())) return failure('createTime')
      const fromId = own(raw, 'fromId'), chatters = own(entity, 'chatters'), chatter = own(chatters, fromId), name = own(chatter, 'name')
      const editVersion = own(raw, 'editVersion'), isEdited = own(raw, 'isEdited'), editTimeMs = own(raw, 'editTimeMs')
      const isRecalled = own(raw, 'isRecalled'), isDeleted = own(raw, 'isDeleted')
      if (editVersion !== undefined && editVersion !== null && (!Number.isSafeInteger(editVersion) || editVersion < 0)) return failure('editVersion')
      if (isEdited !== undefined && typeof isEdited !== 'boolean') return failure('isEdited')
      if (isRecalled !== undefined && typeof isRecalled !== 'boolean') return failure('isRecalled')
      if (isDeleted !== undefined && typeof isDeleted !== 'boolean') return failure('isDeleted')
      const unavailableEvidence = () => ({content: shape(content), richText: shape(own(content, 'richText')),
        ...(richProjection?.unknownElementStructures?.length ? {unknown_element_structures: richProjection.unknownElementStructures} : {})})
      let text = '', media = [], contentUnavailableReason = null, richProjection
      if (isRecalled || isDeleted) {
        contentUnavailableReason = isRecalled ? 'recalled' : 'deleted'
      } else if (type === 4) {
        const projection = renderRichText(own(content, 'richText'))
        if (projection.complete) {
          text = projection.text
          if (projection.inlineMediaCount) media = [{kind: 'inline-media', available: false}]
        } else contentUnavailableReason = 'richtext-elements-unrenderable'
        richProjection = projection
      } else if (type === 2) {
        const projection = renderRichText(own(content, 'richText')), title = own(content, 'title')
        if (projection.complete && (title === undefined || title === null || typeof title === 'string')) {
          text = typeof title === 'string' && title ? title + (projection.text ? '\n' : '') + projection.text : projection.text
          if (projection.inlineMediaCount) media = [{kind: 'inline-media', available: false}]
        } else contentUnavailableReason = 'richtext-elements-unrenderable'
        richProjection = projection
      } else {
        const knownMedia = [['imageKey', 'image'], ['fileKey', 'file'], ['audioKey', 'audio'], ['stickerId', 'sticker'], ['mediaKey', 'media']]
          .filter(([key]) => typeof own(content, key) === 'string' && own(content, key))
          .map(([, kind]) => kind)
        media = [{kind: knownMedia.length === 1 ? knownMedia[0] : 'unavailable', available: false}]
        contentUnavailableReason = 'non-text-content-unavailable'
      }
      const editMilliseconds = numeric(editTimeMs)
      const editedAt = isEdited && Number.isFinite(editMilliseconds) && editMilliseconds > 0 && Number.isFinite(new Date(editMilliseconds).getTime()) ? new Date(editMilliseconds).toISOString() : null
      const replyTo = ['rootId', 'parentId', 'threadId'].map(key => own(raw, key)).filter(value => typeof value === 'string' && value)
      return {id, chat_id: chatId, position, sent_at: sentAt.toISOString(), message_type: type, text, media,
        sender: {ref: typeof fromId === 'string' && fromId ? fromId : null, ...(typeof name === 'string' && name ? {display_name: name} : {})},
        ...(editVersion !== undefined && editVersion !== null ? {revision: editVersion} : {}), ...(isEdited ? {edited: true, edited_at: editedAt} : {}),
        ...(isRecalled !== undefined ? {is_recalled: isRecalled} : {}), ...(isDeleted !== undefined ? {is_deleted: isDeleted} : {}),
        ...(richProjection ? {text_projection: richProjection.text_projection, renderedElementCount: richProjection.renderedElementCount, unknownElementCount: richProjection.unknownElementCount, inlineMediaCount: richProjection.inlineMediaCount} : {}),
        ...(contentUnavailableReason ? {content_unavailable: true, content_unavailable_reason: contentUnavailableReason, content_evidence: unavailableEvidence()} : {}),
        ...(replyTo.length ? {reply_to: replyTo} : {})}
    }
    const projected = []
    for (const item of messageItems) {
      const message = own(messages, own(item, 'itemId')), value = project(message)
      if (value.failure) return {...base, reason: 'selected-chat-page-unrecognized', diagnostics: {originMatched: true, identityBeforeMatched: true, identityAfterMatched: true, selectionStable: true, messageModel: model, response, projectionFailure: value.failure}}
      projected.push(value)
    }
    const seen = new Set()
    if (projected.some(message => seen.has(message.id) || (seen.add(message.id), false)) || projected.some(message => message.position > history.position) ||
      (history.mode === 'initial' && !projected.some(message => message.position === selected.position))) {
      return {...base, reason: 'selected-chat-page-unrecognized', diagnostics: {originMatched: true, identityBeforeMatched: true, identityAfterMatched: true, selectionStable: true, messageModel: model, response}}
    }
    return {...base, reason: 'selected-chat-page-observed', conversationId: selected.chatId, selectedPosition: selected.position, requestedPosition: history.position,
      messages: projected, invalidPositions, missingPositions, dataComplete, diagnostics: {originMatched: true, identityBeforeMatched: true, identityAfterMatched: true, selectionStable: true, messageModel: model, response}}
  }
  return {...base, reason: schemaRecognized ? 'fixed-message-probe-observed' : 'fixed-message-probe-schema-unrecognized', diagnostics: {
    originMatched: true, identityBeforeMatched: true, identityAfterMatched: true,
    request: {scene: 'FIRST_SCREEN', count: 1, strategy: 'SYNC_SERVER_DATA', redundancyCount: 0, subscribChatEvent: false, needResponse: true, selectedAnchorVerified: true},
    messageModel: model, response,
  }}
}
