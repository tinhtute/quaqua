// A stand-in for the engine, with only the surfaces the mod touches. Tests drive
// the mod through these, never through its internals.

export function fakeEngine({ root = '/ws', files = {}, stamps = {}, verb = null, scan = null, fork = null, failWrite = false, caller = 'cos', cap = null } = {}) {
  const calls = { process: [], writes: [], forks: [], verbWrites: [] }
  const store = {}

  // A stand-in for the daemon's `memory.write` verb: cos only, drift guard,
  // an optional cap on each final text, then the files land.
  const daemonVerb = ({ files: asked }) => {
    const refuse = why => ({ exitCode: 1, stdout: '', stderr: `quaqua: refused: ${why}\n` })
    if (caller !== 'cos') return refuse(`${caller}: only cos writes memory`)
    for (const [rel, { before, after }] of Object.entries(asked)) {
      const path = `${root}/${rel}`
      if ((files[path] ?? '') !== before) return refuse(`${rel}: changed outside since you loaded it`)
      if (cap !== null && [...after].length > cap) return refuse(`${rel}: ${[...after].length} chars > ${cap} cap`)
    }
    for (const [rel, { after }] of Object.entries(asked)) {
      files[`${root}/${rel}`] = after
      calls.verbWrites.push(rel)
    }
    return { exitCode: 0, stdout: JSON.stringify({ caller, written: Object.keys(asked) }), stderr: '' }
  }

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
        if (argv[1] === 'call' && argv[2] === 'memory.write') {
          if (verb instanceof Error) throw verb
          return verb ?? daemonVerb(JSON.parse(argv[3]))
        }
        return { exitCode: 127, stdout: '', stderr: `unexpected command ${argv.join(' ')}` }
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
        if (typeof fork === 'function') return fork()
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

/** What `quaqua call memory.write` prints on stderr when the daemon refuses. */
export function denyVerdict(reason) {
  return { exitCode: 1, stdout: '', stderr: `quaqua: refused: ${reason}\n` }
}

/** Leaves the fake daemon's own judgement in charge. */
export const allowVerdict = null
