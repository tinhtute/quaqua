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

1. Identify the format and load it with the right tool. Parse large artifacts off-context per principle-guard-the-context-window and keep the reduced finding in the report.
2. Transform the raw artifact into a form you can query. Dump the trace or heap snapshot into sqlite, one row per sample, frame, or node. Reach the queryable shape before analyzing.
3. Narrow to the cause. Query for the frames holding the most time and walk the call tree to the hot path. For a leak, follow the retainer chain from the leaked object to a GC root. For a spindump, find the thread stuck on-CPU or blocked and its wait reason.
4. Attribute to source. Map the hot frame to file, symbol, and line via the artifact's own symbols. A frame with no source mapping is not yet a diagnosis: resolve the symbols, or state plainly that the artifact does not carry them.
5. If you have a paired capture, diff the before and after artifacts to confirm. Otherwise, mark the finding as the strongest hypothesis the artifact supports, not a confirmed cause.
6. Hand back a cited diagnosis, no fix unless asked. Route to `bug-fix` or `perf-issue` once the cause is known. Throughput checkpoint stays one line: `throughput checkpoint: n/a, read-only forensics`.
7. Check every Evidence line, then write `Verified: <level> <sha>`, citing principle-prove-it-works.

**Reply:** the artifact and format, the reduced finding, the source location, the artifact paths, and whether a paired capture confirmed it.
