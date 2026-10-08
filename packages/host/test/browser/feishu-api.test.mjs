import test from 'node:test'
import assert from 'node:assert/strict'
import {inspectFeishuFeed} from '../../src/browser/extension/providers/feishu-api.js'

const source = id => `feishu\u0000https://tenant.feishu.cn\u0000${id}`
const digest = async value => [...new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(value)))].map(byte => byte.toString(16).padStart(2, '0')).join('')
const identity = {user: {id: 'user-id', tenant: {id: 'tenant-id'}}}
const expected = async () => ({accountHash: await digest(source('user-id')), tenantHash: await digest(source('tenant-id'))})
const withPage = async (adapter, fn, origin = 'https://tenant.feishu.cn') => {
  const previousWindow = globalThis.window, previousLocation = globalThis.location
  globalThis.window = {configurationAdapter: adapter, userId: 'user-id'}
  globalThis.location = {origin}
  try { return await fn() } finally { globalThis.window = previousWindow; globalThis.location = previousLocation }
}

test('serialized probe pins descriptor and only sends the minimal fixed args', async () => {
  const calls = []
  const response = {data: {previews: [{feedId: 'must-not-leak'}], feedCursor: {id: 'next', rankTime: '8'}, authToken: 'must-not-leak'}, contextId: 'trace', sdkCostTime: 3}
  const adapter = {passport: {userId: 'user-id', getCurUserInfo: async () => identity}, transport: {callSdkApi: async (...args) => { calls.push(args); return response }}}
  const serializedEntry = (0, eval)(`(${inspectFeishuFeed.toString()})`)
  const result = await withPage(adapter, async () => serializedEntry('https://tenant.feishu.cn', await expected()))
  assert.equal(result.qualified, false)
  assert.equal(result.reason, 'fixed-feed-probe-observed')
  assert.deepEqual(calls, [['2480|feed.v1.GetFeedCardsV4Request|feed.v1.GetFeedCardsV4Response|1|GET_FEED_CARDS_V4', {feedCursor: {id: '0', rankTime: '9223372036854775807'}, filter: 1, count: 1}]])
  assert.equal(result.diagnostics.response.previews.count, 1)
  assert.equal(JSON.stringify(result).includes('must-not-leak'), false)
  assert.equal(result.diagnostics.response.data.keys.some(item => item.key === 'authToken'), false)
})

test('origin, global identity, or hash mismatch prevents the read', async () => {
  let calls = 0
  const adapter = {passport: {userId: 'user-id', getCurUserInfo: async () => identity}, transport: {callSdkApi: async () => { calls++; return {data: {previews: [], feedCursor: {id: '0', rankTime: '0'}}} }}}
  const e = await expected()
  const wrongOrigin = await withPage(adapter, () => inspectFeishuFeed('https://tenant.feishu.cn', e), 'https://other.feishu.cn')
  const wrongIdentity = await withPage(adapter, () => inspectFeishuFeed('https://tenant.feishu.cn', {...e, accountHash: '0'.repeat(64)}))
  assert.equal(wrongOrigin.reason, 'page-changed')
  assert.equal(wrongIdentity.reason, 'identity-unverified')
  assert.equal(calls, 0)
})

test('identity change after the fixed read discards the response', async () => {
  let reads = 0
  const adapter = {passport: {userId: 'user-id', getCurUserInfo: async () => ++reads === 1 ? identity : {user: {id: 'changed', tenant: {id: 'tenant-id'}}}}, transport: {callSdkApi: async () => ({data: {previews: [], feedCursor: {id: '0', rankTime: '0'}}})}}
  const result = await withPage(adapter, async () => inspectFeishuFeed('https://tenant.feishu.cn', await expected()))
  assert.equal(result.reason, 'identity-changed')
  assert.equal('response' in result.diagnostics, false)
})

test('response accessors and sensitive values are not evaluated or emitted', async () => {
  let touched = false
  const data = {previews: [], feedCursor: {id: '0', rankTime: '0'}}
  Object.defineProperty(data, 'token', {enumerable: true, get() { touched = true; throw new Error('do not read') }})
  const adapter = {passport: {userId: 'user-id', getCurUserInfo: async () => identity}, transport: {callSdkApi: async () => ({data})}}
  const result = await withPage(adapter, async () => inspectFeishuFeed('https://tenant.feishu.cn', await expected()))
  assert.equal(result.reason, 'fixed-feed-probe-observed')
  assert.equal(touched, false)
  assert.equal(result.diagnostics.response.feedCursor.terminalCandidate, true)
})
