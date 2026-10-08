import test from 'node:test'
import assert from 'node:assert/strict'
import {inspectFeishuSelectedMessage} from '../../src/browser/extension/providers/feishu-message-probe.js'

const origin = 'https://tenant.feishu.cn'
const source = id => `feishu\u0000${origin}\u0000${id}`
const digest = async value => [...new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(value)))].map(byte => byte.toString(16).padStart(2, '0')).join('')
const expected = async () => ({accountHash: await digest(source('user-id')), tenantHash: await digest(source('tenant-id'))})
const identity = {user: {id: 'user-id', tenant: {id: 'tenant-id'}}}

class ImmutableRecordBase {
  constructor(values) { this._map = values }
  get(key) { return this._map[key] }
}
class ImmutableRecordLevel1 extends ImmutableRecordBase {}
class ImmutableRecordLevel2 extends ImmutableRecordLevel1 {}
class ImmutableRecordLevel3 extends ImmutableRecordLevel2 {}
class ImmutableRecordLevel4 extends ImmutableRecordLevel3 {}
class MessageItemFixture extends ImmutableRecordLevel4 {}
function messageNode({chatId = 'chat-1', position = 10, id = 'msg-1', createTime = '1720000000', stale = false} = {}) {
  const message = new ImmutableRecordBase({id, chatId, position, createTime})
  const item = new MessageItemFixture({message})
  const node = {getAttribute(name) { return name === 'data-id' ? id : null }, item}
  const current = {memoizedProps: {messageItem: item}, return: null}
  if (stale) {
    const oldMessage = new ImmutableRecordBase({id: 'old-message', chatId: 'old-chat', position: 1, createTime})
  const oldItem = new MessageItemFixture({message: oldMessage})
    const root = {current}
    const attached = {memoizedProps: {messageItem: oldItem}, return: null, stateNode: root}
    attached.alternate = current
    current.alternate = attached
    current.stateNode = root
    node.__reactFiber$test = attached
  } else node.__reactFiber$test = current
  return node
}
function environment({node = messageNode(), state, response, identityReader = async () => identity} = {}) {
  const calls = []
  const store = {getState() { return state ?? {status: {activeFeedId: 'chat-1'}, previews: {'chat-1': {feedId: 'chat-1', lastMessagePosition: 10}}} }}
  const adapter = {
    passport: {userId: 'user-id', getCurUserInfo: identityReader},
    transport: {
      callSdkApi: async (...args) => {
        calls.push(args)
        return response ?? {data: {messageItems: [{itemId: 'msg-1'}], entity: {messages: {'msg-1': {id: 'msg-1', chatId: 'chat-1', position: 10, createTime: '1720000000'}}}, dataComplete: true}}
      },
    },
  }
  const prior = {window: globalThis.window, location: globalThis.location, document: globalThis.document}
  globalThis.window = {configurationAdapter: adapter, __feedStore: store, userId: 'user-id'}
  globalThis.location = {origin}
  globalThis.document = {querySelectorAll(selector) { return selector === '.messageItem-wrapper[data-id]' ? [node] : [] }}
  return {calls, restore() { globalThis.window = prior.window; globalThis.location = prior.location; globalThis.document = prior.document }}
}
function pageResponse(rows, {invalidPositions = [], missingPositions = [], dataComplete = false} = {}) {
  return {data: {
    messageItems: rows.map(row => ({itemId: row.id})),
    entity: {messages: Object.fromEntries(rows.map(row => [row.id, row]))},
    invalidPositions,
    missingPositions,
    dataComplete,
  }}
}
function textMessage({id = 'msg-1', position = 10, text = 'hello', ...rest} = {}) {
  return {id, chatId: 'chat-1', position, createTime: '1720000000', type: 4, content: {richText: {innerText: text}}, ...rest}
}

test('serialized fixed probe derives a selected chat anchor and sends only fixed SDK args', async () => {
  const env = environment()
  try {
    const entry = (0, eval)(`(${inspectFeishuSelectedMessage.toString()})`)
    const result = await entry(origin, await expected())
    assert.equal(result.qualified, false)
    assert.equal(result.reason, 'fixed-message-probe-observed')
    assert.deepEqual(env.calls, [['1020|im.v1.GetChatMessagesRequest|im.v1.GetChatMessagesResponse|1|GET_CHAT_MESSAGES', {chatId: 'chat-1', position: 10, scene: 1, count: 1, strategy: 3, redundancyCount: 0, subscribChatEvent: false, needResponse: true}]])
    assert.equal(result.diagnostics.messageModel.verifiedSameItemCount, 1)
    assert.equal(result.diagnostics.response.responseEvidence.validTimeSeconds, true)
    assert.equal(JSON.stringify(result).includes('msg-1'), false)
  } finally { env.restore() }
})

test('uses bounded inherited Immutable Record getters and the mounted alternate rather than stale fiber props', async () => {
  const env = environment({node: messageNode({stale: true})})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected())
    assert.equal(result.reason, 'fixed-message-probe-observed')
    assert.equal(result.diagnostics.messageModel.verifiedSameItemCount, 1)
    assert.equal(result.diagnostics.messageModel.recordGetMaxDepth, 6)
    assert.equal(env.calls.length, 1)
  } finally { env.restore() }
})

test('does not accept a bare Message Record without the required MessageItem wrapper', async () => {
  const direct = new ImmutableRecordBase({id: 'msg-1', chatId: 'chat-1', position: 10, createTime: '1720000000'})
  const node = {getAttribute(name) { return name === 'data-id' ? 'msg-1' : null }}
  node.__reactFiber$test = {memoizedProps: {messageItem: direct}, return: null}
  const env = environment({node})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected())
    assert.equal(result.reason, 'selected-chat-anchor-unverified')
    assert.equal(result.diagnostics.messageModel.recordGetMethodMissingCount, 1)
    assert.equal(env.calls.length, 0)
  } finally { env.restore() }
})

test('a preview ID or visible message anchor mismatch blocks the SDK call', async () => {
  const mismatchPreview = environment({state: {status: {activeFeedId: 'chat-1'}, previews: {'chat-1': {feedId: 'other', lastMessagePosition: 10}}}})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected())
    assert.equal(result.reason, 'selected-chat-id-unverified')
    assert.equal(mismatchPreview.calls.length, 0)
  } finally { mismatchPreview.restore() }
  const mismatchPosition = environment({node: messageNode({position: 9})})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected())
    assert.equal(result.reason, 'selected-chat-anchor-unverified')
    assert.equal(mismatchPosition.calls.length, 0)
  } finally { mismatchPosition.restore() }
})

test('canonical message time must be a finite positive Unix-second scalar', async () => {
  const node = messageNode({createTime: 'not-a-time'})
  const env = environment({node})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected())
    assert.equal(result.reason, 'selected-chat-anchor-unverified')
    assert.equal(result.diagnostics.messageModel.verifiedSameItemCount, 0)
    assert.equal(env.calls.length, 0)
  } finally { env.restore() }
})

test('reports only sanitized response metadata shape after exact-chat identity validation', async () => {
  const secret = 'must-not-appear-in-probe-output'
  const response = {data: {messageItems: [{itemId: 'msg-1'}], entity: {messages: {
    'msg-1': {id: 'msg-1', chatId: 'chat-1', position: 10, createTime: '1720000000', content: {text: secret, secretField: secret}},
  }}, dataComplete: true}}
  const env = environment({response})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected())
    const metadata = result.diagnostics.response.firstMessageMetadata
    assert.equal(result.reason, 'fixed-message-probe-observed')
    assert.deepEqual(metadata.message.keys.find(item => item.key === 'content'), {key: 'content', type: 'object'})
    assert.deepEqual(metadata.content.keys.find(item => item.key === 'text'), {key: 'text', type: 'string'})
    assert.equal(metadata.content.keys.some(item => item.key === 'secretField'), false)
    assert.equal(JSON.stringify(result).includes(secret), false)
  } finally { env.restore() }
})

test('body structure diagnostics report only bounded field types and lengths, never element IDs or body values', async () => {
  const secret = 'must-not-appear-in-body-structure'
  const privateElementId = 'private-element-id'
  const response = {data: {messageItems: [{itemId: 'msg-1'}], entity: {messages: {
    'msg-1': {id: 'msg-1', chatId: 'chat-1', position: 10, createTime: '1720000000', content: {
      richText: {innerText: '', elementIds: [privateElementId], elements: {[privateElementId]: {tag: 1, childIds: [], property: {text: {content: secret}}}}},
      values: [{text: secret}],
    }},
  }}, dataComplete: true}}
  const env = environment({response})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected())
    const body = result.diagnostics.response.firstMessageMetadata.bodyStructure
    assert.equal(body.innerTextLength, 0)
    assert.equal(body.elementIdsCount, 1)
    assert.deepEqual(body.elements, {type: 'object', count: 1})
    assert.deepEqual(body.firstElements[0].fields.find(field => field.key === 'tag'), {key: 'tag', value: {type: 'number'}})
    assert.equal(body.values.count, 1)
    assert.equal(JSON.stringify(body).includes(secret), false)
    assert.equal(JSON.stringify(body).includes(privateElementId), false)
  } finally { env.restore() }
})

test('reads a bounded selected-chat initial page only after the DOM anchor and response identity match', async () => {
  const response = pageResponse([
    textMessage({id: 'msg-9', position: 9, text: 'earlier', fromId: 'sender-1', rootId: 'root-1'}),
    {id: 'msg-1', chatId: 'chat-1', position: 10, createTime: '1720000001', type: 2, content: {title: 'Post title', richText: {innerText: 'Post body'}}, fromId: 'sender-1', isEdited: true, editTimeMs: '1720000002000', editVersion: 3, isRecalled: false, isDeleted: false},
  ], {invalidPositions: [8], missingPositions: [], dataComplete: false})
  response.data.entity.chatters = {'sender-1': {name: 'Alice'}}
  const env = environment({response})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected(), {mode: 'initial', expectedChatId: 'chat-1', limit: 2})
    assert.equal(result.reason, 'selected-chat-page-observed')
    assert.equal(result.conversationId, 'chat-1')
    assert.equal(result.selectedPosition, 10)
    assert.equal(result.requestedPosition, 10)
    assert.deepEqual(env.calls, [['1020|im.v1.GetChatMessagesRequest|im.v1.GetChatMessagesResponse|1|GET_CHAT_MESSAGES', {chatId: 'chat-1', position: 10, scene: 1, count: 2, strategy: 3, redundancyCount: 0, subscribChatEvent: false, needResponse: true}]])
    assert.deepEqual(result.messages.map(message => [message.id, message.position, message.text]), [['msg-9', 9, 'earlier'], ['msg-1', 10, 'Post title\nPost body']])
    assert.deepEqual(result.messages[0].reply_to, ['root-1'])
    assert.deepEqual(result.messages[0].sender, {ref: 'sender-1', display_name: 'Alice'})
    assert.equal(result.messages[1].revision, 3)
    assert.equal(result.messages[1].edited, true)
    assert.equal(result.messages[1].edited_at, '2024-07-03T09:46:42.000Z')
    assert.equal(result.messages[1].is_recalled, false)
    assert.equal(result.messages[1].is_deleted, false)
    assert.deepEqual(result.invalidPositions, [8])
    assert.deepEqual(result.missingPositions, [])
    assert.equal(result.dataComplete, false)
    assert.deepEqual(result.diagnostics.response.firstMessageMetadata.richText.keys.find(item => item.key === 'innerText'), {key: 'innerText', type: 'string'})
    assert.deepEqual(result.diagnostics.response.pageSchema.firstItemFields, {
      id: 'string', chatId: 'string', position: 'number', createTime: 'string', type: 'number', content: 'object', fromId: 'string',
      editVersion: 'undefined', isEdited: 'undefined', editTimeMs: 'undefined', isRecalled: 'undefined', isDeleted: 'undefined', rootId: 'string', parentId: 'undefined',
    })
    assert.equal(result.diagnostics.response.pageSchema.invalidPositionsArray, true)
    assert.equal(JSON.stringify(result.diagnostics.response.firstMessageMetadata).includes('earlier'), false)
  } finally { env.restore() }
})

test('reads only a bounded previous page at the caller-provided overlapping position', async () => {
  const env = environment({response: pageResponse([textMessage({id: 'msg-9', position: 9})])})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected(), {mode: 'previous', expectedChatId: 'chat-1', priorPosition: 10, limit: 1})
    assert.equal(result.reason, 'selected-chat-page-observed')
    assert.equal(result.requestedPosition, 10)
    assert.equal(result.messages[0].position, 9)
    assert.deepEqual(env.calls[0][1], {chatId: 'chat-1', position: 10, scene: 3, count: 1, strategy: 3, redundancyCount: 0, subscribChatEvent: false, needResponse: true})
  } finally { env.restore() }
})

test('rebuilds visible rich text from ordered elements when the SDK innerText is empty', async () => {
  const elements = {
    paragraph: {tag: 9, property: {paragraph: {}}, childIds: ['text', 'at', 'anchor', 'list']},
    text: {tag: 1, property: {text: {content: 'Hello '}}, childIds: []},
    at: {tag: 2, property: {at: {content: 'Ada'}}, childIds: []},
    anchor: {tag: 3, property: {anchor: {content: ' docs'}}, childIds: []},
    list: {tag: 4, property: {ul: {}}, childIds: ['item']},
    item: {tag: 5, property: {li: {}}, childIds: ['item-text']},
    'item-text': {tag: 1, property: {text: {content: 'item'}}, childIds: []},
  }
  const richText = {innerText: '', elementIds: ['paragraph'], elements}
  const response = pageResponse([
    {id: 'msg-9', chatId: 'chat-1', position: 9, createTime: '1720000000', type: 4, content: {richText}},
    {id: 'msg-1', chatId: 'chat-1', position: 10, createTime: '1720000001', type: 2, content: {title: 'Post', richText}},
  ])
  const env = environment({response})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected(), {mode: 'initial', expectedChatId: 'chat-1', limit: 2})
    assert.equal(result.reason, 'selected-chat-page-observed')
    assert.equal(result.messages[0].text, 'Hello @Ada docs- item')
    assert.equal(result.messages[1].text, 'Post\nHello @Ada docs- item')
    for (const message of result.messages) {
      assert.equal(message.text_projection, 'richtext-elements')
      assert.equal(message.renderedElementCount, 7)
      assert.equal(message.unknownElementCount, 0)
    }
  } finally { env.restore() }
})

test('renders a tag-37 codeBlockV2 using its saved frontend line and fragment contract', async () => {
  const richText = {innerText: '', elementIds: ['code'], elements: {code: {
    tag: 37,
    property: {codeBlockV2: {language: 1, wordWrap: false, contents: [
      {contents: [{type: 'keyword', content: 'const '}, {type: 'plain', content: 'value = 1;'}]},
      {contents: [{type: 'plain', content: 'return value;'}]},
    ]}},
    childIds: [],
  }}}
  const env = environment({response: pageResponse([{id: 'msg-1', chatId: 'chat-1', position: 10, createTime: '1720000000', type: 4, content: {richText}}])})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected(), {mode: 'initial', expectedChatId: 'chat-1', limit: 1})
    assert.equal(result.reason, 'selected-chat-page-observed')
    assert.equal(result.messages[0].text, 'const value = 1;\nreturn value;')
    assert.equal(result.messages[0].text_projection, 'richtext-elements')
    assert.equal(result.messages[0].unknownElementCount, 0)
  } finally { env.restore() }
})

test('a missing rich-text element or cycle produces an explicit unavailable body rather than a partial body', async () => {
  const cyclic = {innerText: '', elementIds: ['one'], elements: {one: {tag: 9, property: {paragraph: {}}, childIds: ['two']}, two: {tag: 9, property: {paragraph: {}}, childIds: ['one']}}}
  const env = environment({response: pageResponse([{id: 'msg-1', chatId: 'chat-1', position: 10, createTime: '1720000000', type: 4, content: {richText: cyclic}}])})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected(), {mode: 'initial', expectedChatId: 'chat-1', limit: 1})
    assert.equal(result.reason, 'selected-chat-page-observed')
    assert.equal(result.messages[0].text, '')
    assert.equal(result.messages[0].content_unavailable_reason, 'richtext-elements-unrenderable')
    assert.equal(result.messages[0].text_projection, 'richtext-elements')
    assert.equal(result.messages[0].unknownElementCount > 0, true)
    const unknown = result.messages[0].content_evidence.unknown_element_structures
    assert.equal(unknown.length, 1)
    assert.equal(unknown[0].tag, 9)
    assert.equal(unknown[0].childCount, 1)
    assert.deepEqual(unknown[0].property.fields.find(field => field.key === 'paragraph'), {key: 'paragraph', value: {type: 'object', fields: []}})
  } finally { env.restore() }
})

test('text with inline media preserves readable text and emits safe media coverage metadata', async () => {
  const richText = {innerText: '', elementIds: ['text', 'image'], elements: {
    text: {tag: 1, property: {text: {content: 'caption'}}, childIds: []},
    image: {tag: 8, property: {image: {token: 'private-media-token'}}, childIds: []},
  }}
  const env = environment({response: pageResponse([{id: 'msg-1', chatId: 'chat-1', position: 10, createTime: '1720000000', type: 4, content: {richText}}])})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected(), {mode: 'initial', expectedChatId: 'chat-1', limit: 1})
    assert.equal(result.reason, 'selected-chat-page-observed')
    assert.equal(result.messages[0].text, 'caption')
    assert.deepEqual(result.messages[0].media, [{kind: 'inline-media', available: false}])
    assert.equal(result.messages[0].text_projection, 'richtext-elements')
    assert.equal(result.messages[0].inlineMediaCount, 1)
    assert.equal(JSON.stringify(result).includes('private-media-token'), false)
  } finally { env.restore() }
})

test('projects recognized media to a kind without returning source media keys or content values', async () => {
  const sourceKey = 'private-media-key-must-not-leak'
  const env = environment({response: pageResponse([{id: 'msg-1', chatId: 'chat-1', position: 10, createTime: '1720000000', type: 1, content: {imageKey: sourceKey}}])})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected(), {mode: 'initial', expectedChatId: 'chat-1', limit: 1})
    assert.equal(result.reason, 'selected-chat-page-observed')
    assert.deepEqual(result.messages[0].media, [{kind: 'image', available: false}])
    assert.equal(result.messages[0].content_unavailable, true)
    assert.equal(JSON.stringify(result).includes(sourceKey), false)
  } finally { env.restore() }
})

test('invalid history options and missing anchor overlap fail closed, while unavailable text remains an explicit metadata record', async () => {
  const badOptions = environment()
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected(), {mode: 'previous', expectedChatId: 'other', priorPosition: 10, limit: 1})
    assert.equal(result.reason, 'history-request-invalid')
    assert.equal(badOptions.calls.length, 0)
  } finally { badOptions.restore() }

  const noAnchor = environment({response: pageResponse([textMessage({id: 'msg-9', position: 9})])})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected(), {mode: 'initial', expectedChatId: 'chat-1', limit: 1})
    assert.equal(result.reason, 'selected-chat-page-unrecognized')
    assert.equal('messages' in result, false)
  } finally { noAnchor.restore() }

  const unavailableText = environment({response: pageResponse([
    textMessage({id: 'msg-1', position: 10}),
    {id: 'msg-2', chatId: 'chat-1', position: 9, createTime: '1720000000', type: 4, content: {}},
  ])})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected(), {mode: 'initial', expectedChatId: 'chat-1', limit: 2})
    assert.equal(result.reason, 'selected-chat-page-observed')
    assert.deepEqual(result.messages[1].media, [])
    assert.equal(result.messages[1].text, '')
    assert.equal(result.messages[1].content_unavailable, true)
    assert.equal(result.messages[1].content_unavailable_reason, 'richtext-elements-unrenderable')
    assert.deepEqual(result.messages[1].content_evidence, {content: {type: 'object', keys: []}, richText: {type: 'undefined', keys: []}})
  } finally { unavailableText.restore() }
})

test('a recalled or deleted message may have no content and remains an explicit tombstone', async () => {
  const tombstone = {id: 'msg-1', chatId: 'chat-1', position: 10, createTime: '1720000000', type: 4, content: null, isRecalled: true, isDeleted: true}
  const env = environment({response: pageResponse([tombstone])})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected(), {mode: 'initial', expectedChatId: 'chat-1', limit: 1})
    assert.equal(result.reason, 'selected-chat-page-observed')
    assert.equal(result.messages[0].text, '')
    assert.equal(result.messages[0].content_unavailable, true)
    assert.equal(result.messages[0].content_unavailable_reason, 'recalled')
    assert.equal(result.messages[0].is_recalled, true)
    assert.equal(result.messages[0].is_deleted, true)
    assert.deepEqual(result.messages[0].content_evidence, {content: {type: 'null', keys: []}, richText: {type: 'undefined', keys: []}})
  } finally { env.restore() }
})

test('an edited record with null live editVersion and no valid edit timestamp preserves the edited marker and null time', async () => {
  const env = environment({response: pageResponse([textMessage({editVersion: null, isEdited: true, editTimeMs: ''})])})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected(), {mode: 'initial', expectedChatId: 'chat-1', limit: 1})
    assert.equal(result.reason, 'selected-chat-page-observed')
    assert.equal(result.messages[0].edited, true)
    assert.equal(result.messages[0].edited_at, null)
    assert.equal('revision' in result.messages[0], false)
  } finally { env.restore() }
})

test('a non-Record wrapped message blocks the SDK call', async () => {
  const node = messageNode()
  node.__reactFiber$test.memoizedProps.messageItem = new MessageItemFixture({message: {id: 'msg-1', chatId: 'chat-1', position: 10, createTime: '1720000000'}})
  const env = environment({node})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected())
    assert.equal(result.reason, 'selected-chat-anchor-unverified')
    assert.equal(result.diagnostics.messageModel.recordGetMethodMissingCount, 1)
    assert.equal(env.calls.length, 0)
  } finally { env.restore() }
})

test('selection or identity change after the read discards response metadata', async () => {
  let reads = 0
  const env = environment({identityReader: async () => ++reads === 1 ? identity : {user: {id: 'changed', tenant: {id: 'tenant-id'}}}})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected())
    assert.equal(result.reason, 'identity-changed')
    assert.equal('response' in result.diagnostics, false)
  } finally { env.restore() }
})

test('metadata diagnostics distinguish unresolved fiber, missing model methods, getter failure, and position mismatch', async () => {
  const noFiber = {getAttribute() { return 'msg-1' }}
  const first = environment({node: noFiber})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected())
    assert.equal(result.reason, 'selected-chat-anchor-unverified')
    assert.equal(result.diagnostics.messageModel.attachedFiberCount, 0)
    assert.equal(result.diagnostics.messageModel.currentFiberUnresolvedCount, 0)
    assert.equal(first.calls.length, 0)
  } finally { first.restore() }

  const noMethods = {getAttribute() { return 'msg-1' }}
  noMethods.__reactFiber$test = {memoizedProps: {messageItem: {}}, return: null}
  const second = environment({node: noMethods})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected())
    assert.equal(result.diagnostics.messageModel.messageItemSeenCount, 1)
    assert.equal(result.diagnostics.messageModel.recordGetMethodMissingCount, 1)
    assert.equal(second.calls.length, 0)
  } finally { second.restore() }

  class ThrowingItem extends MessageItemFixture { get() { throw new Error('no scalar output') } }
  const throwing = messageNode()
  throwing.__reactFiber$test.memoizedProps.messageItem = new ThrowingItem({message: new ImmutableRecordBase({})})
  const third = environment({node: throwing})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected())
    assert.equal(result.diagnostics.messageModel.messageGetFailureCount, 1)
    assert.equal(third.calls.length, 0)
  } finally { third.restore() }

  const fourth = environment({node: messageNode({position: 9})})
  try {
    const result = await inspectFeishuSelectedMessage(origin, await expected())
    const meta = result.diagnostics.messageModel
    assert.equal(meta.stableIdMatchedCount, 1)
    assert.equal(meta.chatMatchedCount, 1)
    assert.equal(meta.canonicalTimeMatchedCount, 1)
    assert.equal(meta.positionMatchedCount, 0)
    assert.equal(meta.verifiedSameItemCount, 0)
    assert.equal(fourth.calls.length, 0)
  } finally { fourth.restore() }
})
