// A stand-in for the engine, with only the surfaces the mod touches. Tests drive
// the mod through these, never through its internals.

export function fakeEngine({ root = '/ws', files = {}, stamps = {}, gate = null, scan = null, fork = null, failWrite = false } = {}) {
  const calls = { process: [], writes: [], forks: [] }
  const store = {}

  const engine = {
    plugin: { name: 'cos-memory', root: '/mod' },
    session: {
      root: async () => root,
      cwd: async () => root,
      id: async () => 'session-1',
    },
    fs: {
      read: async path => {
        if (!(path in files)) throw new Error(`ENOENT: ${path}`)
        return files[path]
      },
      write: async (path, text) => {
        if (failWrite) throw new Error(`EACCES: ${path}`)
        files[path] = text
        calls.writes.push({ path, text })
      },
      exists: async path => path in files,
      stat: async path => {
        if (!(path in files)) throw new Error(`ENOENT: ${path}`)
        const held = stamps[path] ?? { mtimeMs: 1, size: files[path].length }
        return { kind: 'file', size: held.size, mtimeMs: held.mtimeMs, isLink: false }
      },
      list: async () => [],
    },
    process: {
      run: async (argv, init) => {
        calls.process.push({ argv, init })
        // `quaqua memory scan` echoes the text back when it has no objection,
        // exactly as the engine does for a line it accepts.
        if (argv.at(-1) === 'scan') {
          if (scan instanceof Error) throw scan
          return (
            scan ?? {
              exitCode: 0,
              stdout: JSON.stringify({ ok: true, text: String(init?.stdin ?? '') }),
              stderr: '',
            }
          )
        }
        if (gate instanceof Error) throw gate
        return gate ?? { exitCode: 0, stdout: '', stderr: '' }
      },
    },
    store: {
      get: async key => store[key],
      set: async (key, value) => {
        store[key] = value
      },
      delete: async key => {
        delete store[key]
      },
      keys: async () => Object.keys(store),
    },
    model: {
      fork: async request => {
        calls.forks.push(request)
        if (fork instanceof Error) throw fork
        return fork ?? { isAnswered: false, reason: 'nothing to fork' }
      },
    },
  }

  return { $: engine, calls, files, stamps, store }
}

/** Collect the hooks a `register(on)` wires, so a test can raise one. */
export function captureHooks() {
  const byEvent = new Map()
  const on = (event, filterOrHandler, maybeHandler) => {
    const handler = typeof filterOrHandler === 'function' ? filterOrHandler : maybeHandler
    const list = byEvent.get(event) ?? []
    list.push(handler)
    byEvent.set(event, list)
    return { catch() {} }
  }
  return {
    on,
    handlers: event => byEvent.get(event) ?? [],
    raise: async (event, $, e) => {
      const [handler] = byEvent.get(event) ?? []
      if (!handler) throw new Error(`no hook for ${event}`)
      const next = async input => input
      return handler($, e, next)
    },
  }
}

/** Let work the mod started after its hook returned finish. */
export async function settle(ms = 20) {
  await new Promise(resolve => setTimeout(resolve, ms))
}

/** The JSON `quaqua memory hook pre` prints when it refuses a write. */
export function denyVerdict(reason) {
  return {
    exitCode: 0,
    stdout: JSON.stringify({
      hookSpecificOutput: {
        hookEventName: 'PreToolUse',
        permissionDecision: 'deny',
        permissionDecisionReason: reason,
      },
    }),
    stderr: '',
  }
}

export const allowVerdict = { exitCode: 0, stdout: '', stderr: '' }
