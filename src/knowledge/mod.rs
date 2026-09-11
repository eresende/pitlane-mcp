//! Knowledge-base indexing (issue #118, Phase 1).
//!
//! Generic content-source abstraction with a filesystem Markdown source.
//! Documents are parsed into heading-aware sections that carry hierarchy and
//! front-matter metadata, ready for OKF-specific fields in later phases.

pub mod document;
