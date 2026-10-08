import assert from 'node:assert/strict'
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'
import { runChat } from '../../src/chat/tools.js'

test('Teams historical media with capture_status passes the real Node/Python boundary', async t => {
  const root = await mkdtemp(join(tmpdir(), 'opsail-teams-repair-'))
  t.after(() => rm(root, { recursive: true, force: true }))
  const conversation = join(root, 'custody', 'fixture')
  await mkdir(conversation, { recursive: true })
  await writeFile(join(conversation, 'state.json'), JSON.stringify({ chat_url: 'https://teams.microsoftonline.cn/l/chat/19%3Afixture/conversations', chat_name: 'Fixture', total_count: 1, last_export_at: '2026-09-15T00:00:00Z', hit_known_boundary: true, increment_complete: true }))
  await writeFile(join(conversation, 'messages.jsonl'), JSON.stringify({ mid: 'm1', author: 'Synthetic', time_iso: '2026-09-15T00:00:00Z', body: 'Fixture', media: [{ type: 'image', locator: 'fixture://one', alt_text: 'synthetic image', capture_status: 'captured' }] }) + '\n')
  const binding = join(root, 'binding.json')
  await writeFile(binding, JSON.stringify({ schemaVersion: 1, providers: { teams: { snapshot_root: join(root, 'custody') } } }), { mode: 0o600 })
  const original = process.env.OPSAIL_CHAT_BINDING_FILE
  process.env.OPSAIL_CHAT_BINDING_FILE = binding
  try {
    const result = await runChat('read', { provider: 'teams', mode: 'snapshot', conversationId: '19:fixture', limit: 30 })
    assert.equal(result.exitCode, 0, JSON.stringify(result.error))
    assert.equal(result.data.item_count, 1)
    assert.equal(result.data.content.capture.media.length, 1)
    // The saved locator does not prove readable attachment bytes.
    assert.equal(result.data.content.capture.media[0].capture_status, 'unavailable')
    assert.equal(result.data.content.capture.media[0].source_locator, 'fixture://one')
    assert.deepEqual(Object.keys(result.data.content.messages[0].media[0]).sort(), ['alt_text', 'locator', 'type'])
    const status = await runChat('status', { provider: 'teams' })
    assert.equal(status.exitCode, 0, JSON.stringify(status.error))
    assert.equal(status.data.providers[0].modes.snapshot.freshness.snapshotTimeKind, 'provider-export-metadata')
    assert.equal(status.data.providers[0].modes.snapshot.freshness.lastSuccessfulSyncAt, null)
  } finally {
    if (original === undefined) delete process.env.OPSAIL_CHAT_BINDING_FILE
    else process.env.OPSAIL_CHAT_BINDING_FILE = original
  }
})
