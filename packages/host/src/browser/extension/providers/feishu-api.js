// Self-contained by design: Chrome serializes scripting.executeScript `func`
// values and does not retain imported module bindings in the target page.
export async function inspectFeishuFeed(expectedOrigin, expectedIdentity) {
  const descriptor = '2480|feed.v1.GetFeedCardsV4Request|feed.v1.GetFeedCardsV4Response|1|GET_FEED_CARDS_V4'
  const initialCursor = {id: '0', rankTime: '9223372036854775807'}
  const sensitive = /token|cookie|secret|password|credential|authorization|auth/i
  const safeKey = /^[A-Za-z_$][A-Za-z0-9_$]{0,79}$/
  const own = (value, key) => {
    if (!value || (typeof value !== 'object' && typeof value !== 'function')) return undefined
    const property = Object.getOwnPropertyDescriptor(value, key)
    return property && 'value' in property ? property.value : undefined
  }
  const typeOf = value => value === null ? 'null' : Array.isArray(value) ? 'array' : typeof value
  const safeKeys = value => !value || typeof value !== 'object' || Array.isArray(value) ? [] : Object.keys(value).filter(key => safeKey.test(key) && !sensitive.test(key)).slice(0, 32)
  const shape = value => ({type: typeOf(value), keys: safeKeys(value).map(key => ({key, type: typeOf(own(value, key))}))})
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
    const windowUserId = own(window, 'userId'), passportUserId = own(passport, 'userId')
    if (typeof userId !== 'string' || !userId || typeof tenantId !== 'string' || !tenantId) return false
    if (windowUserId !== userId || passportUserId !== userId) return false
    if (!expectedIdentity || typeof expectedIdentity.accountHash !== 'string' || typeof expectedIdentity.tenantHash !== 'string') return false
    const [accountHash, tenantHash] = await Promise.all([hash(userId), hash(tenantId)])
    return accountHash === expectedIdentity.accountHash && tenantHash === expectedIdentity.tenantHash &&
      own(window, 'userId') === userId && own(own(own(window, 'configurationAdapter'), 'passport'), 'userId') === userId
  }
  let before
  try { before = await timeout(getCurUserInfo.call(passport)) } catch { return {...base, reason: 'identity-read-failed', diagnostics: {originMatched: true}} }
  if (!await identityMatches(before)) return {...base, reason: 'identity-unverified', diagnostics: {originMatched: true, identityBeforeMatched: false}}
  if (origin() !== expectedOrigin) return {...base, reason: 'page-changed', diagnostics: {originMatched: false, identityBeforeMatched: true}}
  let wrapper
  try {
    // Intentionally exactly two arguments: no caller options and no invented trace fields.
    wrapper = await timeout(callSdkApi.call(transport, descriptor, {feedCursor: initialCursor, filter: 1, count: 1}))
  } catch (error) {
    return {...base, reason: error?.message === 'timeout' ? 'fixed-feed-probe-timeout' : 'fixed-feed-probe-failed', diagnostics: {originMatched: true, identityBeforeMatched: true}}
  }
  let after
  try { after = await timeout(getCurUserInfo.call(passport)) } catch { return {...base, reason: 'identity-read-failed', diagnostics: {originMatched: true, identityBeforeMatched: true}} }
  const identityAfterMatched = await identityMatches(after)
  const originAfterMatched = origin() === expectedOrigin
  if (!originAfterMatched || !identityAfterMatched) return {...base, reason: originAfterMatched ? 'identity-changed' : 'page-changed', diagnostics: {originMatched: originAfterMatched, identityBeforeMatched: true, identityAfterMatched}}
  const data = own(wrapper, 'data')
  const previews = own(data, 'previews'), cursor = own(data, 'feedCursor')
  const cursorId = own(cursor, 'id'), cursorRankTime = own(cursor, 'rankTime')
  const cursorPresent = !!cursor && typeof cursor === 'object' && !Array.isArray(cursor)
  const response = {
    wrapper: shape(wrapper), data: data && typeof data === 'object' && !Array.isArray(data) ? shape(data) : {present: false},
    previews: {present: previews !== undefined, type: typeOf(previews), count: Array.isArray(previews) ? previews.length : null},
    feedCursor: {present: cursorPresent, idType: typeOf(cursorId), rankTimeType: typeOf(cursorRankTime), terminalCandidate: cursorId === '0' && cursorRankTime === '0'},
    contextIdPresent: typeof own(wrapper, 'contextId') === 'string' && own(wrapper, 'contextId').length > 0,
    sdkCostPresent: typeof own(wrapper, 'sdkCostTime') === 'number',
  }
  const schemaRecognized = Array.isArray(previews) && cursorPresent
  return {...base, reason: schemaRecognized ? 'fixed-feed-probe-observed' : 'fixed-feed-probe-schema-unrecognized', diagnostics: {originMatched: true, identityBeforeMatched: true, identityAfterMatched: true, request: {filter: 'INBOX', count: 1, initialCursor: 'zero-max-rank'}, response}}
}
