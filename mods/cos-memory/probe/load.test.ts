// T1 probe: proves the installed engine loads `hooks/register.js` and serves the
// mod's tools. Run it with `claude plugin test mods/cos-memory`.
//
// The call below is answered by this mod's own `tool.call` hook, before any
// process runs: a file outside the capped pair is refused by the mod itself. If
// the module had not loaded, the tool would not exist at all.

import { test, expect } from 'claude-code/testing'

test('the mod loads and serves memory_add', async ($, on) => {
  on('session.root', () => ({ value: '/ws' }))

  const r = await $.tool.call({
    tool: 'mcp__cos-memory__memory_add',
    file: 'memory/corrections.md',
    text: 'x',
  })

  expect('deny' in r && r.deny).toContain('not a capped memory file')
})
