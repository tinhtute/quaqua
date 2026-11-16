# Build the Lever

When the work is not trivial, build the tool that does it or proves it instead of working by hand.

## Why

A codemod, generator, or script does the work the same way every time and reruns for free. The tool is an artifact a reviewer can read and rerun to check the work. Hand-applied edits can only be re-verified by redoing them. A deterministic script turns "trust me" into "run this".

## When

Applying to any non-trivial work, not just bulk changes: edits, migrations, analyses, checks.

## Rule

- **Do the first unit by hand to learn the recipe, then build the tool.** Prove it by rerunning it on that unit and diffing against the hand-done version. Make the lever safe to rerun.
- **Choose the right lever for the task:** a codemod or script for code edits, a generator for repetitive files, a dump-to-sqlite query for analysis, or a rerunnable check for verification.
- **A deterministic lever beats fan-out.** If the tool can process every unit in one pass, run it directly rather than fanning out workers to hand-apply what a script can do.
- **Write the lever as a shared recipe for workers.** When fanning work out, provide the recipe, the verification contract, and the do-not-touch fences in one artifact outside their write scope so they cannot quietly edit the contract.
- **Applying this principle produces a file.** If you cited it and there is no codemod, script, generator, or recipe in the diff, you did not apply it.
- **Commit the lever when the work outlives the session.**
- **Keep it minimal.** Per the Laziness Protocol (`principle-laziness-protocol`), build the smallest script that does or proves the job, never an unnecessary framework.
