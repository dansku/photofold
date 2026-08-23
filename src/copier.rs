//! Placing the winning files in the output folder.
//!
//! Safety rules enforced here:
//!
//! * A destination file is created with `create_new`, which fails if anything is
//!   already there. Nothing is ever overwritten.
//! * Bytes are written to a temporary file and renamed into the reserved name,
//!   so an interrupted run cannot leave a half-written photo behind.
//! * Source files are only ever opened for reading.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use rayon::prelude::*;

use crate::cli::{Config, CopyMode};
use crate::error::{Error, ErrorLog, Result};
use crate::models::Library;
use crate::planner::CopyPlan;
use crate::progress::Progress;
use crate::timing::Timings;

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct CopyOutcome {
    pub copied: usize,
    pub failed: usize,
    pub bytes: u64,
    /// True when hard linking was requested but the output folder is on another
    /// filesystem, so copying was used instead.
    pub hardlink_fallback: bool,
}

/// Carry out the plan. Does nothing at all when `--dry-run` is set.
pub fn execute(
    plan: &CopyPlan,
    library: &Library,
    config: &Config,
    errors: &ErrorLog,
    progress: &Progress,
    timings: &Timings,
) -> Result<CopyOutcome> {
    if config.dry_run {
        return Ok(CopyOutcome::default());
    }

    fs::create_dir_all(&config.output).map_err(|source| Error::CreateOutput {
        path: config.output.clone(),
        source,
    })?;

    let started = Instant::now();
    let bar = progress.bar(plan.len() as u64, "Copying");
    let copied = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    let bytes = AtomicU64::new(0);
    let fallback = AtomicBool::new(false);

    plan.entries.par_iter().for_each(|entry| {
        let file = library.get(entry.file);
        let result = match config.copy_mode {
            CopyMode::Hardlink => link_or_copy(&file.path, &entry.destination, &fallback),
            CopyMode::Copy => copy_file(&file.path, &entry.destination),
        };
        match result {
            Ok(()) => {
                copied.fetch_add(1, Ordering::Relaxed);
                bytes.fetch_add(file.size, Ordering::Relaxed);
            }
            Err(err) => {
                failed.fetch_add(1, Ordering::Relaxed);
                errors.push(&file.path, "copy", err);
            }
        }
        bar.inc(1);
    });

    bar.finish_and_clear();

    let outcome = CopyOutcome {
        copied: copied.load(Ordering::Relaxed),
        failed: failed.load(Ordering::Relaxed),
        bytes: bytes.load(Ordering::Relaxed),
        hardlink_fallback: fallback.load(Ordering::Relaxed),
    };
    timings.record(
        "copy",
        started.elapsed(),
        Some(outcome.copied as u64),
        Some(outcome.bytes),
    );
    Ok(outcome)
}

/// Copy one file without any chance of clobbering an existing destination.
fn copy_file(source: &Path, destination: &Path) -> io::Result<()> {
    // Reserving the final name first is what makes this safe: the exclusive
    // create either succeeds, meaning the name is ours, or fails, meaning
    // something else is there and we must not touch it.
    let _reservation = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;

    let temporary = temporary_path(destination);
    let outcome = (|| -> io::Result<()> {
        let mut input = File::open(source)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        drop(output);
        // Replaces our own zero-byte reservation, so the destination goes from
        // empty to complete in one step.
        fs::rename(&temporary, destination)
    })();

    if outcome.is_err() {
        let _ = fs::remove_file(&temporary);
        let _ = fs::remove_file(destination);
    }
    outcome
}

/// Hard link when possible, copy when the output is on another filesystem.
fn link_or_copy(source: &Path, destination: &Path, fallback: &AtomicBool) -> io::Result<()> {
    // hard_link already fails if the destination exists, so no reservation is needed.
    match fs::hard_link(source, destination) {
        Ok(()) => Ok(()),
        Err(err) if is_cross_device(&err) => {
            if !fallback.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    "the output folder is on a different filesystem, so files are being copied instead of hard linked"
                );
            }
            copy_file(source, destination)
        }
        Err(err) => Err(err),
    }
}

fn is_cross_device(err: &io::Error) -> bool {
    // EXDEV is 18 on macOS and Linux. Windows reports it differently, and there
    // hard links across volumes fail with a plain permission-style error, so the
    // fallback simply does not trigger.
    err.raw_os_error() == Some(18)
}

fn temporary_path(destination: &Path) -> PathBuf {
    let mut name = destination.file_name().unwrap_or_default().to_os_string();
    name.push(".pmtmp");
    destination.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn copying_never_overwrites_an_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src.jpg");
        let destination = dir.path().join("dst.jpg");
        File::create(&source).unwrap().write_all(b"new").unwrap();
        File::create(&destination)
            .unwrap()
            .write_all(b"old")
            .unwrap();

        let err = copy_file(&source, &destination).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&destination).unwrap(), b"old");
    }

    #[test]
    fn copying_reproduces_the_bytes_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src.jpg");
        let destination = dir.path().join("dst.jpg");
        File::create(&source).unwrap().write_all(b"pixels").unwrap();

        copy_file(&source, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"pixels");
        assert!(!temporary_path(&destination).exists());
    }

    #[test]
    fn a_failed_copy_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.jpg");
        let destination = dir.path().join("dst.jpg");

        assert!(copy_file(&missing, &destination).is_err());
        assert!(!destination.exists(), "the reservation must be cleaned up");
        assert!(!temporary_path(&destination).exists());
    }
}
