//! Event sources: poll an external system and record triggers on the farm bus.
//!
//! A source is a read-only seam. It never mutates the system it reads, and a
//! failed read leaves the farm bus exactly as it was so the next poll retries.

pub mod github_ci;
