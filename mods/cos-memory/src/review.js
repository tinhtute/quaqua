// The periodic review fork (R12). `$.model.fork` cannot restrict its toolset
// (ModelForkRequest is `{ prompt, effort? }`), so the restriction is enforced
// here: the fork is asked for a JSON array of memory operations and told to
// call no tools, and only operations matching that schema are applied. One reply
// is one all-or-nothing batch checked on the final state, like `memory_batch`.

import { FILES } from './memory-gate.js'
import { applyBatch } from './tools.js'

export const REVIEW_PROMPT = `Review this session for memory worth keeping.

Answer with a JSON array and nothing else: no prose, no code fence, no tool calls.
Each element is one of:
  {"op":"add","file":"memory/rules.md","text":"..."}
  {"op":"replace","file":"memory/rules.md","old_text":"...","new_text":"..."}
  {"op":"remove","file":"memory/rules.md","text":"..."}
"file" is "memory/rules.md" or "USER.md" and nothing else. "op" is "add",
"replace" or "remove" and nothing else.

Put a durable rule in memory/rules.md only when it will still be true next week.
Put a durable fact about the person in USER.md. Skip anything already there.
Answer [] when nothing is worth keeping.`

const OPS = new Set(['add', 'replace', 'remove'])

/** The JSON array a reply holds, or `null` when it holds none. */
export function parseOps(text) {
  if (typeof text !== 'string') return null
  const start = text.indexOf('[')
  const end = text.lastIndexOf(']')
  if (start === -1 || end <= start) return null
  let doc
  try {
    doc = JSON.parse(text.slice(start, end + 1))
  } catch {
    return null
  }
  return Array.isArray(doc) ? doc : null
}

/** The operations this mod will act on: the schema's, and no others. */
export function accepted(ops) {
  return ops.filter(
    op =>
      op !== null &&
      typeof op === 'object' &&
      OPS.has(op.op) &&
      FILES.includes(op.file) &&
      typeof (op.op === 'replace' ? op.old_text : op.text) === 'string',
  )
}

/**
 * Run one review and apply what it asks for as one batch: a refusal writes
 * nothing.
 *
 * @returns `{ reviewed, applied, reason? }` — `reviewed: false` means the fork
 *   did not answer or its reply held no array, and the caller must not reset
 *   its counters.
 */
export async function runReview($, { root }) {
  let result
  try {
    result = await $.model.fork({ prompt: REVIEW_PROMPT, effort: 'low' })
  } catch (e) {
    return { reviewed: false, applied: 0, reason: `fork failed: ${e?.message ?? e}` }
  }

  if (result?.isAnswered !== true) {
    return { reviewed: false, applied: 0, reason: result?.reason ?? 'no answer' }
  }

  const ops = parseOps(result.text)
  if (ops === null) return { reviewed: false, applied: 0, reason: 'reply held no array' }

  const batch = accepted(ops)
  if (batch.length === 0) return { reviewed: true, applied: 0, refused: [] }
  const out = await applyBatch($, { root, ops: batch })
  const applied = 'result' in out ? batch.length : 0
  const refused = 'result' in out ? [] : [out.deny]

  return { reviewed: true, applied, refused }
}
