//! Byte-for-byte duplicate detection.
//!
//! Hashing an entire photo library is wasteful, because most files have a size
//! no other file shares. The work is therefore narrowed in three steps:
//!
//! 1. Group by file size. A file with a unique size cannot have an identical
//!    twin, so it is never opened.
//! 2. Within a size group, hash only the head and tail of each file. Different
//!    edges rule out a match without reading the middle.
//! 3. Only the survivors are hashed in full.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use rayon::prelude::*;

use crate::error::ErrorLog;
use crate::models::{FileId, Library};
use crate::progress::Progress;
use crate::timing::Timings;

/// Bytes read from each end of a file during the partial-hash step.
const PARTIAL_WINDOW: u64 = 64 * 1024;

/// Files at or below this size are hashed in full straight away; a partial hash
/// of a small file would read most of it anyway.
const PARTIAL_MIN_SIZE: u64 = 4 * PARTIAL_WINDOW;

/// Sets `exact_hash` on every file that needed one, and returns the sets of
/// files that are byte-for-byte identical.
///
/// Each returned group holds two or more [`FileId`] values, sorted.
pub fn detect(
    library: &mut Library,
    errors: &ErrorLog,
    progress: &Progress,
    timings: &Timings,
) -> Vec<Vec<FileId>> {
    let started = Instant::now();
    let bytes_read = AtomicU64::new(0);

    // Step 1: only sizes shared by more than one file can hold duplicates.
    let mut by_size: HashMap<u64, Vec<FileId>> = HashMap::new();
    for file in &library.files {
        by_size.entry(file.size).or_default().push(file.id);
    }
    let size_candidates: Vec<Vec<FileId>> = by_size
        .into_values()
        .filter(|group| group.len() > 1)
        .collect();

    let candidate_count: usize = size_candidates.iter().map(Vec::len).sum();
    tracing::debug!(
        candidates = candidate_count,
        total = library.files.len(),
        "files sharing a size with at least one other file"
    );

    if candidate_count == 0 {
        timings.record("exact_hash", started.elapsed(), Some(0), Some(0));
        return Vec::new();
    }

    let bar = progress.bar(candidate_count as u64, "Hashing candidates");

    // Step 2: cheap head-and-tail hash to split each size group further.
    let mut full_hash_candidates: Vec<Vec<FileId>> = Vec::new();
    for group in size_candidates {
        let size = library.files[group[0]].size;
        if size <= PARTIAL_MIN_SIZE {
            full_hash_candidates.push(group);
            continue;
        }
        // A cached hash means this file will not be read at all, so the partial
        // step has nothing to save here. Skipping it also keeps the group whole:
        // splitting on a partial hash that some members never computed could
        // separate two files that are in fact identical.
        if group
            .iter()
            .any(|&id| library.files[id].exact_hash.is_some())
        {
            full_hash_candidates.push(group);
            continue;
        }

        let partials: Vec<(FileId, Option<[u8; 32]>)> = group
            .par_iter()
            .map(|&id| {
                let path = &library.files[id].path;
                match partial_hash(path, size) {
                    Ok((hash, read)) => {
                        bytes_read.fetch_add(read, Ordering::Relaxed);
                        (id, Some(hash))
                    }
                    Err(err) => {
                        errors.push(path, "hash", err);
                        (id, None)
                    }
                }
            })
            .collect();

        let mut by_partial: HashMap<[u8; 32], Vec<FileId>> = HashMap::new();
        for (id, hash) in partials {
            let Some(hash) = hash else {
                library.files[id].analysis_failed = true;
                continue;
            };
            by_partial.entry(hash).or_default().push(id);
        }
        full_hash_candidates.extend(by_partial.into_values().filter(|g| g.len() > 1));
    }

    // Step 3: full hash, but only for files that survived both filters.
    let survivors: Vec<FileId> = full_hash_candidates.into_iter().flatten().collect();
    let hashes: Vec<(FileId, Option<[u8; 32]>)> = survivors
        .par_iter()
        .map(|&id| {
            let file = &library.files[id];
            // Already measured on an earlier run, and the file has not changed.
            if let Some(hash) = file.exact_hash {
                bar.inc(1);
                return (id, Some(hash));
            }
            let result = match full_hash(&file.path) {
                Ok(hash) => {
                    bytes_read.fetch_add(file.size, Ordering::Relaxed);
                    (id, Some(hash))
                }
                Err(err) => {
                    errors.push(&file.path, "hash", err);
                    (id, None)
                }
            };
            bar.inc(1);
            result
        })
        .collect();

    let mut by_hash: HashMap<[u8; 32], Vec<FileId>> = HashMap::new();
    for (id, hash) in hashes {
        match hash {
            Some(hash) => {
                library.files[id].exact_hash = Some(hash);
                by_hash.entry(hash).or_default().push(id);
            }
            None => library.files[id].analysis_failed = true,
        }
    }

    bar.finish_and_clear();

    let mut groups: Vec<Vec<FileId>> = by_hash
        .into_values()
        .filter(|g| g.len() > 1)
        .map(|mut g| {
            g.sort_unstable();
            g
        })
        .collect();
    // Deterministic order for the report.
    groups.sort_unstable();

    timings.record(
        "exact_hash",
        started.elapsed(),
        Some(survivors.len() as u64),
        Some(bytes_read.load(Ordering::Relaxed)),
    );
    groups
}

/// Hash the first and last [`PARTIAL_WINDOW`] bytes, plus the size.
///
/// The size is mixed in so that two files cannot collide across size groups.
/// Returns the hash and how many bytes were read.
fn partial_hash(path: &Path, size: u64) -> std::io::Result<([u8; 32], u64)> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&size.to_le_bytes());

    let window = PARTIAL_WINDOW.min(size) as usize;
    let mut buffer = vec![0u8; window];

    file.read_exact(&mut buffer)?;
    hasher.update(&buffer);

    file.seek(SeekFrom::End(-(window as i64)))?;
    file.read_exact(&mut buffer)?;
    hasher.update(&buffer);

    Ok((*hasher.finalize().as_bytes(), (window as u64) * 2))
}

/// Hash the whole file through a memory map, so a 4 GB video never becomes a
/// 4 GB allocation.
fn full_hash(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_mmap(path)?;
    Ok(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn partial_hash_differs_when_the_tail_differs() {
        let dir = tempfile::tempdir().unwrap();
        let size = (PARTIAL_MIN_SIZE + 1024) as usize;

        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        let mut body = vec![7u8; size];
        File::create(&a).unwrap().write_all(&body).unwrap();
        *body.last_mut().unwrap() = 9;
        File::create(&b).unwrap().write_all(&body).unwrap();

        let (ha, _) = partial_hash(&a, size as u64).unwrap();
        let (hb, _) = partial_hash(&b, size as u64).unwrap();
        assert_ne!(ha, hb);
    }

    #[test]
    fn full_hash_matches_for_identical_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        File::create(&a).unwrap().write_all(b"hello world").unwrap();
        File::create(&b).unwrap().write_all(b"hello world").unwrap();
        assert_eq!(full_hash(&a).unwrap(), full_hash(&b).unwrap());
    }
}
