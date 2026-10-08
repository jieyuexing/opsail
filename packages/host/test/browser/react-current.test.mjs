import assert from 'node:assert/strict'
import test from 'node:test'
import {inspectPage} from '../../src/browser/extension/providers/dom.js'
// Execute the exact self-contained helper shipped via chrome.scripting.
const body = inspectPage.toString().split('// BEGIN CURRENT FIBER')[1].split('\n').slice(1).join('\n').split('// END CURRENT FIBER')[0]
const findCurrentFiber = new Function(body + '\nreturn findCurrentFiber')()

function pair() { const a = {}; const b = {}; a.alternate = b; b.alternate = a; return [a, b] }
function rootPair() { const [a, b] = pair(); const root = {current: a}; a.stateNode = b.stateNode = root; return [a, b, root] }

test('keeps a simple attached fiber without an alternate', () => {
  const fiber = {memoizedProps: {private: 'never inspected'}}
  assert.equal(findCurrentFiber(fiber), fiber)
})

test('resolves the current branch through alternate roots', () => {
  const [rootA, rootB, root] = rootPair(); const [a, b] = pair(); a.return = rootA; b.return = rootB; rootA.child = a; rootB.child = b
  assert.equal(findCurrentFiber(a), a)
  root.current = rootB
  assert.equal(findCurrentFiber(a), b)
})

test('handles bailout child lists shared by both alternate parents', () => {
  const [rootA, rootB, root] = rootPair(); const [parentA, parentB] = pair(); const [childA, childB] = pair()
  parentA.return = rootA; parentB.return = rootB; rootA.child = parentA; rootB.child = parentB
  parentA.child = parentB.child = childA; childA.return = parentA; childB.return = parentB
  assert.equal(findCurrentFiber(childA), childA)
  root.current = rootB
  // A shared bailout child is itself the mounted/current child on both trees.
  assert.equal(findCurrentFiber(childA), childA)
})

test('rejects unmounted roots and cyclic linkage', () => {
  const [a, b] = pair(); a.stateNode = b.stateNode = {current: {}}
  assert.equal(findCurrentFiber(a), null)
  a.return = a; b.return = b
  assert.equal(findCurrentFiber(a, {maxSteps: 8}), null)
})

test('handles deeply wrapped current Teams tree within a hard traversal bound', () => {
  const [rootA, rootB, root] = rootPair()
  let old = rootA, current = rootB
  for (let index = 0; index < 406; index++) {
    const [a, b] = pair(); a.return = old; b.return = current
    old.child = a; current.child = b; old = a; current = b
  }
  root.current = rootB
  assert.equal(findCurrentFiber(old), current)
  assert.equal(findCurrentFiber(old, {maxSteps: 256}), null)
})
