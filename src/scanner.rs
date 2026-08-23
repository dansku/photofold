//! Recursive scanning of the source folders.
//!
//! The scanner is the only stage that touches the source tree's shape, so every
//! safety rule about what must not be ingested lives here.

use std::path::Path;
use std::time::UNIX_EPOCH;

use walkdir::{DirEntry, WalkDir};

use crate::cli::Config;
use crate::error::{ErrorLog, Result};
use crate::models::{FileFormat, Library, MediaFile, MediaKind, MediaMetadata};
use crate::progress::Progress;

/// Directory names that are skipped wherever they appear.
///
/// These hold deleted files or filesystem bookkeeping. Copying a photo out of a
/// trash folder would silently undelete it, which is not what a merge should do.
const SKIPPED_DIRECTORIES: &[&str] = &[
    ".Trashes",
    ".Trash",
    ".Spotlight-V100",
    ".fseventsd",
    ".DocumentRevisions-V100",
    ".TemporaryItems",
    ".photofold-cache",
    "$RECYCLE.BIN",
    "System Volume Information",
];

/// What the scan found, beyond the files themselves.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct ScanStats {
    /// Every filesystem entry the walk saw, including ones that were rejected.
    pub entries_seen: u64,
    /// Files whose extension is not a supported media type.
    pub unsupported: u64,
    /// AppleDouble resource forks (`._IMG_1234.HEIC`), common on external and
    /// FAT-formatted volumes. They carry a media extension but no image.
    pub appledouble: u64,
    /// Sidecar metadata files such as Google Takeout `.json` companions.
    pub sidecars: u64,
    /// Videos skipped because `--no-videos` was given.
    pub videos_skipped: u64,
    /// Symbolic links, which are never followed.
    pub symlinks: u64,
    /// Files below the output folder, skipped so a run cannot ingest its own output.
    pub inside_output: u64,
}

/// Walk every source folder and build the [`Library`].
pub fn scan(
    config: &Config,
    errors: &ErrorLog,
    progress: &Progress,
) -> Result<(Library, ScanStats)> {
    let mut library = Library {
        sources: config.sources.clone(),
        ..Library::default()
    };
    let mut stats = ScanStats::default();

    let spinner = progress.spinner("Scanning");
    if config.output_is_nested() {
        tracing::info!(
            output = %config.output.display(),
            "the output folder is inside an input folder; it will be skipped during the scan"
        );
    }

    for root in config.walk_roots() {
        tracing::debug!(root = %root.path.display(), "walking source root");

        let walk = WalkDir::new(&root.path)
            .follow_links(false)
            .into_iter()
            // filter_entry prunes whole subtrees, so a skipped folder costs nothing.
            .filter_entry(|entry| !should_prune(entry, config));

        for entry in walk {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    let path = err.path().unwrap_or(&root.path).to_path_buf();
                    errors.push(&path, "scan", err);
                    continue;
                }
            };

            stats.entries_seen += 1;
            let file_type = entry.file_type();

            if file_type.is_symlink() {
                // Never followed: a link can point back into the tree, or at the
                // very file it duplicates.
                stats.symlinks += 1;
                continue;
            }
            if !file_type.is_file() {
                continue;
            }

            let path = entry.path();
            if is_inside_output(path, config) {
                stats.inside_output += 1;
                continue;
            }

            let Some(format) = FileFormat::from_path(path) else {
                stats.unsupported += 1;
                continue;
            };

            if is_appledouble(path) {
                stats.appledouble += 1;
                library.skipped_appledouble += 1;
                continue;
            }

            match format.kind() {
                MediaKind::Sidecar => {
                    // Recognised, but not part of the merge in this version.
                    stats.sidecars += 1;
                    continue;
                }
                MediaKind::Video if !config.include_videos => {
                    stats.videos_skipped += 1;
                    continue;
                }
                _ => {}
            }

            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(err) => {
                    errors.push(path, "scan", err);
                    continue;
                }
            };

            let (modified_at, modified_nanos) = match metadata.modified() {
                Ok(time) => match time.duration_since(UNIX_EPOCH) {
                    Ok(delta) => (Some(delta.as_secs() as i64), Some(delta.subsec_nanos())),
                    // A modification time before 1970 is odd but not fatal.
                    Err(_) => (None, None),
                },
                Err(_) => (None, None),
            };

            let source = config.owner_of(path).map(|s| s.id).unwrap_or(root.id);

            let id = library.files.len();
            library.files.push(MediaFile {
                id,
                path: path.to_path_buf(),
                source,
                kind: format.kind(),
                format,
                size: metadata.len(),
                modified_at,
                modified_nanos,
                exact_hash: None,
                metadata: MediaMetadata::default(),
                fingerprint: None,
                analysis_failed: false,
                from_cache: false,
            });

            spinner.inc(1);
        }
    }

    // A stable order makes every later stage, and therefore every report,
    // reproducible across runs.
    library.files.sort_by(|a, b| a.path.cmp(&b.path));
    for (index, file) in library.files.iter_mut().enumerate() {
        file.id = index;
    }

    spinner.finish_and_clear();
    Ok((library, stats))
}

/// Whether the walk should refuse to descend into this entry.
fn should_prune(entry: &DirEntry, config: &Config) -> bool {
    if entry.depth() == 0 {
        return false;
    }
    if !entry.file_type().is_dir() {
        return false;
    }
    if entry.path() == config.output || entry.path().starts_with(&config.output) {
        return true;
    }
    entry
        .file_name()
        .to_str()
        .is_some_and(|name| SKIPPED_DIRECTORIES.contains(&name))
}

fn is_inside_output(path: &Path, config: &Config) -> bool {
    path.starts_with(&config.output)
}

/// AppleDouble sidecars mirror a real file's name behind a `._` prefix.
fn is_appledouble(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.starts_with("._"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appledouble_files_are_recognised() {
        assert!(is_appledouble(Path::new("/v/._IMG_1234.HEIC")));
        assert!(!is_appledouble(Path::new("/v/IMG_1234.HEIC")));
        assert!(!is_appledouble(Path::new("/v/_IMG_1234.HEIC")));
    }
}
