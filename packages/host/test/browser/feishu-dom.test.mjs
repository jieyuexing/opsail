import assert from 'node:assert/strict'
import test from 'node:test'
import vm from 'node:vm'
import {webcrypto} from 'node:crypto'
import {inspectPage} from '../../src/browser/extension/providers/dom.js'
import {settledPane} from '../../src/browser/extension/providers/settled-pane.js'

// A small tree of synthetic DOM nodes. All extraction runs through the actual
// serialized inspectPage, including current fibers, identity and scope checks.
class Element {
  constructor(tag, attrs = {}, value = '', children = []) {
    Object.assign(this, {tag, attrs, value, children, isConnected: true})
    for (const child of children) child.parentElement = this
  }
  get textContent() { return this.value + this.children.map(n => n.textContent).join('') }
  get innerText() { return this.textContent }
  getAttribute(key) { return this.attrs[key] ?? null }
  getAttributeNames() { return Object.keys(this.attrs) }
  matches(selectors) {
    return selectors.split(',').some(selector => {
      const parts = selector.trim().split(/\s+(?![^[]*\])/)
      const last = parts.pop()
      const attrs = [...last.matchAll(/\[([^\]=*^]+)(\*=|\^=|=)?(?:"([^"]*)")?\]/g)]
      if (!attrs.every(([, k, op, v]) => op === '=' ? this.attrs[k] === v : op === '*=' ? this.attrs[k]?.includes(v) : op === '^=' ? this.attrs[k]?.startsWith(v) : k in this.attrs)) return false
      const simple = last.replace(/\[[^\]]*\]/g, '')
      const classes = [...simple.matchAll(/\.([\w-]+)/g)].map(m => m[1])
      if (!classes.every(c => (this.attrs.class || '').split(' ').includes(c))) return false
      const tag = simple.split('.')[0]
      if (tag && tag !== this.tag) return false
      if (!parts.length) return true
      return !!this.parentElement?.closest(parts.join(' '))
    })
  }
  closest(selector) { for (let n = this; n; n = n.parentElement) if (n.matches(selector)) return n; return null }
  contains(node) { return node === this || this.children.some(n => n.contains(node)) }
  querySelectorAll(selector) { return this.children.flatMap(n => [...(n.matches(selector) ? [n] : []), ...n.querySelectorAll(selector)]) }
  querySelector(selector) { return this.querySelectorAll(selector)[0] || null }
  checkVisibility() { return true }
  click() { this.attrs.class += ' active' }
}
const el = (tag, attrs, value, children) => new Element(tag, attrs, value, children)
function row(id, {sender = 'Ada', senderClass = 'message-info__name', fromId, mine, timestamp, dateTime, quoted = false, plainRecord = false, chatter = true, chat} = {}) {
  const author = el('span', {class: senderClass}, sender)
  const node = el('div', {class: 'messageItem-wrapper', 'data-id': id}, '', [
    ...(sender ? [author] : []), el('span', {class: 'message-layout-time-tip'}, '09:55'),
    ...(dateTime ? [el('time', {datetime: dateTime})] : []),
    el('div', {class: 'message-text'}, 'hello', quoted ? [el('blockquote', {}, '', [el('span', {class: 'message-info-name'}, 'WRONG QUOTED AUTHOR')])] : []),
  ])
  const record = fields => plainRecord ? fields : {get(key) { return fields[key] }}
  const message = record({id, chatId: 'c1', fromId, createTime: timestamp})
  node.__reactFiber$fixture = {memoizedProps: {messageId: id, chatId: 'c1', mine, messageItem: record({message}), chatter: fromId && chatter ? record({id: fromId, name: fromId === 'self' ? 'Operator' : 'Model Author'}) : null, ...(chat ? {chat: record(chat)} : {})}, return: {memoizedProps: {id: 'c1', messages: []}}}
  return node
}
function fixture(items, {scope = 'c1'} = {}) {
  const pane = el('main', {'data-message-conversation-id': scope}, '', items)
  const card = el('div', {class: 'a11y_feed_card_item active', 'data-chat-id': 'c1'}, '', [el('div', {class: 'a11y_feed_card_main'}, '', [el('span', {style: 'min-width:0'}, 'Fixture')])])
  const document = el('document', {}, '', [card, pane])
  const location = {origin: 'https://fixture.feishu.cn', pathname: '/next/messenger/', href: 'https://fixture.feishu.cn/next/messenger/'}
  const context = {document, location, URL, TextEncoder, crypto: webcrypto, setTimeout, clearTimeout,
    getComputedStyle: element => element.computed || {overflowY: 'visible', flexDirection: 'row'},
    performance: {getEntriesByType: () => [{name: 'https://sf1-scmcdn-cn.feishucdn.com/static/js/index.8daec7ac.js', initiatorType: 'script'}]},
    window: {userId: 'self', configurationAdapter: {passport: {userId: 'self', getCurUserInfo: async () => ({user: {id: 'self', tenant: {id: 'tenant'}}})}}},
  }
  const inspect = vm.runInNewContext(`(${inspectPage.toString()})`, context)
  const call = operation => (args = {}) => inspect('feishu', operation, {conversationId: 'c1', ...args}, location.origin, {}, ['index.8daec7ac.js'])
  return {pane, card, context, read: call('read'), scroll: call('scroll')}
}
const separator = label => el('div', {class: 'message-date-separator'}, label)

test('Feishu names, grouped model authors and self identity survive a real read without diagnostic leakage', async () => {
  const f = fixture([row('1', {fromId: 'other', mine: false, quoted: true}), row('2', {sender: '', fromId: 'self', mine: true}), row('3', {sender: '', fromId: 'other', mine: false, plainRecord: true})])
  const result = await f.read()
  assert.equal(result.error, undefined)
  assert.deepEqual(Array.from(result.messages, m => [m.sender, m.sender_ref, m.is_self]), [['Ada', 'other', false], ['Operator', 'self', true], ['Model Author', 'other', false]])
  assert.doesNotMatch(JSON.stringify(result.diagnostics), /Ada|Operator|Model Author|WRONG QUOTED AUTHOR/)
})

test('missing or conflicting sender evidence stays unknown and never borrows the previous sender', async () => {
  const f = fixture([row('1', {fromId: 'other'}), row('2', {sender: '', quoted: true}), row('3', {fromId: 'other', mine: true})])
  const result = await f.read()
  assert.equal(result.messages[1].sender, ''); assert.equal(result.messages[1].is_self, null)
  assert.equal(result.messages[2].is_self, null)
  const stale = row('4', {sender: '', fromId: 'self', timestamp: 1790211600})
  stale.__reactFiber$fixture.memoizedProps.messageItem = {get: () => ({get: key => ({id: 'stale', chatId: 'other', fromId: 'self', createTime: 1790211600})[key]})}
  const rejectedMetadata = (await fixture([stale]).read()).messages[0]
  assert.equal(rejectedMetadata.sender_ref, null); assert.equal(rejectedMetadata.date_status, 'unknown')
  const conflict = row('5', {fromId: 'other', mine: false})
  conflict.__reactFiber$fixture.return.memoizedProps.message = {id: '5', chatId: 'c1', fromId: 'self'}
  assert.equal((await fixture([conflict]).read()).messages[0].is_self, null)
})

test('a p2p chat record names only its verified peer, and a run reuses a name only by exact fromId', async () => {
  const peer = {id: 'c1', chatterId: 'peer', name: 'Peer'}
  const p2p = await fixture([
    row('1', {sender: '', fromId: 'peer', mine: false, chatter: false, chat: peer}),
    row('2', {sender: '', fromId: 'self', mine: true, chatter: false, chat: peer}),
    row('3', {sender: '', fromId: 'peer', mine: false, chatter: false, chat: {...peer, id: 'other-chat'}}),
  ]).read()
  assert.deepEqual(Array.from(p2p.messages, m => [m.sender, m.sender_ref]), [['Peer', 'peer'], ['self', 'self'], ['Peer', 'peer']])
  const foreign = await fixture([row('1', {sender: '', fromId: 'peer', mine: false, chatter: false, chat: {...peer, id: 'other-chat'}}),
    row('2', {sender: '', fromId: 'peer', mine: false, chatter: false, chat: {...peer, chatterId: 'someone-else'}})]).read()
  assert.deepEqual(Array.from(foreign.messages, m => m.sender), ['peer', 'peer'])
  const group = await fixture([
    row('1', {sender: 'Ada', fromId: 'u1', mine: false, chatter: false}),
    row('2', {sender: '', fromId: 'u1', mine: false, chatter: false}),
    row('3', {sender: '', fromId: 'u2', mine: false, chatter: false}),
    row('4', {sender: '', mine: false, chatter: false}),
    row('5', {sender: 'Bob', fromId: 'u3', mine: false, chatter: false}),
    row('6', {sender: 'Bobby', fromId: 'u3', mine: false, chatter: false}),
    row('7', {sender: '', fromId: 'u3', mine: false, chatter: false}),
  ]).read({limit: 6})
  assert.deepEqual(Array.from(group.messages, m => m.sender), ['Ada', 'u2', '', 'Bob', 'Bobby', 'u3'])
  assert.doesNotMatch(JSON.stringify(group.diagnostics), /Ada|Bob|Peer/)
})

// Browsers clamp scrollTop to the scroll range; column-reverse ranges are negative.
function scrollable(pane, computed, scrollTop) {
  let value = 0
  Object.defineProperty(pane, 'scrollTop', {configurable: true, get: () => value, set: next => {
    const range = pane.scrollHeight - pane.clientHeight
    value = pane.computed.flexDirection === 'column-reverse' ? Math.min(0, Math.max(-range, next)) : Math.min(range, Math.max(0, next))
  }})
  Object.assign(pane, {computed, scrollHeight: 2000, clientHeight: 500})
  pane.scrollTop = scrollTop
}

test('scroll moves only the verified pane container, reports the top, and returns to the bottom', async () => {
  const f = fixture([row('1', {fromId: 'other'}), row('2', {fromId: 'other'})])
  scrollable(f.pane, {overflowY: 'auto', flexDirection: 'column'}, 1500)
  const first = await f.scroll({direction: 'up'})
  assert.deepEqual([first.direction, first.moved, first.atTop, f.pane.scrollTop], ['up', true, false, 1100])
  for (let i = 0; i < 3; i++) await f.scroll({direction: 'up'})
  const top = await f.scroll({direction: 'up'})
  assert.deepEqual([top.moved, top.atTop], [false, true])
  await f.scroll({direction: 'bottom'})
  assert.equal(f.pane.scrollTop, 1500)
  // A column-reverse list scrolls up with negative scrollTop.
  scrollable(f.pane, {overflowY: 'scroll', flexDirection: 'column-reverse'}, 0)
  assert.equal((await f.scroll({direction: 'up'})).atTop, false)
  assert.equal(f.pane.scrollTop, -400)
  f.pane.scrollTop = -1300
  assert.equal((await f.scroll({direction: 'up'})).atTop, true)
  await f.scroll({direction: 'bottom'})
  assert.equal(f.pane.scrollTop, 0)
  assert.doesNotMatch(JSON.stringify(first.diagnostics), /Ada|Model Author/)
})

test('scroll fails closed without a scrollable pane or a selected verified conversation', async () => {
  const f = fixture([row('1', {fromId: 'other'})])
  assert.equal((await f.scroll({direction: 'up'})).error.code, 'scroll-container-unverified')
  f.card.attrs.class = 'a11y_feed_card_item'
  scrollable(f.pane, {overflowY: 'auto', flexDirection: 'column'}, 1500)
  assert.equal((await f.scroll({direction: 'up'})).error.code, 'conversation-not-selected')
  assert.equal(f.pane.scrollTop, 1500)
})

test('date separators are applied before limit slicing, across days, without inventing a timezone', async () => {
  const result = await fixture([separator('2026年9月23日'), row('1'), row('2'), separator('2026-09-24'), row('3')]).read({limit: 2})
  assert.deepEqual(Array.from(result.messages, m => m.time), ['2026-09-23T09:55:00', '2026-09-24T09:55:00'])
  for (const message of result.messages) {
    assert.equal(message.date_status, 'known'); assert.equal(message.raw_time, '09:55')
    assert.equal(message.time_source, 'date-separator'); assert.equal(message.time_zone, 'unknown')
  }
})

test('today/yesterday use the page calendar, including year rollover', async () => {
  const f = fixture([separator('昨天'), row('1'), separator('Today'), row('2')])
  // Use a local calendar instant; the code must not get the date from UTC.
  f.context.Date = class extends Date { constructor(...args) { super(...(args.length ? args : [2026, 0, 1, 0, 5])) } }
  const result = await f.read()
  assert.deepEqual(Array.from(result.messages, m => m.time), ['2025-12-31T09:55:00', '2026-01-01T09:55:00'])
})

test('no separator, a missing year, invalid dates and an unknown separator retain raw time explicitly', async () => {
  for (const label of [null, '9月24日', '2026-02-30', '星期四', 'last week']) {
    const result = await fixture([...(label ? [separator('2026-09-23'), separator(label)] : []), row('1')]).read()
    assert.equal(result.messages[0].time, '09:55'); assert.equal(result.messages[0].date_status, 'unknown')
  }
})

test('matching message metadata and semantic time elements supply canonical dates; arbitrary data attributes do not', async () => {
  const first = row('1', {timestamp: '1790211600'})
  const second = row('2', {dateTime: '2026-09-24T10:16:00+08:00'})
  const third = row('3'); third.attrs['data-timestamp'] = '1790211600'
  const result = await fixture([first, second, third]).read()
  assert.equal(result.messages[0].time, new Date(1790211600 * 1000).toISOString())
  assert.equal(result.messages[0].time_source, 'message-metadata')
  assert.equal(result.messages[1].time, '2026-09-24T10:16:00+08:00')
  assert.equal(result.messages[2].date_status, 'unknown')
})

test('delayed pane never yields the previous chat, and returns after two settled verified samples', async () => {
  const f = fixture([row('1')], {scope: 'old'})
  let now = 0, samples = 0
  const result = await settledPane(async () => {
    samples++
    const value = await f.read()
    if (value.error) throw Object.assign(new Error(value.error.code), {code: value.error.code})
    return value
  }, {deadline: 1000, now: () => now, sleep: async ms => { now += ms; if (now >= 500) f.pane.attrs['data-message-conversation-id'] = 'c1' }})
  assert.equal(result.conversationId, 'c1'); assert.equal(samples, 4); assert.equal(now, 750)
})

test('explicit pane identity cannot override stale mounted rows, and an empty loading pane fails closed', async () => {
  const old = row('1'); old.__reactFiber$fixture.memoizedProps.chatId = 'old'
  assert.equal((await fixture([old]).read()).error.code, 'message-scope-unverified')
  const recycled = row('2'); recycled.__reactFiber$fixture.memoizedProps.messageId = 'stale-id'
  assert.equal((await fixture([recycled]).read()).error.code, 'message-scope-unverified')
  assert.equal((await fixture([]).read()).error.code, 'message-scope-unverified')
})

test('pane wait is bounded, resets on transitions, and never retries identity/ownership errors', async () => {
  for (const code of ['message-scope-unverified', 'conversation-not-selected']) {
    let now = 0, calls = 0
    await assert.rejects(settledPane(async () => { calls++; throw Object.assign(new Error(code), {code}) }, {deadline: 55000, now: () => now, sleep: async ms => { now += ms }}), {code})
    assert.equal(now, 55000); assert.equal(calls, 220)
  }
  for (const code of ['account-changed', 'tenant-changed', 'tab-not-prepared', 'page-changed', 'conversation-not-unique']) {
    let calls = 0
    await assert.rejects(settledPane(async () => { calls++; throw Object.assign(new Error(code), {code}) }, {deadline: Date.now() + 500}), {code})
    assert.equal(calls, 1)
  }
  await assert.rejects(settledPane(() => new Promise(() => {}), {deadline: Date.now() + 20}), {code: 'message-scope-unverified'})
  let now = 0, index = 0
  const states = ['old', null, 'new', 'new']
  const result = await settledPane(async () => {
    const id = states[index++]
    if (!id) throw Object.assign(new Error(), {code: 'message-scope-unverified'})
    return {conversationId: 'c1', messages: [{id}]}
  }, {deadline: 1000, now: () => now, sleep: async ms => { now += ms }})
  assert.equal(result.messages[0].id, 'new'); assert.equal(index, 4)
})

test('full dates within a time label are usable, but missing years and millisecond-shaped model values are not guessed', async () => {
  for (const [label, expected, status] of [['2026年9月24日 10:16', '2026-09-24T10:16:00', 'known'], ['9月24日 10:16', '9月24日 10:16', 'unknown']]) {
    const node = row('1'); node.querySelector('.message-layout-time-tip').value = label
    const result = (await fixture([separator('2026-09-23'), node]).read()).messages[0]
    assert.equal(result.time, expected); assert.equal(result.date_status, status)
  }
  const value = (await fixture([row('1', {timestamp: 1790211600000})]).read()).messages[0]
  assert.equal(value.date_status, 'unknown'); assert.equal(value.time, '09:55')
})
