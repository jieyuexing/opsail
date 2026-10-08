import test from 'node:test'
import assert from 'node:assert/strict'
import {continuation, resumeContinuation} from '../../src/browser/extension/providers/selected-chat-cursor.js'
const context = {conversationId: '123', accountHash: 'account', tenantHash: 'tenant', documentId: 'doc', buildId: 'build'}
test('continuation preserves one position of overlap and pins exact identity/document/build', () => {
  const cursor = continuation({reason: 'selected-chat-page-observed', conversationId: '123', messages: [{position: 80}, {position: 100}], selectedPosition: 100}, context, 1000)
  assert.equal(resumeContinuation(cursor, context, 1001), 81)
  for (const key of Object.keys(context)) assert.throws(() => resumeContinuation(cursor, {...context, [key]: 'changed'}, 1001), {code: 'cursor-context-changed'})
  assert.throws(() => resumeContinuation(cursor, context, 1_801_000), {code: 'cursor-expired'})
  assert.throws(() => resumeContinuation(undefined, context), {code: 'invalid-cursor'})
})
test('unverified or empty page cannot issue a continuation', () => {
  for (const page of [{}, {reason: 'selected-chat-page-observed', conversationId: 'other'}, {reason: 'selected-chat-page-observed', conversationId: '123', messages: []}]) assert.throws(() => continuation(page, context))
})
