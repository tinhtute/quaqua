# Explain the Number

Before you trust, report, or act on a number you measured (a speedup, a regression, a throughput, a latency, or an eval result), find what limits it, and rule out that it measured something other than the work you think.

## Why

A performance measurement without a verified limiter or run spread is easily fooled by noise, cached runs, errors, or measuring an unintended path.

## When

Before trusting, reporting, or acting on a measured performance number or evaluation result.

## Rule

- **Ask "why not double?"** Name the resource or code path that bounds the result, such as a core, a lock, the disk, the network, or the load generator itself. Get it from a profile or from system counters taken during a run, then map it to source. A guess from reading the code is not a limiter. If you cannot say why the number is not twice as good, you do not know what you measured.
- **List what else the number could be measuring, and rule out each one with evidence.** The usual suspects are errors, skipped or cached work, code that never ran, an untuned side (one left on default settings), noise, and a piece too small to matter end to end.
- **Keep the evidence with the number.** Put the run count, the spread, and the limiter in the notes or a linked artifact.
- When evaluating trials: check whether every run completed the task, verify whether the gap holds across trials and models, and ensure the scenario reflects actual production workloads.
