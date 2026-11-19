import { test } from 'node:test'
import assert from 'node:assert/strict'

import { runTool } from '../src/tools.js'
import { fakeEngine, denyVerdict, allowVerdict } from './fake.js'

const ENGINE_REASON = "memory/rules.md is 2400 chars, over the 2200 cap (AGENTS.md '## Memory')"

test('an engine refusal is the tool error, verbatim, and nothing is written', async () => {
  const { $, calls, files } = fakeEngine({
    files: { '/ws/memory/rules.md': 'old\n' },
    verb: denyVerdict(ENGINE_REASON),
  })

  const out = await runTool($, 'memory_add', { file: 'memory/rules.md', text: 'new rule' })

  assert.deepEqual(out, { deny: `refused: ${ENGINE_REASON}\n\n--- current memory/rules.md (4 chars) ---\nold\n` })
  assert.equal(files['/ws/memory/rules.md'], 'old\n')
  assert.equal(calls.writes.length, 0)
})

test('an allowed write lands and reports the file it touched', async () => {
  const { $, files } = fakeEngine({
    files: { '/ws/memory/rules.md': 'old\n' },
    verb: allowVerdict,
  })

  const out = await runTool($, 'memory_add', { file: 'memory/rules.md', text: 'new rule' })

  assert.deepEqual(out, { result: 'updated memory/rules.md' })
  assert.equal(files['/ws/memory/rules.md'], 'old\nnew rule\n')
})

test('the daemon verb is asked with what was loaded and the whole result', async () => {
  const { $, calls } = fakeEngine({ files: { '/ws/USER.md': 'a\n' } })

  await runTool($, 'memory_replace', { file: 'USER.md', old_text: 'a', new_text: 'b' })

  assert.equal(calls.process.length, 1)
  const [call] = calls.process
  assert.deepEqual(call.argv.slice(0, 3), ['quaqua', 'call', 'memory.write'])
  assert.equal(call.init.cwd, '/ws')
  assert.deepEqual(JSON.parse(call.argv[3]), { files: { 'USER.md': { before: 'a\n', after: 'b\n' } } })
  assert.equal(calls.writes.length, 0, 'the mod never writes the file itself')
})

test('a daemon that cannot be reached is a refusal, not a silent allow', async () => {
  const { $, calls } = fakeEngine({
    files: { '/ws/memory/rules.md': '' },
    verb: Object.assign(new Error('spawn quaqua ENOENT'), { code: 'ENOENT' }),
  })

  const out = await runTool($, 'memory_add', { file: 'memory/rules.md', text: 'x' })

  assert.match(out.deny, /unavailable/)
  assert.equal(calls.writes.length, 0)
})

test('a verb that exits non-zero without a reason is a refusal', async () => {
  const { $, calls } = fakeEngine({
    files: { '/ws/memory/rules.md': '' },
    verb: { exitCode: 2, stdout: '', stderr: '' },
  })

  const out = await runTool($, 'memory_add', { file: 'memory/rules.md', text: 'x' })

  assert.match(out.deny, /failed \(exit 2\)/)
  assert.equal(calls.writes.length, 0)
})

test('a verdict that cannot be parsed is a refusal', async () => {
  const { $, calls } = fakeEngine({
    files: { '/ws/memory/rules.md': '' },
    verb: { exitCode: 0, stdout: 'not json', stderr: '' },
  })

  const out = await runTool($, 'memory_add', { file: 'memory/rules.md', text: 'x' })

  assert.match(out.deny, /no verdict/)
  assert.equal(calls.writes.length, 0)
})

test('a file outside the capped pair is refused without asking the engine', async () => {
  const { $, calls } = fakeEngine({ verb: allowVerdict })

  const out = await runTool($, 'memory_add', { file: 'memory/corrections.md', text: 'x' })

  assert.match(out.deny, /not a capped memory file/)
  assert.equal(calls.process.length, 0)
  assert.equal(calls.writes.length, 0)
})

test('memory_replace refuses an old_text that is not there', async () => {
  const { $, calls } = fakeEngine({ files: { '/ws/USER.md': 'a\n' }, verb: allowVerdict })

  const out = await runTool($, 'memory_replace', { file: 'USER.md', old_text: 'zzz', new_text: 'b' })

  assert.match(out.deny, /not in the file/)
  assert.equal(calls.process.length, 0)
  assert.equal(calls.writes.length, 0)
})

test('memory_remove drops every line holding the text', async () => {
  const { $, files } = fakeEngine({
    files: { '/ws/memory/rules.md': 'keep\nstale rule\nkeep too\nstale again\n' },
    verb: allowVerdict,
  })

  const out = await runTool($, 'memory_remove', { file: 'memory/rules.md', text: 'stale' })

  assert.deepEqual(out, { result: 'updated memory/rules.md' })
  assert.equal(files['/ws/memory/rules.md'], 'keep\nkeep too\n')
})

test('memory_replace writes new_text literally, dollar patterns included', async () => {
  const { $, files } = fakeEngine({ files: { '/ws/USER.md': 'a\n' }, verb: allowVerdict })

  const out = await runTool($, 'memory_replace', {
    file: 'USER.md',
    old_text: 'a',
    new_text: 'cost $& x',
  })

  assert.deepEqual(out, { result: 'updated USER.md' })
  assert.equal(files['/ws/USER.md'], 'cost $& x\n')
})

test('an unknown tool name is refused', async () => {
  const { $ } = fakeEngine({ verb: allowVerdict })
  const out = await runTool($, 'memory_drop', { file: 'USER.md', text: 'x' })
  assert.match(out.deny, /unknown tool/)
})

test('a batch is checked on the final state: a drop that makes room lets the add land', async () => {
  const { $, files, calls } = fakeEngine({
    files: { '/ws/memory/rules.md': 'aaaaaaaaaaaaaaaa\nstale\n' },
    cap: 20,
  })

  const out = await runTool($, 'memory_batch', {
    ops: [
      { op: 'remove', file: 'memory/rules.md', text: 'aaaa' },
      { op: 'add', file: 'memory/rules.md', text: 'new rule' },
    ],
  })

  assert.deepEqual(out, { result: 'updated memory/rules.md' })
  assert.equal(files['/ws/memory/rules.md'], 'stale\nnew rule\n')
  assert.equal(calls.process.length, 1, 'one check on the final text, not one per op')
})

test('a batch over the cap writes nothing, in any file, and returns current content', async () => {
  const { $, files, calls } = fakeEngine({
    files: { '/ws/memory/rules.md': 'keep\n', '/ws/USER.md': 'user\n' },
    cap: 20,
  })

  const out = await runTool($, 'memory_batch', {
    ops: [
      { op: 'add', file: 'USER.md', text: 'fine' },
      { op: 'add', file: 'memory/rules.md', text: 'x'.repeat(30) },
    ],
  })

  assert.match(out.deny, /36 chars > 20 cap/)
  assert.match(out.deny, /--- current memory\/rules\.md \(5 chars\) ---\nkeep\n/)
  assert.match(out.deny, /--- current USER\.md \(5 chars\) ---\nuser\n/)
  assert.equal(calls.writes.length, 0)
  assert.equal(files['/ws/USER.md'], 'user\n')
})

test('a batch with an op that cannot apply writes nothing', async () => {
  const { $, calls } = fakeEngine({ files: { '/ws/USER.md': 'a\n' }, verb: allowVerdict })

  const out = await runTool($, 'memory_batch', {
    ops: [
      { op: 'add', file: 'USER.md', text: 'b' },
      { op: 'replace', file: 'USER.md', old_text: 'zzz', new_text: 'c' },
    ],
  })

  assert.match(out.deny, /ops\[1\]: old_text is not in the file/)
  assert.match(out.deny, /--- current USER\.md \(2 chars\) ---\na\n/)
  assert.equal(calls.process.length, 0)
  assert.equal(calls.writes.length, 0)
})

test('a batch with a secret verdict is refused as a whole', async () => {
  const { $, calls } = fakeEngine({
    files: { '/ws/USER.md': 'a\n' },
    verb: denyVerdict('USER.md contains a token'),
  })

  const out = await runTool($, 'memory_batch', { ops: [{ op: 'add', file: 'USER.md', text: 'k' }] })

  assert.match(out.deny, /contains a token/)
  assert.equal(calls.writes.length, 0)
})

test('an empty or foreign-file batch is refused', async () => {
  const { $, calls } = fakeEngine({ verb: allowVerdict })
  assert.match((await runTool($, 'memory_batch', { ops: [] })).deny, /empty/)
  const out = await runTool($, 'memory_batch', { ops: [{ op: 'add', file: 'SOUL.md', text: 'x' }] })
  assert.match(out.deny, /not a capped memory file/)
  assert.equal(calls.process.length, 0)
})

test('a single refused write also returns the file as it stands', async () => {
  const { $ } = fakeEngine({
    files: { '/ws/USER.md': 'mine\n' },
    verb: denyVerdict('over cap'),
  })
  const out = await runTool($, 'memory_add', { file: 'USER.md', text: 'x' })
  assert.equal(out.deny, 'refused: over cap\n\n--- current USER.md (5 chars) ---\nmine\n')
})
