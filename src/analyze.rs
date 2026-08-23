//! The parallel analysis stage: metadata for every file, a fingerprint for
//! every image.
//!
//! This module owns the parallelism and the failure handling. The work itself
//! lives in [`crate::metadata`] and [`crate::fingerprint`], which are both
//! single-file, single-threaded, and therefore easy to test.
//!
//! A file that fails here is marked and kept. It loses the ability to be merged
//! visually, which is the safe direction: an unanalysed photo is copied rather
//! than discarded.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use rayon::prelude::*;

use crate::error::ErrorLog;
use crate::models::{FileFormat, Library, MediaKind, MediaMetadata, VisualFingerprint};
use crate::progress::Progress;
use crate::timing::Timings;
use crate::{decode, fingerprint, metadata};

/// What one worker learned about one file.
struct Analysis {
    format: FileFormat,
    metadata: Option<MediaMetadata>,
    fingerprint: Option<VisualFingerprint>,
    failed: bool,
}

/// Read just enough of the file to identify it.
fn read_header(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    let mut header = vec![0u8; FileFormat::SNIFF_BYTES];
    // A file shorter than the buffer is not an error; it is just not a photo.
    let read = read_at_most(&mut file, &mut header)?;
    header.truncate(read);
    Ok(header)
}

fn read_at_most(file: &mut std::fs::File, buffer: &mut [u8]) -> std::io::Result<usize> {
    use std::io::Read;

    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct AnalyzeStats {
    pub metadata_read: u64,
    pub fingerprinted: u64,
    pub metadata_failed: u64,
    pub fingerprint_failed: u64,
    /// Files whose contents did not match their extension, for example a JPEG
    /// named `.HEIC`. Common in Google Photos exports.
    pub misnamed: u64,
}

/// Fill in `metadata` and `fingerprint` for every file that can supply them.
pub fn run(
    library: &mut Library,
    errors: &ErrorLog,
    progress: &Progress,
    timings: &Timings,
) -> AnalyzeStats {
    let started = Instant::now();
    let bar = progress.bar(library.files.len() as u64, "Analysing");

    let metadata_read = AtomicU64::new(0);
    let fingerprinted = AtomicU64::new(0);
    let metadata_failed = AtomicU64::new(0);
    let fingerprint_failed = AtomicU64::new(0);
    let misnamed = AtomicU64::new(0);

    // Collect first, then write back, so the parallel section only needs a
    // shared borrow of the library.
    let results: Vec<Analysis> = library
        .files
        .par_iter()
        .map(|file| {
            // Everything below was measured on an earlier run and the file has
            // not changed since, so there is nothing to do.
            if file.from_cache {
                bar.inc(1);
                return Analysis {
                    format: file.format,
                    metadata: Some(file.metadata.clone()),
                    fingerprint: file.fingerprint,
                    failed: file.analysis_failed,
                };
            }

            let mut failed = false;

            // The extension is a hint, not a fact. Everything below uses the
            // format the bytes actually are.
            let format = match read_header(&file.path) {
                Ok(header) => FileFormat::reconcile(file.format, FileFormat::sniff(&header)),
                Err(err) => {
                    errors.push(&file.path, "identify", err);
                    file.format
                }
            };
            if format != file.format {
                misnamed.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    path = %file.path.display(),
                    declared = file.format.as_str(),
                    actual = format.as_str(),
                    "the file contents do not match the extension"
                );
            }

            let metadata = match metadata::extract(&file.path, format) {
                Ok(metadata) => {
                    metadata_read.fetch_add(1, Ordering::Relaxed);
                    Some(metadata)
                }
                Err(err) => {
                    metadata_failed.fetch_add(1, Ordering::Relaxed);
                    errors.push(&file.path, "metadata", err);
                    failed = true;
                    None
                }
            };

            let fingerprint = if format.kind() == MediaKind::Image && decode::can_decode(format) {
                let orientation = metadata.as_ref().and_then(|m| m.orientation);
                match fingerprint::compute(&file.path, format, orientation) {
                    Ok(fingerprint) => {
                        fingerprinted.fetch_add(1, Ordering::Relaxed);
                        Some(fingerprint)
                    }
                    Err(err) => {
                        fingerprint_failed.fetch_add(1, Ordering::Relaxed);
                        errors.push(&file.path, "decode", err);
                        failed = true;
                        None
                    }
                }
            } else {
                None
            };

            bar.inc(1);
            Analysis {
                format,
                metadata,
                fingerprint,
                failed,
            }
        })
        .collect();

    for (file, analysis) in library.files.iter_mut().zip(results) {
        file.format = analysis.format;
        file.kind = analysis.format.kind();
        if let Some(metadata) = analysis.metadata {
            file.metadata = metadata;
        }
        file.fingerprint = analysis.fingerprint;
        file.analysis_failed |= analysis.failed;
    }

    bar.finish_and_clear();

    let stats = AnalyzeStats {
        metadata_read: metadata_read.load(Ordering::Relaxed),
        fingerprinted: fingerprinted.load(Ordering::Relaxed),
        metadata_failed: metadata_failed.load(Ordering::Relaxed),
        fingerprint_failed: fingerprint_failed.load(Ordering::Relaxed),
        misnamed: misnamed.load(Ordering::Relaxed),
    };
    timings.record(
        "analyze",
        started.elapsed(),
        Some(stats.fingerprinted),
        None,
    );
    stats
}
