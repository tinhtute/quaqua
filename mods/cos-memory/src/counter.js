// The review trigger (R12): a counter in `$.store`, a memory-unchanged guard,
// and a `stop_hook_active` veto. Nothing here knows a memory cap — those live in
// the engine. The numbers below are R12's review thresholds, not caps.

const KEY = 'review'
const TURNS_BEFORE_REVIEW = 10
const TOOL_CALLS_BEFORE_REVIEW = 15

// A review that never reported back (the process died mid-fork) must not wedge
// the trigger: past this age its marker is stale and the next Stop may retry.
const INFLIGHT_MS = 5 * 60 * 1000

const EMPTY = { turns: 0, toolCalls: 0, lastReviewAt: null, stamp: null, inflightAt: null }

export async function load($) {
  const held = await $.store.get(KEY)
  return held && typeof held === 'object' ? { ...EMPTY, ...held } : { ...EMPTY }
}

export async function save($, state) {
  await $.store.set(KEY, state)
}

/** `YYYY-MM-DD` in the session's own timezone. */
export function today(now = new Date()) {
  const pad = n => String(n).padStart(2, '0')
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`
}

async function statOrNull($, path) {
  try {
    const s = await $.fs.stat(path)
    return { mtimeMs: s.mtimeMs, size: s.size }
  } catch {
    return null
  }
}

/**
 * What the two guarded files look like now: a change stamp, not a content read
 * and not a length of anything the engine caps.
 */
export async function memoryStamp($, root, date) {
  return {
    rules: await statOrNull($, `${root}/memory/rules.md`),
    today: await statOrNull($, `${root}/memory/${date}.md`),
  }
}

/** Two stamps name the same state. An absent baseline counts as unchanged. */
export function unchanged(before, now) {
  if (before === null || before === undefined) return true
  return JSON.stringify(before) === JSON.stringify(now)
}

/**
 * Decide whether this Stop should start a review, without changing anything.
 *
 * @returns `{ fire, state, stamp }` — the caller saves `state` itself.
 */
export async function shouldReview($, { stopHookActive, root, date, now = Date.now() }) {
  const state = await load($)
  const stamp = await memoryStamp($, root, date)
  const busy = state.inflightAt !== null && now - state.inflightAt < INFLIGHT_MS
  const due = state.turns >= TURNS_BEFORE_REVIEW || state.toolCalls >= TOOL_CALLS_BEFORE_REVIEW
  const fire = !busy && stopHookActive !== true && due && unchanged(state.stamp, stamp)
  return { fire, state, stamp }
}

/** Mark a review as started, so a second Stop does not start another. */
export async function noteAttempt($, state, now = Date.now()) {
  await save($, { ...state, inflightAt: now })
}

/** The review reported back: counters reset, baseline moved, marker cleared. */
export async function noteReview($, stamp, now = Date.now()) {
  await save($, { turns: 0, toolCalls: 0, lastReviewAt: now, stamp, inflightAt: null })
}

/** The review did not answer: counters stay, the marker clears so a retry can fire. */
export async function noteMiss($, state) {
  await save($, { ...state, inflightAt: null })
}
