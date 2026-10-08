import test from 'node:test'
import assert from 'node:assert/strict'
import {validateRequest, validateBinding, safeError, sha256} from '../../src/browser/extension/protocol.js'
import {qualification, apiPage} from '../../src/browser/extension/providers/index.js'

const request = (operation, args = {}) => ({type: 'request', protocolVersion: 1, requestId: '123456', operation, provider: 'feishu', args})
test('only fixed operations and arguments cross the browser boundary', () => {
  assert.equal(validateRequest(request('catalog', {limit: 100})).operation, 'catalog')
  for (const value of [request('eval', {script: 'document.cookie'}), request('read', {url: 'https://example.com'}), {...request('status'), token: 'secret'}, request('catalog', {limit: 101}), request('read', {limit: true})]) {
    assert.throws(() => validateRequest(value))
  }
  assert.equal(validateRequest(request('scrollBack', {conversationId: 'c1', direction: 'bottom'})).operation, 'scrollBack')
  for (const args of [{conversationId: 'c1', direction: 'down'}, {conversationId: 'c1', direction: 'up', by: '9999'}, {direction: 7}]) {
    assert.throws(() => validateRequest(request('scrollBack', args)))
  }
})
test('provider binding pins HTTPS site and exact tenant origin', () => {
  const binding = {targetUrl: 'https://tenant.feishu.cn/messenger/', allowedOrigins: ['https://tenant.feishu.cn']}
  assert.equal(validateBinding('feishu', binding), binding)
  for (const bad of [{...binding, targetUrl: 'http://tenant.feishu.cn'}, {...binding, allowedOrigins: ['https://evil.test']}, {...binding, targetUrl: 'https://evil@tenant.feishu.cn'}, {...binding, accountHash: 'name'}]) assert.throws(() => validateBinding('feishu', bad))
  assert.throws(() => validateBinding('teams', binding))
})
test('API qualification cannot be enabled by config or successful DOM reads', async () => {
  const identity = {accountHash: await sha256('account'), tenantHash: await sha256('tenant'), qualified: true}
  for (const provider of ['feishu', 'teams']) {
    assert.equal(qualification(provider, identity).qualified, false)
    await assert.rejects(apiPage(provider, 'catalogPage', {}, {qualified: true}), {code: 'browser-api-unqualified'})
  }
})
test('unexpected errors never expose browser internals', () => {
  assert.deepEqual(safeError(new Error('cookie=secret')), {code: 'browser-operation-failed', message: 'Browser operation could not complete.'})
})
