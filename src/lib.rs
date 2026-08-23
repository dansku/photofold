//! photofold: merge photo folders, remove duplicates, keep the best copy.
//!
//! The program is a pipeline. [`pipeline::run`] calls each stage once, in order,
//! and every stage lives in its own module:
//!
//! [`scanner`] -> [`exact`] -> [`quality`] -> [`planner`] -> [`copier`] -> [`report`]
//!
//! Two rules shape the whole design:
//!
//! * **Source folders are read-only.** No code path in this crate opens a source
//!   file for writing, renames one, or deletes one.
//! * **When unsure, keep both.** A wrongly discarded photo cannot be recovered,
//!   so only [`models::MatchConfidence::Exact`] and
//!   [`models::MatchConfidence::VeryHigh`] matches remove a file from the output.

pub mod analyze;
pub mod apple;
pub mod bucketing;
pub mod cache;
pub mod cli;
pub mod copier;
pub mod decode;
pub mod error;
pub mod exact;
pub mod fingerprint;
pub mod grouping;
pub mod isobmff;
pub mod live_photo;
pub mod matching;
pub mod metadata;
pub mod models;
pub mod pipeline;
pub mod planner;
pub mod progress;
pub mod quality;
pub mod report;
pub mod scanner;
pub mod timing;
pub mod video;

#[cfg(test)]
pub mod testing;
