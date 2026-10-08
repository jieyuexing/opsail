// Fixed, read-only DOM readers. The exported page reader is self-contained so Electron
// serializes it into the already loaded, provider-checked document; Node never reads DOM.
export const SOURCE_SITES = Object.freeze({
  feishu: [/^(?:https:\/\/)?(?:[\w-]+\.)?feishu\.cn\/?$/],
  teams: [/^https:\/\/teams\.(?:microsoft\.com|microsoftonline\.cn|cloud)\/?$/],
  'outlook-mail': [/^https:\/\/outlook\.office(?:365)?\.com\/?$/],
  'outlook-calendar': [/^https:\/\/outlook\.office(?:365)?\.com\/?$/],
  jira: [/^https:\/\/[\w-]+\.atlassian\.net\/?$/],
})
export function allowedOrigin(provider, origin) { return SOURCE_SITES[provider]?.some(pattern => pattern.test(origin)) === true }
export function normalizeRows(provider, operation, rows, { limit = 50 } = {}) {
  const seen = new Set(), result = []
  for (const row of rows ?? []) {
    const id = String(row?.id ?? '').trim()
    if (!id || seen.has(id)) continue
    seen.add(id)
    result.push({ id, title: String(row?.title ?? '').trim() || null, body: typeof row?.body === 'string' ? row.body : null, bodyComplete: row?.bodyComplete === true, revision: row?.revision?.id && row?.revision?.hash ? row.revision : null, sourceUrl: typeof row?.sourceUrl === 'string' ? row.sourceUrl : null })
    if (result.length >= limit) break
  }
  return result
}
// Do not add imports, closures, or caller supplied selectors: this function is serialized
// into a WebContents executeJavaScript call and runs in the authenticated page.
export function inspectUniverseSourcePage(provider, operation, expectedOrigin, limit = 50) {
  const maxBody = 256 * 1024
  const allowed = {
    feishu: /^https:\/\/(?:[\w-]+\.)?feishu\.cn\/?$/,
    teams: /^https:\/\/teams\.(?:microsoft\.com|microsoftonline\.cn|cloud)\/?$/,
    'outlook-mail': /^https:\/\/outlook\.office(?:365)?\.com\/?$/,
    'outlook-calendar': /^https:\/\/outlook\.office(?:365)?\.com\/?$/,
    jira: /^https:\/\/[\w-]+\.atlassian\.net\/?$/,
  }
  const origin = location.origin
  if (!allowed[provider]?.test(origin) || origin !== expectedOrigin) return { error: 'origin-denied', origin }
  const selectors = operation === 'directory'
    ? { feishu: '[data-conversation-id]', teams: '[data-conversation-id]', 'outlook-mail': '[data-conversation-id]', 'outlook-calendar': '[data-calendar-id]', jira: '[data-issue-key]' }
    : { feishu: '[data-message-id]', teams: '[data-message-id]', 'outlook-mail': '[data-message-id]', 'outlook-calendar': '[data-event-id]', jira: '[data-issue-key]' }
  const queryOne = typeof document.querySelector === 'function' ? document.querySelector.bind(document) : () => null
  const account = queryOne('[data-account-id],[data-user-id],[data-uid]')?.getAttribute('data-account-id')
    || queryOne('[data-user-id]')?.getAttribute('data-user-id')
    || queryOne('[data-uid]')?.getAttribute('data-uid') || ''
  const rows = [...document.querySelectorAll(selectors[provider] || '')].slice(0, Math.min(Math.max(Number(limit) || 50, 1), 100)).map(node => {
    const body = operation === 'collect' ? String(node.querySelector('[data-body]')?.textContent || '') : ''
    const truncated = body.length > maxBody
    return {
      id: node.getAttribute('data-message-id') || node.getAttribute('data-conversation-id') || node.getAttribute('data-calendar-id') || node.getAttribute('data-event-id') || node.getAttribute('data-issue-key') || '',
      title: node.getAttribute('aria-label') || node.querySelector('[data-title]')?.textContent || '',
      body: truncated ? body.slice(0, maxBody) : body,
      bodyComplete: false,
      sourceUrl: location.href,
      truncated,
    }
  }).filter(row => row.id)
  return { origin, url: location.href, account, rows, nextCursor: null, completeness: { directory: 'unknown', body: rows.some(row => row.truncated) ? 'partial' : 'partial', reasons: ['rendered-dom-window', ...(rows.some(row => row.truncated) ? ['body-limit'] : [])] } }
}
export function inspectUniverseSource(provider, operation, expectedOrigin, { limit = 50 } = {}) {
  return inspectUniverseSourcePage(provider, operation, expectedOrigin, limit)
}
