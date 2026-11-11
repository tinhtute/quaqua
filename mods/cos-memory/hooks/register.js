// The mod's entry point: `hooks/hooks.json` names this module, the engine calls
// `register(on, options)` once, and every hook below is wired from here.
//
// Nothing in this file decides what memory may hold: the engine does, through
// `quaqua memory hook pre` (see `src/memory-gate.js`).

import { TOOLS, runTool } from '../src/tools.js'
import {
  load,
  save,
  shouldReview,
  noteAttempt,
  noteReview,
  noteVeto,
  noteMiss,
  memoryStamp,
  today,
} from '../src/counter.js'
import { runReview } from '../src/review.js'
import { CORRECTION, capture } from '../src/corrections.js'

function projectOf(root) {
  return String(root).split('/').filter(Boolean).at(-1) ?? 'unknown'
}

export const register = on => {
  // The first `session.start` is awaited, so the tools are listed by turn one.
  on('session.start', async ($, e, next) => {
    const r = await next(e)
    for (const spec of TOOLS) await $.tool.register(spec)
    return r
  })

  // One catch-all: count the call, then serve this mod's own tools.
  on('tool.call', async ($, e, next) => {
    const plugin = $.plugin.name
    const prefix = `mcp__${plugin}__`
    const tool = String(e?.tool ?? '')

    try {
      const state = await load($)
      await save($, { ...state, toolCalls: state.toolCalls + 1 })
    } catch {
      // A counter that cannot be written must never stop a tool call.
    }

    if (!tool.startsWith(prefix)) return next(e)
    return runTool($, tool.slice(prefix.length), e)
  })

  // The review nudge: count the Stop, then decide. The fork itself runs after
  // this hook returns, so a slow model never eats the hook's budget.
  on('classic.Stop', async ($, e, next) => {
    const r = await next(e)
    const root = await $.session.root()
    const date = today()

    const state = await load($)
    await save($, { ...state, stops: state.stops + 1 })

    const { fire, vetoed, stamp } = await shouldReview($, {
      stopHookActive: e?.stop_hook_active,
      root,
      date,
    })

    // Memory moved since the last review: the agent already did the work this
    // review would ask for. Advance the baseline so the next window is judged
    // from here, not from a state that will never come back.
    if (vetoed) {
      await noteVeto($, await load($), stamp)
      return r
    }
    if (!fire) return r

    await noteAttempt($, await load($))
    void (async () => {
      try {
        const out = await runReview($, { root })
        if (out.reviewed) await noteReview($, await memoryStamp($, root, date))
        else await noteMiss($, await load($))
      } catch {
        // A review that throws must still release its marker, or the trigger
        // waits out the stale window for nothing.
        try {
          await noteMiss($, await load($))
        } catch {
          // The store is gone too; the stale marker ages out on its own.
        }
      }
    })()
    return r
  })

  // Correction capture: append, never block, never print.
  on('prompt.submit', async ($, e, next) => {
    try {
      if (e?.isMeta !== true) {
        const state = await load($)
        await save($, { ...state, prompts: state.prompts + 1 })
      }
    } catch {
      // A counter that cannot be written must never block a prompt.
    }

    try {
      if (CORRECTION.test(String(e?.text ?? ''))) {
        const root = await $.session.root()
        await capture($, {
          root,
          project: projectOf(root),
          date: today(),
          text: e.text,
        })
      }
    } catch {
      // A capture that fails is a missed line, never a blocked prompt.
    }
    return next(e)
  })
}
