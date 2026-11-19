// The caps and the secret patterns live in the engine
// (`crates/qq-core/src/memory`, `TARGETS` and `check_text`). This module never
// restates them: capped writes go to the daemon verb `memory.write` and its
// verdict is relayed; a corrections line goes to `quaqua memory scan`. No cap
// number, no pattern, no length here.

/** The files the engine caps, workspace-relative. */
export const FILES = ['memory/rules.md', 'USER.md']

/**
 * Write capped files through the daemon: `quaqua call memory.write`. The daemon
 * names the caller from the socket peer (this process is a child of the
 * session), refuses any caller but `cos`, checks the cap and the strict threat
 * scan on each final text, guards against an outside edit since `before`, journals
 * and writes. This mod never writes those files itself.
 *
 * @param files `{ [rel]: { before, after } }`: the text loaded and the text wanted
 * @returns `{ ok: true }` or `{ ok: false, reason }` with the daemon's own reason.
 *   A daemon that cannot answer — a missing binary, a daemon that is down, an
 *   unreadable reply — is a refusal, never an allow.
 */
export async function write($, { root, files }) {
  let out
  try {
    out = await $.process.run(['quaqua', 'call', 'memory.write', JSON.stringify({ files })], {
      cwd: root,
    })
  } catch (e) {
    return { ok: false, reason: `memory write unavailable: ${e?.message ?? e}` }
  }

  if (out.exitCode !== 0) {
    const why = String(out.stderr ?? '').trim().replace(/^quaqua: /, '')
    return { ok: false, reason: why || `memory write failed (exit ${out.exitCode})` }
  }

  let doc
  try {
    doc = JSON.parse(String(out.stdout ?? '').trim())
  } catch {
    return { ok: false, reason: 'memory write returned no verdict' }
  }
  if (!Array.isArray(doc?.written)) return { ok: false, reason: 'memory write returned no verdict' }
  return { ok: true }
}

/**
 * Ask the engine to screen one line for a memory file that is not itself
 * capped (`memory/corrections.md`): it refuses a secret and cuts a long line to
 * its own bound.
 *
 * @returns `{ ok: true, text }` with the engine's bounded text, else
 *   `{ ok: false, reason }` with the engine's own reason. A screen that cannot
 *   answer — a missing binary, a non-zero exit, an unreadable verdict — is a
 *   refusal, never an allow: an unscreened line could carry a secret.
 */
export async function screen($, { root, text }) {
  let out
  try {
    out = await $.process.run(['quaqua', 'memory', 'scan'], {
      cwd: root,
      stdin: String(text ?? ''),
    })
  } catch (e) {
    return { ok: false, reason: `memory scan unavailable: ${e?.message ?? e}` }
  }

  if (out.exitCode !== 0) {
    const why = String(out.stderr ?? '').trim()
    return {
      ok: false,
      reason: `memory scan failed (exit ${out.exitCode})${why ? `: ${why}` : ''}`,
    }
  }

  let doc
  try {
    doc = JSON.parse(String(out.stdout ?? '').trim())
  } catch {
    return { ok: false, reason: 'memory scan returned no verdict' }
  }

  if (doc?.ok === true && typeof doc.text === 'string') {
    return { ok: true, text: doc.text }
  }
  return { ok: false, reason: String(doc?.reason ?? 'refused') }
}
