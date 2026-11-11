// The three capped memory tools. Every write goes through `memory-gate.js`
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

/**
 * Apply one memory operation.
 *
 * @returns `{ result }` on success or `{ deny }` with the engine's reason.
 */
export async function apply($, { root, op, file, input }) {
  if (!FILES.includes(file)) return { deny: `not a capped memory file: ${file}` }

  const path = `${root}/${file}`
  const current = await readOrEmpty($, path)
  const step = compute(op, current, input)
  if (step.refuse) return { deny: step.refuse }

  const verdict = await gate($, { root, rel: file, content: step.next })
  if (!verdict.ok) return { deny: verdict.reason }

  await $.fs.write(path, step.next)
  return { result: `updated ${file}` }
}

/**
 * Serve one `tool.call` for this mod.
 *
 * @param name the short tool name (`memory_add`, …)
 * @param input the call's arguments
 */
export async function runTool($, name, input) {
  const spec = TOOLS.find(t => t.name === name)
  if (!spec) return { deny: `unknown tool ${name}` }
  const root = await $.session.root()
  const op = name.slice('memory_'.length)
  return apply($, { root, op, file: String(input?.file ?? ''), input: input ?? {} })
}
