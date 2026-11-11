import { test } from 'node:test'
import assert from 'node:assert/strict'

import { register } from '../hooks/register.js'
import { fakeEngine, captureHooks, settle, denyVerdict, allowVerdict } from './fake.js'

const RULES = '/ws/memory/rules.md'

function engine({ turns = 0, toolCalls = 0, stamp = null, fork = null, files = {}, stamps = {}, gate = allowVerdict } = {}) {
  const made = fakeEngine({ files, stamps, gate, fork })
  made.store.review = { turns, toolCalls, lastReviewAt: null, stamp, inflightAt: null }
  return made
}

async function wire(made) {
  const hooks = captureHooks()
  register(hooks.on)
  return hooks
}

const answered = text => ({ isAnswered: true, text })

test('nine turns are not enough', async () => {
  const made = engine({ turns: 8 })
  const hooks = await wire(made)

  await hooks.raise('classic.Stop', made.$, { stop_hook_active: false })
  await settle()

  assert.equal(made.calls.forks.length, 0)
})

test('ten turns start one review, and what it asks for lands', async () => {
  const made = engine({
    turns: 9,
    files: { [RULES]: 'old\n' },
    fork: answered('[{"op":"add","file":"memory/rules.md","text":"new rule"}]'),
  })
  const hooks = await wire(made)

  await hooks.raise('classic.Stop', made.$, { stop_hook_active: false })
  await settle()

  assert.equal(made.calls.forks.length, 1)
  assert.equal(made.calls.forks[0].effort, 'low')
  assert.equal(made.files[RULES], 'old\nnew rule\n')
  assert.equal(made.store.review.turns, 0)
})

test('a memory file that changed since the last review vetoes it', async () => {
  const made = engine({
    turns: 9,
    stamp: { rules: { mtimeMs: 1, size: 3 }, today: null },
    files: { [RULES]: 'old\n' },
    stamps: { [RULES]: { mtimeMs: 2, size: 4 } },
    fork: answered('[]'),
  })
  const hooks = await wire(made)

  await hooks.raise('classic.Stop', made.$, { stop_hook_active: false })
  await settle()

  assert.equal(made.calls.forks.length, 0)
})

test('unchanged memory lets the review through', async () => {
  const made = engine({
    turns: 9,
    stamp: { rules: { mtimeMs: 1, size: 4 }, today: null },
    files: { [RULES]: 'old\n' },
    stamps: { [RULES]: { mtimeMs: 1, size: 4 } },
    fork: answered('[]'),
  })
  const hooks = await wire(made)

  await hooks.raise('classic.Stop', made.$, { stop_hook_active: false })
  await settle()

  assert.equal(made.calls.forks.length, 1)
})

test('stop_hook_active vetoes the review', async () => {
  const made = engine({ turns: 9, fork: answered('[]') })
  const hooks = await wire(made)

  await hooks.raise('classic.Stop', made.$, { stop_hook_active: true })
  await settle()

  assert.equal(made.calls.forks.length, 0)
})

test('fifteen tool calls are enough on their own', async () => {
  const made = engine({ toolCalls: 14, fork: answered('[]') })
  const hooks = await wire(made)

  await hooks.raise('tool.call', made.$, { tool: 'Read', file_path: '/ws/x' })
  await hooks.raise('classic.Stop', made.$, { stop_hook_active: false })
  await settle()

  assert.equal(made.calls.forks.length, 1)
})

test('a second review waits another ten turns', async () => {
  const made = engine({ turns: 9, fork: answered('[]') })
  const hooks = await wire(made)

  await hooks.raise('classic.Stop', made.$, { stop_hook_active: false })
  await settle()
  assert.equal(made.calls.forks.length, 1)

  for (let i = 0; i < 9; i += 1) await hooks.raise('classic.Stop', made.$, { stop_hook_active: false })
  await settle()
  assert.equal(made.calls.forks.length, 1)

  await hooks.raise('classic.Stop', made.$, { stop_hook_active: false })
  await settle()
  assert.equal(made.calls.forks.length, 2)
})

test('a fork that never answers leaves the counters alone, so a later Stop retries', async () => {
  const made = engine({ turns: 9, fork: { isAnswered: false, reason: 'nothing to fork yet' } })
  const hooks = await wire(made)

  await hooks.raise('classic.Stop', made.$, { stop_hook_active: false })
  await settle()

  assert.equal(made.calls.forks.length, 1)
  assert.equal(made.store.review.turns, 10)
  assert.equal(made.store.review.inflightAt, null)

  await hooks.raise('classic.Stop', made.$, { stop_hook_active: false })
  await settle()
  assert.equal(made.calls.forks.length, 2)
})

test('an operation on a file outside the capped pair is dropped', async () => {
  const made = engine({
    turns: 9,
    files: { [RULES]: 'old\n' },
    fork: answered(
      '[{"op":"add","file":"memory/corrections.md","text":"sneaky"},{"op":"add","file":"memory/rules.md","text":"kept"}]',
    ),
  })
  const hooks = await wire(made)

  await hooks.raise('classic.Stop', made.$, { stop_hook_active: false })
  await settle()

  assert.equal(made.files[RULES], 'old\nkept\n')
  assert.equal(made.files['/ws/memory/corrections.md'], undefined)
})

test('an engine refusal is reported and the file is left alone', async () => {
  const made = engine({
    turns: 9,
    files: { [RULES]: 'old\n' },
    gate: denyVerdict('over the cap'),
    fork: answered('[{"op":"add","file":"memory/rules.md","text":"new"}]'),
  })
  const hooks = await wire(made)

  await hooks.raise('classic.Stop', made.$, { stop_hook_active: false })
  await settle()

  assert.equal(made.files[RULES], 'old\n')
  assert.equal(made.calls.writes.length, 0)
})

test('a reply that is not an array is no review at all', async () => {
  const made = engine({ turns: 9, fork: answered('I think you should remember the plan.') })
  const hooks = await wire(made)

  await hooks.raise('classic.Stop', made.$, { stop_hook_active: false })
  await settle()

  assert.equal(made.store.review.turns, 10)
})
