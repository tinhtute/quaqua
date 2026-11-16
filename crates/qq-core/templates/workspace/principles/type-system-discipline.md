# Type System Discipline

Make the compiler reject impossible states, mismatched primitives, and unhandled variants. Prefer defining errors and special cases out of existence over proliferating handlers.

## Why

A type that admits an illegal state forces every reader and every call site to defend against it. A type that excludes it removes the handler entirely.

## When

Designing types, reviewing a function signature, or writing code in any statically typed language.

## Rule

- **Brand semantic primitives.** `UserId` and `OrderId` are strings underneath but should not be interchangeable. Give each its own type, such as a branded type or a newtype. Validate once at creation, trust the type downstream. If two arguments share a primitive type but mean different things, brand them.
- **External data is untyped until parsed.** RPC payloads, JSON, IPC messages, CLI arguments, config files, environment variables, and database rows are untyped. Have a parse function at every boundary that turns unstructured input into the typed model. `principle-boundary-discipline` says where to put validation.
- **Derive types from authoritative schemas.** When a protocol buffer, OpenAPI spec, GraphQL schema, database migration, design-token file, or another file owns a shape, derive from it instead of hand-rolling a parallel type. See `principle-encode-lessons-in-structure`.
