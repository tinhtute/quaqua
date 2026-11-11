// Correction capture (R13): a prompt that corrects the assistant becomes one
// line in `memory/corrections.md`. The hook never blocks the prompt and never
// writes to stdout.
//
// What may land in that line is not decided here: the engine screens it
// (`quaqua memory scan`, see `src/memory-gate.js`), refusing a secret and
// cutting a long prompt to its own bound. A line the engine refuses is a missed
// line, never a blocked prompt.

import { screen } from './memory-gate.js'

export const CORRECTION = /không phải|sai rồi|đừng|no, use|not that/i

/** One line, whatever the prompt held: the format is one correction per line. */
export function oneLine(text) {
  return String(text ?? '')
    .replace(/\s+/g, ' ')
    .trim()
}

/** The `memory/corrections.md` entry for one screened body. */
export function line(date, project, body) {
  return `- ${date} ${project} ${body}`
}

function lastLine(text) {
  const lines = text.split('\n').filter(l => l.trim() !== '')
  return lines.at(-1) ?? null
}

/**
 * Append one correction line, unless the file already ends with that same line.
 *
 * @returns `{ appended, line }`, or `{ appended: false, line: null, refused }`
 *   when the engine refuses the text.
 */
export async function capture($, { root, project, date, text }) {
  const screened = await screen($, { root, text: oneLine(text) })
  if (!screened.ok) return { appended: false, line: null, refused: screened.reason }

  const entry = line(date, project, screened.text)
  const path = `${root}/memory/corrections.md`

  let current = ''
  try {
    current = String(await $.fs.read(path))
  } catch {
    current = ''
  }

  if (lastLine(current) === entry) return { appended: false, line: entry }

  const base = current === '' || current.endsWith('\n') ? current : `${current}\n`
  await $.fs.write(path, `${base}${entry}\n`)
  return { appended: true, line: entry }
}
