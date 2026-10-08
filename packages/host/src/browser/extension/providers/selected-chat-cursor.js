import {assert} from '../protocol.js'

// Local continuation evidence only; this is not a server cursor or a claim of
// provider-wide history qualification. The caller persists it in session storage.
export function continuation(page, context, now = Date.now()) {
  assert(page.reason === 'selected-chat-page-observed' && page.conversationId === context.conversationId, 'invalid-page-output')
  assert(Array.isArray(page.messages) && page.messages.length > 0, 'empty-message-page')
  const positions = page.messages.map(message => message.position)
  assert(positions.every(position => Number.isSafeInteger(position) && position > 0), 'invalid-page-output')
  assert(Number.isSafeInteger(page.selectedPosition) && page.selectedPosition >= Math.max(...positions), 'invalid-page-output')
  const minimum = Math.min(...positions)
  return {...context, priorPosition: Math.min(minimum + 1, page.selectedPosition), issuedAt: now}
}

export function resumeContinuation(cursor, context, now = Date.now()) {
  assert(cursor && typeof cursor === 'object', 'invalid-cursor')
  for (const key of ['conversationId', 'accountHash', 'tenantHash', 'documentId', 'buildId']) assert(cursor[key] === context[key], 'cursor-context-changed')
  assert(Number.isSafeInteger(cursor.priorPosition) && cursor.priorPosition > 0 && Number.isFinite(cursor.issuedAt) && now >= cursor.issuedAt && now - cursor.issuedAt < 30 * 60_000, 'cursor-expired')
  return cursor.priorPosition
}
