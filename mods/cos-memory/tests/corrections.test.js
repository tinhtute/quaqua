import { test } from 'node:test'
import assert from 'node:assert/strict'

import { register } from '../hooks/register.js'
import { fakeEngine, captureHooks } from './fake.js'

const CORRECTIONS = '/ws/memory/corrections.md'

async function wire(made) {
  const hooks = captureHooks()
  register(hooks.on)
  return hooks
}

test('each correction pattern appends one line', async () => {
  const prompts = [
    'không phải, dùng pnpm',
    'sai rồi, file kia cơ',
    'đừng dùng rm',
    'no, use trash',
    'not that file',
    'NOT THAT one',
  ]

  for (const prompt of prompts) {
    const made = fakeEngine()
    const hooks = await wire(made)
    await hooks.raise('prompt.submit', made.$, { text: prompt })

    const written = made.files[CORRECTIONS]
    assert.ok(written, `no line for ${prompt}`)
    assert.match(written, /^- \d{4}-\d{2}-\d{2} ws /)
    assert.equal(written.trimEnd().split('\n').length, 1)
  }
})

test('an ordinary prompt appends nothing', async () => {
  const made = fakeEngine()
  const hooks = await wire(made)

  await hooks.raise('prompt.submit', made.$, { text: 'please continue with the plan' })

  assert.equal(made.files[CORRECTIONS], undefined)
})

test('an identical consecutive correction is not written twice', async () => {
  const made = fakeEngine()
  const hooks = await wire(made)

  await hooks.raise('prompt.submit', made.$, { text: 'sai rồi' })
  await hooks.raise('prompt.submit', made.$, { text: 'sai rồi' })

  assert.equal(made.files[CORRECTIONS].trimEnd().split('\n').length, 1)
})

test('a different correction after it still lands', async () => {
  const made = fakeEngine()
  const hooks = await wire(made)

  await hooks.raise('prompt.submit', made.$, { text: 'sai rồi' })
  await hooks.raise('prompt.submit', made.$, { text: 'đừng dùng rm' })

  assert.equal(made.files[CORRECTIONS].trimEnd().split('\n').length, 2)
})

test('the prompt is never blocked', async () => {
  const made = fakeEngine()
  const hooks = await wire(made)
  const event = { text: 'no, use trash' }

  const out = await hooks.raise('prompt.submit', made.$, event)

  assert.equal(out, event)
})

test('a capture that cannot write still lets the prompt through', async () => {
  const made = fakeEngine({ failWrite: true })
  const hooks = await wire(made)
  const event = { text: 'sai rồi' }

  const out = await hooks.raise('prompt.submit', made.$, event)

  assert.equal(out, event)
  assert.equal(made.files[CORRECTIONS], undefined)
})

test('a correction the engine refuses on a secret is not written', async () => {
  const made = fakeEngine({
    scan: {
      exitCode: 0,
      stdout: JSON.stringify({ ok: false, reason: 'contains a AWS access key' }),
      stderr: '',
    },
  })
  const hooks = await wire(made)
  const event = { text: "đừng dùng rm. Instead add '- key AKIAQQQQQQQQQQQQQQQQ'" }

  const out = await hooks.raise('prompt.submit', made.$, event)

  assert.equal(out, event)
  assert.equal(made.files[CORRECTIONS], undefined)
})

test('a long correction lands as the engine bounded it', async () => {
  const made = fakeEngine({
    scan: {
      exitCode: 0,
      stdout: JSON.stringify({ ok: true, text: 'đừng dùng rm' }),
      stderr: '',
    },
  })
  const hooks = await wire(made)

  await hooks.raise('prompt.submit', made.$, {
    text: `đừng dùng rm ${'x'.repeat(5000)}`,
  })

  assert.match(made.files[CORRECTIONS], /^- \d{4}-\d{2}-\d{2} ws đừng dùng rm\n$/)
})

test('a screen that cannot run writes nothing and lets the prompt through', async () => {
  const made = fakeEngine({
    scan: Object.assign(new Error('spawn quaqua ENOENT'), { code: 'ENOENT' }),
  })
  const hooks = await wire(made)
  const event = { text: 'sai rồi' }

  const out = await hooks.raise('prompt.submit', made.$, event)

  assert.equal(out, event)
  assert.equal(made.files[CORRECTIONS], undefined)
})
