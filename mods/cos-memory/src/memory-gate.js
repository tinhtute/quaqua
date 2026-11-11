// The caps and the secret patterns live in the engine
// (`crates/quaqua/src/memory.rs`, `TARGETS` and `check_text`). This module never
// restates them: it pipes a synthetic PreToolUse payload to `quaqua memory hook
// pre` and relays the verdict. No cap number, no pattern, no length here.

/** The files the engine caps, workspace-relative. */
export const FILES = ['memory/rules.md', 'USER.md']

/**
 * Ask the engine whether `content` may land in `<root>/<rel>`.
 *
 * @returns `{ ok: true }` when the engine allows it, else `{ ok: false, reason }`
 *   with the engine's own reason verbatim. A gate that cannot answer — a missing
 *   binary, a non-zero exit, an unreadable verdict — is a refusal, never an allow.
 */
export async function gate($, { root, rel, content }) {
  const payload = JSON.stringify({
    tool_name: 'Write',
    tool_input: { file_path: `${root}/${rel}`, content },
  })

  let out
  try {
    out = await $.process.run(['quaqua', 'memory', 'hook', 'pre'], {
      cwd: root,
      stdin: payload,
    })
  } catch (e) {
    return { ok: false, reason: `memory gate unavailable: ${e?.message ?? e}` }
  }

  if (out.exitCode !== 0) {
    const why = String(out.stderr ?? '').trim()
    return {
      ok: false,
      reason: `memory gate failed (exit ${out.exitCode})${why ? `: ${why}` : ''}`,
    }
  }

  // The engine prints nothing when it has no objection.
  const stdout = String(out.stdout ?? '').trim()
  if (stdout === '') return { ok: true }

  let doc
  try {
    doc = JSON.parse(stdout)
  } catch {
    return { ok: false, reason: 'memory gate returned no verdict' }
  }

  const decision = doc?.hookSpecificOutput
  if (decision?.permissionDecision === 'deny') {
    return { ok: false, reason: String(decision.permissionDecisionReason ?? 'denied') }
  }
  return { ok: true }
}
