// CoS #68: the plugin is user-global, so a worker session sees all four write
// tools. Each one is refused for a caller that is not `cos`: the daemon names
// the caller from the socket peer and refuses, and the file stays as it was.

import { test } from 'node:test'
import assert from 'node:assert/strict'

import { runTool } from '../src/tools.js'
import { fakeEngine } from './fake.js'

const CALLS = {
  memory_add: { file: 'USER.md', text: 'likes tea' },
  memory_replace: { file: 'USER.md', old_text: 'old', new_text: 'new' },
  memory_remove: { file: 'USER.md', text: 'old' },
  memory_batch: { ops: [{ op: 'add', file: 'USER.md', text: 'likes tea' }] },
}

for (const [name, input] of Object.entries(CALLS)) {
  test(`${name} is refused for a caller that is not cos`, async () => {
    const { $, calls, files } = fakeEngine({
      files: { '/ws/USER.md': 'old\n' },
      caller: 'worker-1',
    })

    const out = await runTool($, name, input)

    assert.match(out.deny, /worker-1: only cos writes memory/)
    assert.equal(files['/ws/USER.md'], 'old\n')
    assert.equal(calls.verbWrites.length, 0)
    assert.equal(calls.writes.length, 0)
  })

  test(`${name} lands for cos, through the verb and never a direct write`, async () => {
    const { $, calls, files } = fakeEngine({ files: { '/ws/USER.md': 'old\n' } })

    const out = await runTool($, name, input)

    assert.equal(out.deny, undefined)
    assert.notEqual(files['/ws/USER.md'], 'old\n')
    assert.equal(calls.writes.length, 0)
    assert.deepEqual(calls.process.map(c => c.argv.slice(0, 3)), [['quaqua', 'call', 'memory.write']])
  })
}

test('a file that moved since the tool read it is refused, and the reply shows it as it stands', async () => {
  const { $, files } = fakeEngine({ files: { '/ws/USER.md': 'old\n' } })
  const read = $.fs.read
  let first = true
  $.fs.read = async path => {
    const text = await read(path)
    if (first) {
      first = false
      files[path] = 'old\nedited by hand\n'
    }
    return text
  }

  const out = await runTool($, 'memory_add', { file: 'USER.md', text: 'x' })

  assert.match(out.deny, /changed outside/)
  assert.match(out.deny, /edited by hand/)
  assert.equal(files['/ws/USER.md'], 'old\nedited by hand\n')
})
