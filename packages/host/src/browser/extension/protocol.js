import {FEISHU_BUILD, MAX_IDENTITY_BUILDS} from './identity-builds.js'
import {boundedDiagnostics, identityNextStep} from './diagnostics.js'
export const PROTOCOL_VERSION = 1
export const MAX_FRAME_BYTES = 512 * 1024
export const OPERATIONS = new Set(['ping', 'diagnose', 'verifyBinding', 'verifyBuild', 'status', 'configure', 'prepare', 'select', 'catalog', 'read', 'qualify', 'catalogPage', 'messagesPage', 'scrollBack', 'pause'])
export class BridgeError extends Error {
  constructor(code, message = code, diagnostics) { super(message); this.code = code; if (diagnostics) this.diagnostics = boundedDiagnostics(diagnostics) }
}
export function assert(condition, code) { if (!condition) throw new BridgeError(code) }
export function validateRequest(value) {
  assert(value && typeof value === 'object' && !Array.isArray(value), 'invalid-request')
  assert(Object.keys(value).every(k => ['type', 'protocolVersion', 'requestId', 'operation', 'provider', 'args', 'binding'].includes(k)), 'invalid-request')
  assert(value.type === 'request' && value.protocolVersion === PROTOCOL_VERSION, 'protocol-mismatch')
  assert(typeof value.requestId === 'string' && /^[\w-]{1,80}$/.test(value.requestId), 'invalid-request')
  assert(OPERATIONS.has(value.operation), 'unsupported-operation')
  assert(value.args && typeof value.args === 'object' && !Array.isArray(value.args), 'invalid-request')
  if (!['ping', 'pause'].includes(value.operation)) assert(['feishu', 'teams'].includes(value.provider), 'invalid-provider')
  const allowed = {
    ping: [], diagnose: [], verifyBinding: [], verifyBuild: ['build'], status: [], configure: [], prepare: [], qualify: [], pause: ['paused'],
    select: ['conversationId', 'conversationName'], catalog: ['limit'],
    read: ['conversationId', 'conversationName', 'conversationUrl', 'limit'],
    catalogPage: ['cursor', 'limit'], messagesPage: ['conversationId', 'cursor', 'limit', 'rangeStart', 'rangeEnd'],
    scrollBack: ['conversationId', 'conversationName', 'direction'],
  }[value.operation]
  assert(Object.keys(value.args).every(k => allowed.includes(k)), 'invalid-request')
  if (value.operation === 'verifyBuild') assert(value.provider === 'feishu' && typeof value.args.build === 'string' && FEISHU_BUILD.test(value.args.build), 'invalid-build')
  if ('limit' in value.args) assert(Number.isInteger(value.args.limit) && value.args.limit >= 1 && value.args.limit <= (['catalog', 'catalogPage'].includes(value.operation) ? 100 : 200), 'invalid-limit')
  for (const [key, val] of Object.entries(value.args)) {
    if (!['limit', 'paused'].includes(key)) assert(typeof val === 'string' && val.length > 0 && val.length <= (key === 'cursor' ? 8192 : 2048) && !/[\x00-\x1f]/.test(val), 'invalid-request')
  }
  if (value.operation === 'pause') assert(typeof value.args.paused === 'boolean', 'invalid-request')
  if ('direction' in value.args) assert(['up', 'bottom'].includes(value.args.direction), 'invalid-request')
  return value
}
export function validateBinding(provider, binding) {
  assert(binding && typeof binding === 'object', 'not-configured')
  const origin = value => {
    let url
    try { url = new URL(value) } catch { throw new BridgeError('invalid-binding') }
    assert(url.protocol === 'https:' && !url.username && !url.password && (!url.port || url.port === '443'), 'invalid-binding')
    assert(provider === 'feishu' ? url.hostname === 'feishu.cn' || url.hostname.endsWith('.feishu.cn') : ['teams.microsoft.com', 'teams.microsoftonline.cn', 'teams.cloud.microsoft'].includes(url.hostname), 'origin-denied')
    return url.origin
  }
  assert(Array.isArray(binding.allowedOrigins) && binding.allowedOrigins.length > 0 && binding.allowedOrigins.length < 10, 'invalid-binding')
  for (const item of binding.allowedOrigins) assert(origin(item) === item, 'invalid-binding')
  assert(binding.allowedOrigins.includes(origin(binding.targetUrl)), 'origin-denied')
  for (const key of ['accountHash', 'tenantHash']) if (binding[key] != null) assert(/^[a-f0-9]{64}$/.test(binding[key]), 'invalid-binding')
  if (binding.identityBuilds !== undefined) assert(provider === 'feishu' && Array.isArray(binding.identityBuilds) && binding.identityBuilds.length <= MAX_IDENTITY_BUILDS && binding.identityBuilds.every(name => typeof name === 'string' && FEISHU_BUILD.test(name)), 'invalid-binding')
  return binding
}
export async function sha256(value) {
  return [...new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(value)))].map(x => x.toString(16).padStart(2, '0')).join('')
}
export function safeError(error) {
  return {code: error instanceof BridgeError ? error.code : 'browser-operation-failed', message: error instanceof BridgeError ? error.code : 'Browser operation could not complete.', ...(error instanceof BridgeError && error.diagnostics ? {diagnostics: boundedDiagnostics(error.diagnostics)} : {}), ...(error instanceof BridgeError && identityNextStep(error.code) ? {nextStep: identityNextStep(error.code)} : {})}
}
