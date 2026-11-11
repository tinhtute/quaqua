// Correction capture (R13): a prompt that corrects the assistant becomes one
// line in `memory/corrections.md`. The hook never blocks the prompt and never
// writes to stdout.

export const CORRECTION = /không phải|sai rồi|đừng|no, use|not that/i

/** One line, whatever the prompt held: the format is one correction per line. */
export function line(date, project, text) {
  const body = String(text ?? '')
    .replace(/\s+/g, ' ')
    .trim()
  return `- ${date} ${project} ${body}`
}

function lastLine(text) {
  const lines = text.split('\n').filter(l => l.trim() !== '')
  return lines.at(-1) ?? null
}

/**
 * Append one correction line, unless the file already ends with that same line.
 *
 * @returns `{ appended, line }`
 */
export async function capture($, { root, project, date, text }) {
  const entry = line(date, project, text)
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
