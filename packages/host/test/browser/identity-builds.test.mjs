import assert from 'node:assert/strict'
import test from 'node:test'
import vm from 'node:vm'
import {webcrypto} from 'node:crypto'
import {inspectPage} from '../../src/browser/extension/providers/dom.js'
import {boundedDiagnostics} from '../../src/browser/extension/diagnostics.js'

function page({names = ['index.745e4057.js'], pathname = '/next/messenger/', mismatch = false, domIdentity = false, provider = 'feishu', host = 'sf1-scmcdn-cn.feishucdn.com'} = {}) {
  let calls = 0
  const identityNode = {getAttribute: key => key === 'data-tenant-id' ? 'tenant-private' : 'user-private', isConnected: true}
  const context = {URL, TextEncoder, crypto: webcrypto, setTimeout, clearTimeout,
    location: {origin: 'https://x.feishu.cn', pathname, href: 'https://x.feishu.cn' + pathname},
    document: {querySelector: selector => domIdentity && selector.startsWith('[data-user-id]') ? identityNode : null, querySelectorAll: () => []},
    performance: {getEntriesByType: () => names.map(name => ({name: `https://${host}/assets/static/js/${name}?PRIVATE_QUERY`, initiatorType: 'script'}))},
    window: {userId: mismatch ? 'wrong-private' : 'user-private', configurationAdapter: {passport: {userId: 'user-private', getCurUserInfo: async () => {calls++; return {user: {id: 'user-private', tenant: {id: 'tenant-private'}}}}}}},
  }
  const inspect = vm.runInNewContext(`(${inspectPage.toString()})`, context)
  return {run: builds => inspect(provider, 'status', {}, context.location.origin, {}, builds), calls: () => calls}
}

test('unqualified build returns only bounded bundle metadata before calling the session method', async () => {
  const f = page({names: ['index.abcdef01.js'], domIdentity: true})
  const value = await f.run()
  assert.equal(value.error.code, 'unqualified-build'); assert.equal(f.calls(), 0)
  assert.deepEqual(Array.from(value.diagnostics.bundleNames), ['index.abcdef01.js'])
  assert.doesNotMatch(JSON.stringify(value), /PRIVATE|https:|user-private|tenant-private/)
})

test('default and allow-listed builds require the same behavioral contract, including DOM metadata pages', async () => {
  for (const name of ['index.745e4057.js', 'index.abcdef01.js']) {
    const f = page({names: [name]})
    const result = await f.run([name])
    assert.equal(result.error, undefined); assert.equal(f.calls(), 1)
    assert.equal(result.diagnostics.behavioralContractPassed, true)
    assert.equal(result.identity.accountId, 'user-private')
    const bad = await page({names: [name], mismatch: true, domIdentity: true}).run([name])
    assert.equal(bad.error.code, 'identity-unverified')
    assert.equal(bad.diagnostics.behavioralContractPassed, false)
    assert.doesNotMatch(JSON.stringify(bad), /user-private|tenant-private/)
  }
  assert.equal((await page().run()).error, undefined)
})

test('unknown mixed builds, another CDN, no bundle and excessive bundles fail closed', async () => {
  for (const options of [{names: []}, {host: 'evil.test'}, {names: ['index.745e4057.js', 'index.abcdef01.js']}, {names: Array.from({length: 17}, (_, i) => `index.${i.toString(16).padStart(8, '0')}.js`)}]) {
    const f = page(options); const value = await f.run()
    assert.equal(value.error.code, 'unqualified-build'); assert.equal(f.calls(), 0)
    assert.ok(value.diagnostics.bundleNames.length <= 16)
  }
})

test('degraded route has its own code before identity or build inspection; Teams stays unchanged', async () => {
  for (const pathname of ['/next/messenger/degraded', '/next/messenger/degraded/', '/messenger/degraded']) {
    const f = page({pathname}); assert.equal((await f.run()).error.code, 'provider-degraded'); assert.equal(f.calls(), 0)
  }
  const teams = page({provider: 'teams', domIdentity: true, names: []})
  assert.equal((await teams.run()).identity.accountId, 'user-private'); assert.equal(teams.calls(), 0)
})

test('diagnostics projection drops arbitrary names, raw text, IDs, paths and oversized metadata', () => {
  const result = boundedDiagnostics({bundleNames: ['index.12345678.js', 'https://evil.test/index.12345678.js?PRIVATE', '../index.12345678.js', 'index.12345678.js'], accountId: 'PRIVATE', token: 'PRIVATE', conflictingTabCount: 1e9, buildQualified: true,
    bootstrap: {globals: [{name: 'PRIVATE', fields: [{key: 'PRIVATE', type: 'string', value: 'PRIVATE'}]}], scriptResources: ['https://PRIVATE/']}})
  assert.deepEqual(result, {buildQualified: true, bundleNames: ['index.12345678.js'], bootstrap: {fieldTypes: {string: 1}, globalsCount: 1}})
  assert.deepEqual(boundedDiagnostics(result), result)
})
