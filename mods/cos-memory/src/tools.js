// The four capped memory tools. Every write goes through `memory-gate.js`
// first, so the engine's cap and secret check is the only authority on what
// lands; a refusal comes back as the tool's error text, verbatim.

import { FILES, gate } from './memory-gate.js'

const FILE_SCHEMA = { type: 'string', enum: FILES }

export const TOOLS = [
  {
    name: 'memory_add',
    description:
      'Append a line or section to a capped memory file. The engine refuses an over-cap or secret-bearing result; its reason is the error.',
    inputSchema: {
      type: 'object',
      properties: {
        file: FILE_SCHEMA,
        text: { type: 'string', description: 'What to append.' },
      },
      required: ['file', 'text'],
    },
  },
  {
    name: 'memory_replace',
    description:
      'Replace the first occurrence of old_text with new_text in a capped memory file. The engine refuses an over-cap or secret-bearing result; its reason is the error.',
    inputSchema: {
      type: 'object',
      properties: {
        file: FILE_SCHEMA,
        old_text: { type: 'string', description: 'Text that must already be there.' },
        new_text: { type: 'string', description: 'What replaces it.' },
      },
      required: ['file', 'old_text', 'new_text'],
    },
  },
  {
    name: 'memory_remove',
    description:
      'Remove every line containing text from a capped memory file. The engine refuses an over-cap or secret-bearing result; its reason is the error.',
    inputSchema: {
      type: 'object',
      properties: {
        file: FILE_SCHEMA,
        text: { type: 'string', description: 'A fragment of the lines to drop.' },
      },
      required: ['file', 'text'],
    },
  },
  {
    name: 'memory_batch',
    description:
      'Apply several add/replace/remove operations across the capped memory files as one write. Operations run in order on a copy; the engine checks the cap and secrets on the final state of each file, and any refusal writes nothing. A refusal returns each touched file\'s current content with the reason, so one batch can consolidate.',
    inputSchema: {
      type: 'object',
      properties: {
        ops: {
          type: 'array',
          minItems: 1,
          items: {
            type: 'object',
            properties: {
              op: { type: 'string', enum: ['add', 'replace', 'remove'] },
              file: FILE_SCHEMA,
              text: { type: 'string', description: 'add: what to append. remove: a fragment of the lines to drop.' },
              old_text: { type: 'string', description: 'replace: text that must already be there.' },
              new_text: { type: 'string', description: 'replace: what replaces it.' },
            },
            required: ['op', 'file'],
          },
        },
      },
      required: ['ops'],
    },
  },
]

/** The tool names this mod serves, as the model calls them. */
export function toolNames(plugin) {
  return TOOLS.map(t => `mcp__${plugin}__${t.name}`)
}

async function readOrEmpty($, path) {
  try {
    return String(await $.fs.read(path))
  } catch {
    return ''
  }
}

/**
 * The next content a call would leave behind, or `{ refuse }` when the call
 * cannot be applied at all (an `old_text` that is not there).
 */
export function compute(kind, current, input) {
  if (kind === 'add') {
    const text = String(input.text ?? '')
    if (text === '') return { refuse: 'text is empty' }
    const base = current === '' || current.endsWith('\n') ? current : `${current}\n`
    return { next: `${base}${text}\n` }
  }
  if (kind === 'replace') {
    const oldText = String(input.old_text ?? '')
    const newText = String(input.new_text ?? '')
    if (oldText === '') return { refuse: 'old_text is empty' }
    if (!current.includes(oldText)) return { refuse: 'old_text is not in the file' }
    // A function replacer: `$&`, `$'`, `` $` `` and `$$` in new_text stay text.
    return { next: current.replace(oldText, () => newText) }
  }
  if (kind === 'remove') {
    const text = String(input.text ?? '')
    if (text === '') return { refuse: 'text is empty' }
    const lines = current.split('\n')
    const kept = lines.filter(line => !line.includes(text))
    if (kept.length === lines.length) return { refuse: 'no line contains that text' }
    return { next: kept.join('\n') }
  }
  return { refuse: `unknown operation ${kind}` }
}

/** A refusal's text: the reason, then the file's current content, so the caller can retry from it. */
function withCurrent(reason, files) {
  const shown = files.map(
    ([file, text]) => `--- current ${file} (${[...text].length} chars) ---\n${text}`,
  )
  return [reason, ...shown].join('\n\n')
}

/**
 * Apply one memory operation.
 *
 * @returns `{ result }` on success or `{ deny, current }`: the reason and the
 *   file's text as it stands (absent for a file this mod does not serve).
 */
export async function apply($, { root, op, file, input }) {
  if (!FILES.includes(file)) return { deny: `not a capped memory file: ${file}` }

  const path = `${root}/${file}`
  const current = await readOrEmpty($, path)
  const step = compute(op, current, input)
  if (step.refuse) return { deny: step.refuse, current }

  const verdict = await gate($, { root, rel: file, content: step.next })
  if (!verdict.ok) return { deny: verdict.reason, current }

  await $.fs.write(path, step.next)
  return { result: `updated ${file}` }
}

/**
 * Apply a list of operations as one write: every file's final text is
 * simulated first, the engine checks each final text, and nothing is written
 * unless all pass. A failed write rolls the earlier ones back.
 *
 * @returns `{ result }` or `{ deny, current }` where `current` maps each
 *   touched file to its text before the batch.
 */
export async function applyBatch($, { root, ops }) {
  if (!Array.isArray(ops) || ops.length === 0) return { deny: 'ops is empty' }
  const before = {}
  const next = {}
  for (const [i, o] of ops.entries()) {
    const file = String(o?.file ?? '')
    if (!FILES.includes(file)) return { deny: `ops[${i}]: not a capped memory file: ${file}` }
    if (!(file in before)) {
      before[file] = await readOrEmpty($, `${root}/${file}`)
      next[file] = before[file]
    }
  }
  const touched = Object.entries(before)
  for (const [i, o] of ops.entries()) {
    const step = compute(String(o.op ?? ''), next[o.file], o)
    if (step.refuse) return { deny: `ops[${i}]: ${step.refuse}`, current: touched }
    next[o.file] = step.next
  }
  const reasons = []
  for (const [file] of touched) {
    const verdict = await gate($, { root, rel: file, content: next[file] })
    if (!verdict.ok) reasons.push(verdict.reason)
  }
  if (reasons.length) return { deny: reasons.join('; '), current: touched }

  const done = []
  try {
    for (const [file] of touched) {
      await $.fs.write(`${root}/${file}`, next[file])
      done.push(file)
    }
  } catch (e) {
    for (const file of done) {
      try {
        await $.fs.write(`${root}/${file}`, before[file])
      } catch {
        // Nothing more can be done; the error below names the failure.
      }
    }
    return { deny: `write failed, nothing kept: ${e?.message ?? e}`, current: touched }
  }
  return { result: `updated ${touched.map(([f]) => f).join(', ')}` }
}

/**
 * Serve one `tool.call` for this mod. A refusal is the tool's error text: the
 * reason, then the file's current content (the one error channel there is).
 *
 * @param name the short tool name (`memory_add`, …)
 * @param input the call's arguments
 */
export async function runTool($, name, input) {
  const spec = TOOLS.find(t => t.name === name)
  if (!spec) return { deny: `unknown tool ${name}` }
  const root = await $.session.root()
  let out
  if (name === 'memory_batch') {
    out = await applyBatch($, { root, ops: input?.ops })
  } else {
    const op = name.slice('memory_'.length)
    const file = String(input?.file ?? '')
    out = await apply($, { root, op, file, input: input ?? {} })
    if (out.current !== undefined) out = { ...out, current: [[file, out.current]] }
  }
  if (out.deny === undefined) return out
  return { deny: withCurrent(out.deny, out.current ?? []) }
}
