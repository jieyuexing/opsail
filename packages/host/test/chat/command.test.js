import assert from 'node:assert/strict'
import test from 'node:test'
import { runCommand } from '../../src/chat/command.js'

const ready = { configured: true, available: true, diagnostic: 'snapshot-root-available', validation: 'not-validated', freshness: { coverage: { complete: true } } }
test('quiet successful checks exercise the reader and emit nothing', async () => {
  const calls = []
  const result = await runCommand(['check', '--provider', 'wechat', '--mode', 'snapshot', '--quiet'], async (operation, args) => {
    calls.push([operation, args])
    return operation === 'status' ? { exitCode: 0, data: { providers: [{ modes: { snapshot: ready } }] } } : { exitCode: 0, data: { entries: [] } }
  })
  assert.deepEqual(result, { code: 0, stdout: '', stderr: '' })
  assert.deepEqual(calls.map(row => row[0]), ['status', 'catalog'])
  assert.equal(calls[1][1].limit, 1)
})
test('quiet incomplete sources remain nonzero and diagnostic', async () => {
  const result = await runCommand(['check', '--provider', 'wechat', '--mode', 'snapshot', '--quiet'], async () => ({ exitCode: 0, data: { providers: [{ modes: { snapshot: { ...ready, diagnostic: 'snapshot-coverage-incomplete', freshness: { coverage: { complete: false } } } } }] } }))
  assert.equal(result.code, 2); assert.equal(result.stdout, '')
  assert.equal(JSON.parse(result.stderr).error.code, 'snapshot-coverage-incomplete')
})
test('exact read arguments reach the shared boundary unchanged', async () => {
  let captured
  const result = await runCommand(['read', '--provider', 'teams', '--mode', 'snapshot', '--conversation-id', 'fixture', '--limit', '20'], async (operation, args) => { captured = [operation, args]; return { exitCode: 0, data: {} } })
  assert.equal(result.code, 0)
  assert.deepEqual(captured, ['read', { provider: 'teams', mode: 'snapshot', conversationId: 'fixture', limit: 20 }])
})
test('CLI rejects unsupported flags and ambiguous silent reads before dispatch', async () => {
  for (const args of [['read', '--quiet'], ['read', '--limit', '1.5'], ['check', '--provider', 'teams'], ['status', '--token', 'private']]) {
    const result = await runCommand(args, async () => assert.fail('must not dispatch'))
    assert.equal(result.code, 2); assert.doesNotMatch(result.stderr, /private/)
  }
})
