# Guard the Context Window

Context window overflow degrades reasoning quality, introduces compression artifacts, and halts progress. Route bulk work to subagents and keep concise summaries in the main thread.

## Why

Dumping raw file outputs, verbose logs, or wide exploratory diffs into the coordinator context floods working memory, causes loss of steering instructions, and leads to hallucinations.

## When

Context is filling up: large outputs, long files, repeated reads, fan-out planning. Route bulk to subagents; keep summaries in the main thread, not raw payloads.

## Rule

- **Size phases and cap scope.** Limit files per phase, set turn budgets, and account for mechanism overhead.
- **File pointers, not dumps.** Pass file paths and entry points instead of inlined dumps of long source files.
- **Delegate bulk analysis.** Route large diff reviews, log processing, or multi-file research to specialized subagents, returning structured summaries.
