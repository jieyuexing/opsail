import {BridgeError} from '../protocol.js'

// Request-scoped wait, not a background alarm. Each sample rechecks ownership,
// identity and the exact conversation in inspectPage. No failed sample escapes.
export async function settledPane(read, {deadline, now = Date.now, sleep = ms => new Promise(resolve => setTimeout(resolve, ms)), intervalMs = 250} = {}) {
  let previous = null
  let lastError = new BridgeError('message-scope-unverified')
  while (now() < deadline) {
    let timer
    try {
      const value = await Promise.race([read(), new Promise((_, reject) => {
        timer = setTimeout(() => reject(new BridgeError('message-scope-unverified')), Math.max(1, deadline - now()))
      })])
      const signature = JSON.stringify([value.conversationId, value.messageIds ?? value.messages])
      if (previous === signature && now() < deadline) return value
      previous = signature
    } catch (error) {
      if (!['message-scope-unverified', 'conversation-not-selected'].includes(error.code)) throw error
      previous = null
      lastError = error
    } finally { clearTimeout(timer) }
    const remaining = deadline - now()
    if (remaining > 0) await sleep(Math.min(intervalMs, remaining))
  }
  throw lastError
}
