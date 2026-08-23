//! Remembering what was learned about a file, so a second run is nearly free.
//!
//! Hashing and decoding dominate the cost of a large merge. Neither depends on
//! anything but the file itself, so both results are stored in a small SQLite
//! database and reused as long as the file has not changed.
//!
//! The key is `(path, size, modification time)`. Any edit changes at least one
//! of those, so a stale entry is never used. Nothing else is trusted: the cache
//! never decides that two files are duplicates, it only replays measurements.
//!
//! A cache failure is never fatal. If the database cannot be opened or written,
//! the run continues at full cost and says so.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::models::{FileFormat, Library, MediaMetadata, VisualFingerprint};

/// Bumped whenever a stored value changes meaning.
///
/// The fingerprint algorithm and the format-sniffing rules both feed this
/// table, so an old database must be discarded rather than half-trusted.
const SCHEMA_VERSION: i64 = 1;

/// What was measured about one file, last time it was seen.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub size: u64,
    pub mtime_sec: i64,
    pub mtime_nanos: u32,
    pub format: FileFormat,
    pub exact_hash: Option<[u8; 32]>,
    pub fingerprint: Option<VisualFingerprint>,
    pub metadata: MediaMetadata,
    pub analysis_failed: bool,
}

#[derive(Debug, Default)]
pub struct Cache {
    path: Option<PathBuf>,
    entries: HashMap<PathBuf, CacheEntry>,
    /// Entries reused during this run.
    pub hits: usize,
}

impl Cache {
    /// A cache that stores nothing, used for `--no-cache`.
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Open, or create, the database at `path`.
    ///
    /// Returns a disabled cache rather than an error when the database is
    /// unusable, because a broken cache must not stop a merge.
    pub fn open(path: &Path) -> Self {
        match load(path) {
            Ok(entries) => {
                tracing::info!(
                    path = %path.display(),
                    entries = entries.len(),
                    "analysis cache opened"
                );
                Self {
                    path: Some(path.to_path_buf()),
                    entries,
                    hits: 0,
                }
            }
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "could not use the analysis cache, so every file will be analysed again"
                );
                Self::disabled()
            }
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.path.is_some()
    }

    /// Copy every still-valid cached measurement into the library.
    ///
    /// Files that were served from the cache are marked, so the hashing and
    /// analysis stages can skip them.
    pub fn apply(&mut self, library: &mut Library) {
        if !self.is_enabled() {
            return;
        }
        let mut hits = 0;
        for file in &mut library.files {
            let Some(entry) = self.entries.get(&file.path) else {
                continue;
            };
            // Any change to the file invalidates everything measured from it.
            if entry.size != file.size
                || Some(entry.mtime_sec) != file.modified_at
                || Some(entry.mtime_nanos) != file.modified_nanos
            {
                continue;
            }

            file.format = entry.format;
            file.kind = entry.format.kind();
            file.exact_hash = entry.exact_hash;
            file.fingerprint = entry.fingerprint;
            file.metadata = entry.metadata.clone();
            file.analysis_failed = entry.analysis_failed;
            file.from_cache = true;
            hits += 1;
        }
        self.hits = hits;
        tracing::info!(hits, total = library.files.len(), "cache hits");
    }

    /// Write everything measured in this run back to the database.
    pub fn save(&self, library: &Library) {
        let Some(path) = &self.path else {
            return;
        };
        if let Err(err) = store(path, library) {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "could not write the analysis cache; the next run will be slower"
            );
        }
    }
}

fn load(path: &Path) -> rusqlite::Result<HashMap<PathBuf, CacheEntry>> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        // A missing folder is not an error worth failing the run over; if it
        // cannot be created, opening the database below reports the real cause.
        let _ = std::fs::create_dir_all(parent);
    }

    let connection = Connection::open(path)?;
    prepare_schema(&connection)?;

    let mut statement = connection.prepare(
        "SELECT path, size, mtime_sec, mtime_nanos, format, exact_hash, dhash, blockhash, \
         metadata, analysis_failed FROM analysis",
    )?;
    let rows = statement.query_map([], |row| {
        let path: String = row.get(0)?;
        let format: String = row.get(4)?;
        let exact_hash: Option<Vec<u8>> = row.get(5)?;
        let dhash: Option<i64> = row.get(6)?;
        let blockhash: Option<Vec<u8>> = row.get(7)?;
        let metadata: String = row.get(8)?;

        let fingerprint = match (dhash, blockhash) {
            (Some(dhash), Some(blockhash)) if blockhash.len() == 32 => Some(VisualFingerprint {
                dhash: dhash as u64,
                blockhash: blockhash.try_into().expect("length checked above"),
            }),
            _ => None,
        };

        Ok((
            PathBuf::from(path),
            CacheEntry {
                size: row.get::<_, i64>(1)? as u64,
                mtime_sec: row.get(2)?,
                mtime_nanos: row.get::<_, i64>(3)? as u32,
                format: parse_format(&format),
                exact_hash: exact_hash.and_then(|bytes| bytes.try_into().ok()),
                fingerprint,
                metadata: serde_json::from_str(&metadata).unwrap_or_default(),
                analysis_failed: row.get(9)?,
            },
        ))
    })?;

    let mut entries = HashMap::new();
    for row in rows {
        let (path, entry) = row?;
        entries.insert(path, entry);
    }
    Ok(entries)
}

/// Create the table, discarding any database written by an older version.
fn prepare_schema(connection: &Connection) -> rusqlite::Result<()> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let table_exists: Option<String> = connection
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='analysis'",
            [],
            |row| row.get(0),
        )
        .optional()?;

    if table_exists.is_some() && version != SCHEMA_VERSION {
        tracing::info!(
            found = version,
            expected = SCHEMA_VERSION,
            "the analysis cache was written by another version, so it is being rebuilt"
        );
        connection.execute("DROP TABLE analysis", [])?;
    }

    connection.execute_batch(&format!(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE IF NOT EXISTS analysis (
             path            TEXT PRIMARY KEY,
             size            INTEGER NOT NULL,
             mtime_sec       INTEGER NOT NULL,
             mtime_nanos     INTEGER NOT NULL,
             format          TEXT NOT NULL,
             exact_hash      BLOB,
             dhash           INTEGER,
             blockhash       BLOB,
             metadata        TEXT NOT NULL,
             analysis_failed INTEGER NOT NULL
         );
         PRAGMA user_version = {SCHEMA_VERSION};"
    ))?;
    Ok(())
}

fn store(path: &Path, library: &Library) -> rusqlite::Result<()> {
    let mut connection = Connection::open(path)?;
    prepare_schema(&connection)?;

    // One transaction: 200,000 separate commits would take minutes.
    let transaction = connection.transaction()?;
    {
        let mut statement = transaction.prepare(
            "INSERT OR REPLACE INTO analysis
             (path, size, mtime_sec, mtime_nanos, format, exact_hash, dhash, blockhash, \
              metadata, analysis_failed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for file in &library.files {
            // Without a modification time there is no way to tell later whether
            // the file changed, so it is not worth storing.
            let (Some(mtime_sec), Some(mtime_nanos)) = (file.modified_at, file.modified_nanos)
            else {
                continue;
            };
            let metadata = serde_json::to_string(&file.metadata).unwrap_or_else(|_| "{}".into());
            statement.execute(params![
                file.path.to_string_lossy(),
                file.size as i64,
                mtime_sec,
                i64::from(mtime_nanos),
                file.format.as_str(),
                file.exact_hash.map(|hash| hash.to_vec()),
                file.fingerprint.map(|print| print.dhash as i64),
                file.fingerprint.map(|print| print.blockhash.to_vec()),
                metadata,
                file.analysis_failed,
            ])?;
        }
    }
    transaction.commit()
}

fn parse_format(text: &str) -> FileFormat {
    FileFormat::from_extension(text).unwrap_or(FileFormat::Jpeg)
}

/// Where the cache lives when the user does not say.
///
/// Follows the platform convention so that the database does not end up inside
/// a photo folder, where a later run would try to scan it.
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("PHOTOFOLD_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from))
        .or_else(|| {
            let home = PathBuf::from(std::env::var_os("HOME")?);
            Some(if cfg!(target_os = "macos") {
                home.join("Library/Caches")
            } else {
                home.join(".cache")
            })
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("photofold").join("analysis.sqlite")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    fn library_with_measurements() -> Library {
        let (mut library, _) = testing::library(&[
            testing::file("/a/IMG_1.JPG")
                .dimensions(4032, 3024)
                .tags(30),
            testing::file("/a/clip.MOV"),
        ]);
        library.files[0].exact_hash = Some([3; 32]);
        library.files[0].fingerprint = Some(VisualFingerprint {
            dhash: 0x0102_0304_0506_0708,
            blockhash: [9; 32],
        });
        library
    }

    #[test]
    fn measurements_survive_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite");

        let library = library_with_measurements();
        Cache::open(&path).save(&library);

        let mut reloaded = library_with_measurements();
        for file in &mut reloaded.files {
            file.exact_hash = None;
            file.fingerprint = None;
            file.metadata = MediaMetadata::default();
        }

        let mut cache = Cache::open(&path);
        cache.apply(&mut reloaded);

        assert_eq!(cache.hits, 2);
        assert_eq!(reloaded.files[0].exact_hash, Some([3; 32]));
        assert_eq!(
            reloaded.files[0].fingerprint.unwrap().dhash,
            0x0102_0304_0506_0708
        );
        assert_eq!(reloaded.files[0].metadata.width, Some(4032));
        assert!(reloaded.files[0].from_cache);
    }

    #[test]
    fn a_changed_file_is_not_served_from_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite");
        Cache::open(&path).save(&library_with_measurements());

        for change in ["size", "seconds", "nanoseconds"] {
            let mut library = library_with_measurements();
            library.files[0].exact_hash = None;
            match change {
                "size" => library.files[0].size += 1,
                "seconds" => library.files[0].modified_at = Some(1),
                _ => library.files[0].modified_nanos = Some(1),
            }

            let mut cache = Cache::open(&path);
            cache.apply(&mut library);
            assert!(
                !library.files[0].from_cache,
                "a change in {change} must invalidate the entry"
            );
            assert_eq!(library.files[0].exact_hash, None);
        }
    }

    #[test]
    fn a_database_from_another_version_is_rebuilt_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite");
        Cache::open(&path).save(&library_with_measurements());

        // Pretend an older release wrote this file.
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("PRAGMA user_version = 0;")
            .unwrap();
        drop(connection);

        let mut library = library_with_measurements();
        library.files[0].exact_hash = None;
        let mut cache = Cache::open(&path);
        cache.apply(&mut library);

        assert_eq!(cache.hits, 0);
        assert!(!library.files[0].from_cache);
    }

    #[test]
    fn an_unusable_cache_path_does_not_stop_the_run() {
        // A directory is not a database file.
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path());
        assert!(!cache.is_enabled());

        let mut library = library_with_measurements();
        cache.save(&library);
        Cache::disabled().apply(&mut library);
    }

    #[test]
    fn the_default_path_is_outside_any_photo_folder() {
        let path = default_path();
        assert!(path.ends_with("photofold/analysis.sqlite"));
    }
}
