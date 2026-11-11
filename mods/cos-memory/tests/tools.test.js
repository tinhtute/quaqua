import { test } from 'node:test'
import assert from 'node:assert/strict'

import { runTool } from '../src/tools.js'
import { fakeEngine, denyVerdict, allowVerdict } from './fake.js'

const ENGINE_REASON = "memory/rules.md is 2400 chars, over the 2200 cap (AGENTS.md '## Memory')"

test('an engine refusal is the tool error, verbatim, and nothing is written', async () => {
  const { $, calls, files } = fakeEngine({
    files: { '/ws/memory/rules.md': 'old\n' },
    gate: denyVerdict(ENGINE_REASON),
  })

  const out = await runTool($, 'memory_add', { file: 'memory/rules.md', text: 'new rule' })

  assert.deepEqual(out, { deny: ENGINE_REASON })
  assert.equal(files['/ws/memory/rules.md'], 'old\n')
  assert.equal(calls.writes.length, 0)
})

test('an allowed write lands and reports the file it touched', async () => {
  const { $, files } = fakeEngine({
    files: { '/ws/memory/rules.md': 'old\n' },
    gate: allowVerdict,
  })

  const out = await runTool($, 'memory_add', { file: 'memory/rules.md', text: 'new rule' })

  assert.deepEqual(out, { result: 'updated memory/rules.md' })
  assert.equal(files['/ws/memory/rules.md'], 'old\nnew rule\n')
})

test('the gate is asked about the absolute path and the whole result', async () => {
  const { $, calls } = fakeEngine({ files: { '/ws/USER.md': 'a\n' }, gate: allowVerdict })

  await runTool($, 'memory_replace', { file: 'USER.md', old_text: 'a', new_text: 'b' })

  assert.equal(calls.process.length, 1)
  const [call] = calls.process
  assert.deepEqual(call.argv, ['quaqua', 'memory', 'hook', 'pre'])
  assert.equal(call.init.cwd, '/ws')
  assert.deepEqual(JSON.parse(call.init.stdin), {
    tool_name: 'Write',
    tool_input: { file_path: '/ws/USER.md', content: 'b\n' },
  })
})

test('a gate that cannot run is a refusal, not a silent allow', async () => {
  const { $, calls } = fakeEngine({
    files: { '/ws/memory/rules.md': '' },
    gate: Object.assign(new Error('spawn quaqua ENOENT'), { code: 'ENOENT' }),
  })

  const out = await runTool($, 'memory_add', { file: 'memory/rules.md', text: 'x' })

  assert.match(out.deny, /unavailable/)
  assert.equal(calls.writes.length, 0)
})

test('a gate that exits non-zero is a refusal', async () => {
  const { $, calls } = fakeEngine({
    files: { '/ws/memory/rules.md': '' },
    gate: { exitCode: 2, stdout: '', stderr: 'boom' },
  })

  const out = await runTool($, 'memory_add', { file: 'memory/rules.md', text: 'x' })

  assert.match(out.deny, /failed \(exit 2\).*boom/s)
  assert.equal(calls.writes.length, 0)
})

test('a verdict that cannot be parsed is a refusal', async () => {
  const { $, calls } = fakeEngine({
    files: { '/ws/memory/rules.md': '' },
    gate: { exitCode: 0, stdout: 'not json', stderr: '' },
  })

  const out = await runTool($, 'memory_add', { file: 'memory/rules.md', text: 'x' })

  assert.match(out.deny, /no verdict/)
  assert.equal(calls.writes.length, 0)
})

test('a file outside the capped pair is refused without asking the engine', async () => {
  const { $, calls } = fakeEngine({ gate: allowVerdict })

  const out = await runTool($, 'memory_add', { file: 'memory/corrections.md', text: 'x' })

  assert.match(out.deny, /not a capped memory file/)
  assert.equal(calls.process.length, 0)
  assert.equal(calls.writes.length, 0)
})

test('memory_replace refuses an old_text that is not there', async () => {
  const { $, calls } = fakeEngine({ files: { '/ws/USER.md': 'a\n' }, gate: allowVerdict })

  const out = await runTool($, 'memory_replace', { file: 'USER.md', old_text: 'zzz', new_text: 'b' })

  assert.match(out.deny, /not in the file/)
  assert.equal(calls.process.length, 0)
  assert.equal(calls.writes.length, 0)
})

test('memory_remove drops every line holding the text', async () => {
  const { $, files } = fakeEngine({
    files: { '/ws/memory/rules.md': 'keep\nstale rule\nkeep too\nstale again\n' },
    gate: allowVerdict,
  })

  const out = await runTool($, 'memory_remove', { file: 'memory/rules.md', text: 'stale' })

  assert.deepEqual(out, { result: 'updated memory/rules.md' })
  assert.equal(files['/ws/memory/rules.md'], 'keep\nkeep too\n')
})

test('an unknown tool name is refused', async () => {
  const { $ } = fakeEngine({ gate: allowVerdict })
  const out = await runTool($, 'memory_drop', { file: 'USER.md', text: 'x' })
  assert.match(out.deny, /unknown tool/)
})
