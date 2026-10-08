import assert from 'node:assert/strict'
import { mkdtemp, writeFile, rm, readFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { PassThrough } from 'node:stream'
import test from 'node:test'
import { createHash } from 'node:crypto'
import { pathToFileURL } from 'node:url'
import { createOpsailChatToolDefinitions, extensionBuildId, runChat } from '../../src/chat/tools.js'
import { startOpsailHostMcpServer as startOpsailReadMcpServer } from '../../src/mcp-server.js'

async function script(t, code) {
  const dir = await mkdtemp(join(tmpdir(), 'opsail-chat-test-'))
  t.after(() => rm(dir, { recursive: true, force: true }))
  const entry = join(dir, 'reader.py')
  await writeFile(entry, code)
  return { dir, entry }
}

test('extension identity reads the retained-chat installation record only', async t => {
  const { dir } = await script(t, '')
  const config = join(dir, 'config.json')
  await writeFile(config, JSON.stringify({ schemaVersion: 1, buildId: 'a'.repeat(64) }))
  assert.equal(extensionBuildId(config), 'a'.repeat(64))
  assert.equal(extensionBuildId(join(dir, 'missing.json')), null)
})

test('three shared tools require explicit modes and bounded exact reads', async () => {
  assert.deepEqual(createOpsailChatToolDefinitions().map(t => t.name), ['opsail_chat_status', 'opsail_chat_catalog', 'opsail_chat_read'])
  for (const args of [
    { provider: 'wechat', mode: 'snapshot' },
    { provider: 'teams', conversationId: 'one' },
    { provider: 'feishu', mode: 'snapshot', conversationName: 'one', limit: 201 },
    { provider: 'wechat', mode: 'snapshot', conversationId: 'one', cookie: 'secret' },
    { provider: 'wechat', mode: 'snapshot', conversationId: 'one', limit: 1.5 },
  ]) assert.equal((await runChat('read', args)).error.code, 'invalid-request')
})

test('malformed output, source mismatch, and catalog body leakage fail closed', async t => {
  for (const value of [
    'not JSON',
    JSON.stringify({ schemaVersion: 1, operation: 'catalog', provider: 'teams', mode: 'snapshot', exitCode: 0, data: { entries: [] } }),
    JSON.stringify({ schemaVersion: 1, operation: 'catalog', provider: 'wechat', mode: 'snapshot', exitCode: 0, data: { entries: [{ conversationId: 'one', conversationName: 'One', preview: 'private body' }] } }),
  ]) {
    const { entry } = await script(t, `print(${JSON.stringify(value)})\n`)
    const result = await runChat('catalog', { provider: 'wechat', mode: 'snapshot' }, { entry })
    assert.equal(result.error.code, 'invalid-provider-output')
    assert.doesNotMatch(JSON.stringify(result), /private body/)
  }
})

test('raw stderr is never returned, including unavailable runtime', async t => {
  const { entry } = await script(t, 'import sys\nprint("TOKEN=private-secret /private/source", file=sys.stderr)\nraise SystemExit(1)\n')
  const result = await runChat('status', {}, { entry })
  assert.equal(result.exitCode, 2)
  assert.doesNotMatch(JSON.stringify(result), /private-secret|\/private\/source/)
  assert.equal((await runChat('status', {}, { python: '/missing/chat/python' })).error.code, 'provider-unavailable')
})

test('read boundary rejects a different conversation and extra private fields', async t => {
  const make = identity => {
    const subject = 'chat://wechat/conversation/' + createHash('sha256').update(identity).digest('hex').slice(0, 24)
    return { schemaVersion: 1, operation: 'read', provider: 'wechat', mode: 'snapshot', exitCode: 0, data: {
      schema: 1, provider_id: 'wechat', subject_ref: subject, item_count: 0,
      content: { conversation_name: identity, messages: [], capture: {
        provider_id: 'wechat', subject_ref: subject, messages: [], media: [], source_window: {},
        extensions: { source: { mode: 'snapshot', identity_kind: 'wechat-local-conversation-id', conversation_id: identity } },
      } },
    } }
  }
  const wrong = make('other')
  const leak = make('requested'); leak.data.privateBinding = '/private/secret-binding'
  for (const result of [wrong, leak]) {
    const { entry } = await script(t, `print(${JSON.stringify(JSON.stringify(result))})\n`)
    const value = await runChat('read', { provider: 'wechat', mode: 'snapshot', conversationId: 'requested' }, { entry })
    assert.equal(value.error.code, 'invalid-provider-output')
    assert.doesNotMatch(JSON.stringify(value), /secret-binding/)
  }
})

test('unqualified live-api is explicit and never invokes DOM or snapshot', async () => {
  const result = await runChat('read', { provider: 'teams', mode: 'live-api', conversationId: 'one' })
  assert.equal(result.error.code, 'api-protocol-unverified')
  assert.equal(result.exitCode, 2)
  assert.equal(result.implementation.adapterSha256.length, 64)
})

test('long-lived Node process refuses Python implementation drift', async t => {
  const { dir, entry } = await script(t, 'raise SystemExit(99)\n')
  await writeFile(join(dir, 'package.json'), '{"type":"module"}')
  await writeFile(join(dir, 'tools.js'), await readFile(new URL('../../src/chat/tools.js', import.meta.url)))
  await writeFile(join(dir, '..', 'layout.js'), await readFile(new URL('../../src/layout.js', import.meta.url)))
  let loaded
  loaded = await import(pathToFileURL(join(dir, 'tools.js')).href)

  await writeFile(entry, 'raise SystemExit(100)\n')
  const result = await loaded.runChat('status', {}, { entry })
  assert.equal(result.error.code, 'implementation-changed')
})

test('cancellation and deadline stop the actual provider process group', async t => {
  const { dir, entry } = await script(t, 'import subprocess,time,pathlib,sys\nchild=subprocess.Popen([sys.executable,"-c","import time;time.sleep(60)"])\npathlib.Path(__file__).with_suffix(".pid").write_text(str(child.pid))\ntime.sleep(60)\n')
  const controller = new AbortController()
  const pending = runChat('status', {}, { entry, signal: controller.signal })
  let descendant
  for (let i = 0; i < 100; i++) {
    try { descendant = Number(await readFile(join(dir, 'reader.pid'), 'utf8')); break } catch {}
    await new Promise(resolve => setTimeout(resolve, 20))
  }
  assert.ok(descendant, 'real descendant started')
  controller.abort()
  assert.equal((await pending).error.code, 'cancelled')
  await new Promise(resolve => setTimeout(resolve, 400))
  assert.throws(() => process.kill(descendant, 0), /ESRCH/)
  assert.equal((await runChat('status', {}, { entry, timeoutMs: 50 })).error.code, 'provider-timeout')
})

test('fresh MCP projects the canonical chat tools and error semantics', async () => {
  const input = new PassThrough(), output = new PassThrough()
  const definitions = createOpsailChatToolDefinitions()
  const received = []
  let buffer = ''
  output.on('data', chunk => {
    buffer += chunk.toString()
    while (buffer.includes('\n')) {
      const at = buffer.indexOf('\n')
      received.push(JSON.parse(buffer.slice(0, at)))
      buffer = buffer.slice(at + 1)
    }
  })
  const server = startOpsailReadMcpServer({ input, output, definitions })
  try {
    input.write(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/list' }) + '\n')
    input.write(JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'opsail_chat_read', arguments: { provider: 'teams', mode: 'live-api', conversationId: 'one' } } }) + '\n')
    for (let i = 0; received.length < 2 && i < 100; i++) await new Promise(resolve => setTimeout(resolve, 20))
    assert.deepEqual(received[0].result.tools.map(t => t.name), definitions.map(t => t.name))
    assert.deepEqual(received[0].result.tools[2].inputSchema, definitions[2].parameters)
    assert.equal(received[1].result.isError, true)
    assert.equal(received[1].result.structuredContent.error.code, 'api-protocol-unverified')
  } finally { server.close(); input.end() }
})

import { stat, utimes } from 'node:fs/promises'

test('an mtime-only touch with identical content is not implementation drift', async t => {
  const { dir, entry } = await script(t, 'import json\nprint(json.dumps({"schemaVersion":1,"operation":"status","provider":None,"mode":None,"exitCode":0,"data":{"providers":[{"provider":"wechat","modes":{}},{"provider":"feishu","modes":{}},{"provider":"teams","modes":{}}]}}))\n')
  await writeFile(join(dir, 'package.json'), '{"type":"module"}')
  await writeFile(join(dir, 'tools.js'), await readFile(new URL('../../src/chat/tools.js', import.meta.url)))
  await writeFile(join(dir, '..', 'layout.js'), await readFile(new URL('../../src/layout.js', import.meta.url)))
  let loaded
  loaded = await import(pathToFileURL(join(dir, 'tools.js')).href + '?touch')

  const info = await stat(entry)
  await utimes(entry, new Date(info.atimeMs + 5000), new Date(info.mtimeMs + 5000))
  const touched = await loaded.runChat('status', {}, { entry })
  assert.equal(touched.exitCode, 0, JSON.stringify(touched))
  await writeFile(entry, 'raise SystemExit(100)\n')
  assert.equal((await loaded.runChat('status', {}, { entry })).error.code, 'implementation-changed')
})

test('live Feishu sender and date provenance cross the Python and Node output boundaries', async t => {
  const source = new URL('../../src/', import.meta.url).pathname
  const {entry} = await script(t, `import sys,json
sys.path.insert(0, ${JSON.stringify(source)})
from chat.live_sources.capture import build_visible_observation
rows = [{"id":"m1", "sender":"Ada", "sender_ref":"user-1", "time":"2026-09-24T09:55:00", "raw_time":"09:55", "date_status":"known", "time_source":"date-separator", "time_zone":"unknown", "is_self":False, "text":"fixture", "media":[]}, {"id":"m2", "sender":"", "sender_ref":None, "time":"10:16", "raw_time":"10:16", "date_status":"unknown", "time_source":"raw", "time_zone":"unknown", "is_self":None, "text":"fixture", "media":[]}]
data = build_visible_observation("feishu", "c1", "Fixture", "https://fixture.feishu.cn/next/messenger/", rows, 2, source_mode="chrome-extension")
print(json.dumps({"schemaVersion":1,"operation":"read","provider":"feishu","mode":"live-dom","exitCode":0,"data":data}))
`)
  const result = await runChat('read', {provider: 'feishu', mode: 'live-dom', conversationId: 'c1', limit: 2}, {entry})
  assert.equal(result.exitCode, 0, JSON.stringify(result.error))
  const [known, unknown] = result.data.content.capture.messages
  assert.deepEqual(known.sender, {display_name: 'Ada', ref: 'user-1', is_self: false})
  assert.equal(known.sent_at, '2026-09-24T09:55:00'); assert.equal(known.extensions.time_zone, 'unknown')
  assert.equal(unknown.sent_at, '10:16'); assert.equal(unknown.extensions.date_status, 'unknown'); assert.equal(unknown.sender.is_self, null)
})
