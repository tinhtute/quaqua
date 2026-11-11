import { test } from 'node:test'
import assert from 'node:assert/strict'

import { register } from '../hooks/register.js'
import { fakeEngine, captureHooks, settle, denyVerdict, allowVerdict } from './fake.js'

const RULES = '/ws/memory/rules.md'

function engine({
  prompts = 0,
  stops = 0,
  toolCalls = 0,
  stamp = null,
  fork = null,
  files = {},
  stamps = {},
  gate = allowVerdict,
} = {}) {
  const made = fakeEngine({ files, stamps, gate, fork })
  made.store.review = {
    prompts,
    stops,
    toolCalls,
    lastReviewAt: null,
    stamp,
    inflightAt: null,
  }
  return made
}

async function wire(made) {
  const hooks = captureHooks()
  register(hooks.on)
  return hooks
}

const answered = text => ({ isAnswered: true, text })

// One main-loop turn ending: what the engine raises once per user turn.
async function stops(hooks, $, n) {
  for (let i = 0; i < n; i += 1) await hooks.raise('turn.complete', $, { reason: 'answer', answer: 'ok' })
}

async function prompts(hooks, $, n) {
  for (let i = 0; i < n; i += 1) await hooks.raise('prompt.submit', $, { text: 'continue' })
}

test('nine stops are not enough', async () => {
  const made = engine({ prompts: 10, stops: 8 })
  const hooks = await wire(made)

  await stops(hooks, made.$, 1)
  await settle()

  assert.equal(made.calls.forks.length, 0)
})

test('ten stops start one review, and what it asks for lands', async () => {
  const made = engine({
    prompts: 10,
    stops: 9,
    files: { [RULES]: 'old\n' },
    fork: answered('[{"op":"add","file":"memory/rules.md","text":"new rule"}]'),
  })
  const hooks = await wire(made)

  await stops(hooks, made.$, 1)
  await settle()

  assert.equal(made.calls.forks.length, 1)
  assert.equal(made.calls.forks[0].effort, 'low')
  assert.equal(made.files[RULES], 'old\nnew rule\n')
  assert.equal(made.store.review.stops, 0)
})

test('fewer than ten user turns hold the review back', async () => {
  const made = engine({ prompts: 9, stops: 9, fork: answered('[]') })
  const hooks = await wire(made)

  await stops(hooks, made.$, 1)
  await settle()

  assert.equal(made.calls.forks.length, 0)
})

test('a memory file that changed since the last review vetoes it', async () => {
  const made = engine({
    prompts: 10,
    stops: 9,
    stamp: { rules: { mtimeMs: 1, size: 3 }, today: null },
    files: { [RULES]: 'old\n' },
    stamps: { [RULES]: { mtimeMs: 2, size: 4 } },
    fork: answered('[]'),
  })
  const hooks = await wire(made)

  await stops(hooks, made.$, 1)
  await settle()

  assert.equal(made.calls.forks.length, 0)
})

test('a veto moves the baseline, so the next window can review again', async () => {
  const made = engine({
    prompts: 10,
    stops: 9,
    stamp: { rules: { mtimeMs: 1, size: 3 }, today: null },
    files: { [RULES]: 'old\n' },
    stamps: { [RULES]: { mtimeMs: 2, size: 4 } },
    fork: answered('[]'),
  })
  const hooks = await wire(made)

  await stops(hooks, made.$, 1)
  await settle()
  assert.equal(made.calls.forks.length, 0)

  // Memory has not moved since the veto: the next window must be able to fire.
  await stops(hooks, made.$, 10)
  await settle()

  assert.equal(made.calls.forks.length, 1)
})

test('unchanged memory lets the review through', async () => {
  const made = engine({
    prompts: 10,
    stops: 9,
    stamp: { rules: { mtimeMs: 1, size: 4 }, today: null },
    files: { [RULES]: 'old\n' },
    stamps: { [RULES]: { mtimeMs: 1, size: 4 } },
    fork: answered('[]'),
  })
  const hooks = await wire(made)

  await stops(hooks, made.$, 1)
  await settle()

  assert.equal(made.calls.forks.length, 1)
})

test('only a main-loop answer counts as a turn', async () => {
  const made = engine({ prompts: 10, stops: 9, fork: answered('[]') })
  const hooks = await wire(made)

  await hooks.raise('turn.complete', made.$, { reason: 'answer', agentId: 'sub-1', answer: 'x' })
  await hooks.raise('turn.complete', made.$, { reason: 'aborted' })
  await hooks.raise('turn.complete', made.$, { reason: 'error' })
  await settle()

  assert.equal(made.calls.forks.length, 0)
  assert.equal(made.store.review.stops, 9)
})

test('a real session turn increments the counter and reaches the review', async () => {
  const made = engine({ prompts: 10, stops: 0, fork: answered('[]') })
  const hooks = await wire(made)

  await stops(hooks, made.$, 1)
  assert.equal(made.store.review.stops, 1)

  await stops(hooks, made.$, 9)
  await settle()
  assert.equal(made.calls.forks.length, 1)
})

test('the dead classic.Stop hook is not registered', async () => {
  const hooks = await wire(engine())
  assert.equal(hooks.handlers('classic.Stop').length, 0)
})

test('fifteen tool calls are enough on their own', async () => {
  const made = engine({ prompts: 10, toolCalls: 14, fork: answered('[]') })
  const hooks = await wire(made)

  await hooks.raise('tool.call', made.$, { tool: 'Read', file_path: '/ws/x' })
  await stops(hooks, made.$, 1)
  await settle()

  assert.equal(made.calls.forks.length, 1)
})

test('fifteen tool calls in the first turn do not start a second review', async () => {
  const made = engine({ fork: answered('[]') })
  const hooks = await wire(made)

  // A review just happened: every counter is at zero.
  for (let i = 0; i < 15; i += 1) {
    await hooks.raise('tool.call', made.$, { tool: 'Read', file_path: '/ws/x' })
  }
  await stops(hooks, made.$, 1)
  await settle()

  assert.equal(made.calls.forks.length, 0)
})

test('a second review waits another ten user turns', async () => {
  const made = engine({ prompts: 10, stops: 9, fork: answered('[]') })
  const hooks = await wire(made)

  await stops(hooks, made.$, 1)
  await settle()
  assert.equal(made.calls.forks.length, 1)

  await prompts(hooks, made.$, 9)
  await stops(hooks, made.$, 10)
  await settle()
  assert.equal(made.calls.forks.length, 1)

  await prompts(hooks, made.$, 1)
  await stops(hooks, made.$, 1)
  await settle()
  assert.equal(made.calls.forks.length, 2)
})

test('a fork that never answers leaves the counters alone, so a later Stop retries', async () => {
  const made = engine({ prompts: 10, stops: 9, fork: { isAnswered: false, reason: 'nothing to fork yet' } })
  const hooks = await wire(made)

  await stops(hooks, made.$, 1)
  await settle()

  assert.equal(made.calls.forks.length, 1)
  assert.equal(made.store.review.stops, 10)
  assert.equal(made.store.review.inflightAt, null)

  await stops(hooks, made.$, 1)
  await settle()
  assert.equal(made.calls.forks.length, 2)
})

test('a review that throws releases its marker', async () => {
  const made = engine({ prompts: 10, stops: 9, fork: new Error('provider 500') })
  const hooks = await wire(made)

  await stops(hooks, made.$, 1)
  await settle()

  assert.equal(made.store.review.inflightAt, null)
  assert.equal(made.store.review.stops, 10)
})

test('an operation on a file outside the capped pair is dropped', async () => {
  const made = engine({
    prompts: 10,
    stops: 9,
    files: { [RULES]: 'old\n' },
    fork: answered(
      '[{"op":"add","file":"memory/corrections.md","text":"sneaky"},{"op":"add","file":"memory/rules.md","text":"kept"}]',
    ),
  })
  const hooks = await wire(made)

  await stops(hooks, made.$, 1)
  await settle()

  assert.equal(made.files[RULES], 'old\nkept\n')
  assert.equal(made.files['/ws/memory/corrections.md'], undefined)
})

test('an engine refusal is reported and the file is left alone', async () => {
  const made = engine({
    prompts: 10,
    stops: 9,
    files: { [RULES]: 'old\n' },
    gate: denyVerdict('over the cap'),
    fork: answered('[{"op":"add","file":"memory/rules.md","text":"new"}]'),
  })
  const hooks = await wire(made)

  await stops(hooks, made.$, 1)
  await settle()

  assert.equal(made.files[RULES], 'old\n')
  assert.equal(made.calls.writes.length, 0)
})

test('a reply that is not an array is no review at all', async () => {
  const made = engine({ prompts: 10, stops: 9, fork: answered('I think you should remember the plan.') })
  const hooks = await wire(made)

  await stops(hooks, made.$, 1)
  await settle()

  assert.equal(made.store.review.stops, 10)
})
