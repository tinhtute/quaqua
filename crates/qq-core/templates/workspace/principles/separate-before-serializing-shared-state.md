# Separate Before Serializing Shared State

When concurrent actors might write to shared state, eliminate the sharing first. Enforce serialization structurally only when one shared writer is a real invariant.

## Why

Concurrent writes to shared state create race conditions and contention that are intermittent, hard to reproduce, and expensive to debug. Separating state eliminates coordination overhead at the write path.

## When

Concurrent actors might write to the same file, branch, key, database row, or state object.

## Rule

- **Identify shared mutable state.** Locate files both read and write, branches both push to, keys both mutate, or tables both update.
- **Default: eliminate the shared write target.** Ask whether the actors need one canonical object or are publishing independent facts. Give each actor its own owned file, key, branch, or state directory, and merge only at the read or reporting boundary. Two workers writing their own fields into one shared file is still shared mutation; independent files combined at read time are not.
- **Structural serialization only when necessary.** Only when one shared write target is an unavoidable invariant, serialize access structurally (lockfiles, sequential phases, single-writer queues, or atomic operations). Treat "we need a lock" as a design smell to inspect, not a default answer.
- **Enforce boundaries by structure.** Instructions and conventions are not concurrency control; isolate worktrees, branches, and storage paths.
