---
name: runtime-forensics
when: Diagnose a runtime symptom (leak, idle-CPU spin, glitch) from live instrumentation: deliverable is a diagnosis, not a fix.
---

## Orchestrator

1. You own the diagnosis. Instrument the live process, do not theorize from source. The deliverable is a cited diagnosis, not a fix.
2. Frame the symptom, live target process, and environment in the brief. Scope the worker strictly to runtime diagnosis; no fix unless explicitly asked. Hand back to `bug-fix` or `perf-issue` once the root cause is known.
3. Protect context per principle-guard-the-context-window: parse large profiles or traces off-context and summarize findings in the report rather than inlining full artifacts.
4. Review the captured signal, reduced mechanism proof, and source location before accepting.

## Worker

1. Capture the live signal on the matching surface: a CPU profile for a spinning process, a heap snapshot for a memory leak, or a browser inspector/CDP trace for a visual or event loop glitch. A real artifact, not a guess.
2. Reduce the artifact to the smoking gun: the function on the hot path, the retainer chain from the leaked object to a GC root, or the loop firing without input. Parse large artifacts off-context per principle-guard-the-context-window, keeping the reduced finding in the report.
3. Prove the mechanism before believing it. Inject instrumentation via runtime evaluation on the running process, or hotfix the live code without reloading, to confirm the hypothesis cheaply.
4. Map the finding back to source: file, symbol, and the line that allocates or schedules.
5. Throughput checkpoint stays one line: `throughput checkpoint: n/a, read-only forensics`.
6. Check every Evidence line, then write `Verified: <level> <sha>`, citing principle-prove-it-works.

**Reply:** the signal captured, the reduced finding, how you proved the mechanism, the source location, artifact paths. No fix unless asked. Hand back to Bug fix or Perf once the cause is known.
