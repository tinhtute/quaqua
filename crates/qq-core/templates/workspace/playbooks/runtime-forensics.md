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
