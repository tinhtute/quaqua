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

  assert.deepEqual(out, { deny: `${ENGINE_REASON}\n\n--- current memory/rules.md (4 chars) ---\nold\n` })
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

test('memory_replace writes new_text literally, dollar patterns included', async () => {
  const { $, files } = fakeEngine({ files: { '/ws/USER.md': 'a\n' }, gate: allowVerdict })

  const out = await runTool($, 'memory_replace', {
    file: 'USER.md',
    old_text: 'a',
    new_text: 'cost $& x',
  })

  assert.deepEqual(out, { result: 'updated USER.md' })
  assert.equal(files['/ws/USER.md'], 'cost $& x\n')
})

test('an unknown tool name is refused', async () => {
  const { $ } = fakeEngine({ gate: allowVerdict })
  const out = await runTool($, 'memory_drop', { file: 'USER.md', text: 'x' })
  assert.match(out.deny, /unknown tool/)
})

// A gate that judges the content like the engine: over 20 chars is over the cap.
function capGate($) {
  const run = $.process.run
  $.process.run = async (argv, init) => {
    await run(argv, init)
    const { content } = JSON.parse(init.stdin).tool_input
    if ([...content].length <= 20) return { exitCode: 0, stdout: '', stderr: '' }
    const reason = `${[...content].length} chars > 20 cap`
    return {
      exitCode: 0,
      stdout: JSON.stringify({
        hookSpecificOutput: { permissionDecision: 'deny', permissionDecisionReason: reason },
      }),
      stderr: '',
    }
  }
  return $
}

test('a batch is checked on the final state: a drop that makes room lets the add land', async () => {
  const { $, files, calls } = fakeEngine({
    files: { '/ws/memory/rules.md': 'aaaaaaaaaaaaaaaa\nstale\n' },
  })
  capGate($)

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
  })
  capGate($)

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
  const { $, calls } = fakeEngine({ files: { '/ws/USER.md': 'a\n' }, gate: allowVerdict })

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
    gate: denyVerdict('USER.md contains a token'),
  })

  const out = await runTool($, 'memory_batch', { ops: [{ op: 'add', file: 'USER.md', text: 'k' }] })

  assert.match(out.deny, /contains a token/)
  assert.equal(calls.writes.length, 0)
})

test('a batch write that fails halfway restores the earlier file', async () => {
  const { $, files } = fakeEngine({
    files: { '/ws/memory/rules.md': 'r\n', '/ws/USER.md': 'u\n' },
    gate: allowVerdict,
  })
  const write = $.fs.write
  $.fs.write = async (path, text) => {
    if (path.endsWith('USER.md') && text !== 'u\n') throw new Error('EACCES')
    return write(path, text)
  }

  const out = await runTool($, 'memory_batch', {
    ops: [
      { op: 'add', file: 'memory/rules.md', text: 'x' },
      { op: 'add', file: 'USER.md', text: 'y' },
    ],
  })

  assert.match(out.deny, /write failed, nothing kept/)
  assert.equal(files['/ws/memory/rules.md'], 'r\n')
})

test('an empty or foreign-file batch is refused', async () => {
  const { $, calls } = fakeEngine({ gate: allowVerdict })
  assert.match((await runTool($, 'memory_batch', { ops: [] })).deny, /empty/)
  const out = await runTool($, 'memory_batch', { ops: [{ op: 'add', file: 'SOUL.md', text: 'x' }] })
  assert.match(out.deny, /not a capped memory file/)
  assert.equal(calls.process.length, 0)
})

test('a single refused write also returns the file as it stands', async () => {
  const { $ } = fakeEngine({
    files: { '/ws/USER.md': 'mine\n' },
    gate: denyVerdict('over cap'),
  })
  const out = await runTool($, 'memory_add', { file: 'USER.md', text: 'x' })
  assert.equal(out.deny, 'over cap\n\n--- current USER.md (5 chars) ---\nmine\n')
})
