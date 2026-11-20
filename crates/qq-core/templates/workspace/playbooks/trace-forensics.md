---
name: trace-forensics
when: Diagnose a captured profiling artifact (cpuprofile, trace, spindump, heap snapshot) handed to you after the fact.
---

## Orchestrator

1. You own the diagnosis from the artifact. Distinct from `runtime-forensics`, which instruments the live process: here the capture already exists. The artifact is a fixed dataset: read it, do not re-run it.
2. Keep tooling generic: trace parsers for cpuprofile and `.json.gz`, a text editor for a spindump, heap tooling for heapsnapshots, or sqlite for queryable transformations.
3. Protect context per principle-guard-the-context-window: require workers to reduce large traces off-context and return findings, never dumping raw traces into the report.
4. Review the cited diagnosis, artifact paths, and confidence (confirmed via paired capture vs strongest hypothesis). Route to `bug-fix` or `perf-issue` once the cause is known. No fix unless asked.

## Worker
