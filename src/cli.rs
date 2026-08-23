//! Command-line parsing and validation.
//!
//! [`Cli`] is the raw parsed form. [`Config`] is the validated, canonicalised
//! form that the rest of the program uses; nothing downstream sees a relative or
//! unchecked path.

use std::path::{Component, Path, PathBuf};

use clap::{ArgAction, Parser, ValueEnum};

use crate::error::{Error, Result};
use crate::models::SourceRoot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum ReportFormat {
    Json,
    Csv,
}

impl ReportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Csv => "csv",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum CopyMode {
    /// Duplicate the bytes into the output directory.
    Copy,
    /// Create a hard link to the source file. Fast and free, but the output
    /// entry then shares storage with the source.
    Hardlink,
}

const LONG_ABOUT: &str = "\
Merge photos and videos from several folders into one, keeping the best copy of
each picture.

photofold scans every source folder, finds files that are the same photo even
when the format, file name, or compression differs, and copies only the best
version into the output folder.

Source folders are opened read-only. Nothing in them is ever moved, changed, or
deleted.

When photofold is not sure that two files are the same photo, it keeps both.";

const EXAMPLES: &str = "\
EXAMPLES:
  Preview a merge without copying anything:
    photofold ~/Photos/iPhone ~/Downloads/GooglePhotos -o ~/Merged --dry-run

  Merge, and prefer the phone originals when quality is otherwise equal:
    photofold ~/Phone ~/Google -o ~/Merged --prefer-source ~/Phone

  Analyse everything again, ignoring earlier results:
    photofold ~/Phone ~/Google -o ~/Merged --no-cache

  Merge photos only, and write a CSV report:
    photofold ~/Phone ~/Google -o ~/Merged --no-videos \\
      --report merge.csv --report-format csv
";

#[derive(Debug, Parser)]
#[command(
    name = "photofold",
    version,
    about = "Merge photo folders, remove duplicates, keep the best copy",
    long_about = LONG_ABOUT,
    after_help = EXAMPLES,
    max_term_width = 100
)]
pub struct Cli {
    /// Folders to scan. Give as many as you like.
    #[arg(required = true, value_name = "SOURCE")]
    pub inputs: Vec<PathBuf>,

    /// Folder to write the merged library into. Created if it does not exist.
    #[arg(short, long, value_name = "PATH")]
    pub output: PathBuf,

    /// Analyse everything and write the report, but copy nothing.
    #[arg(long)]
    pub dry_run: bool,

    /// Print more detail. Repeat for even more (-vv).
    #[arg(short, long, action = ArgAction::Count)]
    pub verbose: u8,

    /// Number of worker threads [default: number of CPU cores]
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u16).range(1..))]
    pub workers: Option<u16>,

    /// How different two pictures may look and still count as the same photo,
    /// in bits of a 256-bit fingerprint. Lower is safer.
    #[arg(long, value_name = "N", default_value_t = 8, value_parser = clap::value_parser!(u32).range(0..=64))]
    pub hash_distance: u32,

    /// Where to write the report [default: ./photofold-report.json]
    #[arg(long, value_name = "PATH")]
    pub report: Option<PathBuf>,

    /// Report file format.
    #[arg(long, value_enum, default_value_t = ReportFormat::Json)]
    pub report_format: ReportFormat,

    /// How to place files in the output folder.
    #[arg(long, value_enum, default_value_t = CopyMode::Copy)]
    pub copy_mode: CopyMode,

    /// Include videos. This is the default.
    #[arg(long, overrides_with = "no_videos")]
    pub include_videos: bool,

    /// Skip video files completely.
    #[arg(long, overrides_with = "include_videos")]
    pub no_videos: bool,

    /// Do not keep Live Photo stills and movies together.
    #[arg(long)]
    pub no_live_photos: bool,

    /// Prefer this source folder when two copies are otherwise equal.
    /// Repeat to set an order.
    #[arg(long, value_name = "PATH")]
    pub prefer_source: Vec<PathBuf>,

    /// Analyse every file again instead of reusing earlier results.
    #[arg(long)]
    pub no_cache: bool,

    /// Where to keep the analysis cache
    /// [default: a photofold folder in the system cache directory]
    #[arg(long, value_name = "PATH", conflicts_with = "no_cache")]
    pub cache_path: Option<PathBuf>,
}

/// Validated settings for one run.
#[derive(Debug, Clone)]
pub struct Config {
    pub sources: Vec<SourceRoot>,
    pub output: PathBuf,
    pub dry_run: bool,
    pub verbose: u8,
    pub workers: usize,
    pub hash_distance: u32,
    pub report_path: PathBuf,
    pub report_format: ReportFormat,
    pub copy_mode: CopyMode,
    pub include_videos: bool,
    pub live_photos: bool,
    /// Where to keep the analysis cache, or `None` when caching is off.
    pub cache_path: Option<PathBuf>,
}

impl Cli {
    /// Check the arguments against the filesystem and turn them into a [`Config`].
    ///
    /// This is where every path becomes absolute and every impossible request is
    /// rejected, so that no later stage has to re-check.
    pub fn into_config(self) -> Result<Config> {
        let mut sources: Vec<SourceRoot> = Vec::new();
        for input in &self.inputs {
            if !input.exists() {
                return Err(Error::MissingInput(input.clone()));
            }
            if !input.is_dir() {
                return Err(Error::NotADirectory(input.clone()));
            }
            let path = resolve(input)?;
            // The same folder listed twice must not double-count its files.
            if sources.iter().any(|s| s.path == path) {
                tracing::warn!(path = %path.display(), "input folder given more than once, ignoring the repeat");
                continue;
            }
            sources.push(SourceRoot {
                id: sources.len(),
                path,
                preference: 0,
            });
        }

        let output = resolve(&self.output)?;
        if let Some(clash) = sources.iter().find(|s| s.path == output) {
            return Err(Error::OutputIsInput(clash.path.clone()));
        }

        // Earlier --prefer-source flags outrank later ones.
        let preferred: Vec<PathBuf> = self
            .prefer_source
            .iter()
            .map(|p| resolve(p))
            .collect::<Result<_>>()?;
        for (rank, wanted) in preferred.iter().enumerate() {
            let weight = (preferred.len() - rank) as u32;
            match sources.iter_mut().find(|s| &s.path == wanted) {
                Some(source) => source.preference = weight,
                None => return Err(Error::PreferSourceNotAnInput(wanted.clone())),
            }
        }

        let workers = match self.workers {
            Some(n) => usize::from(n),
            None => std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
        };

        let report_path = match self.report {
            Some(path) => path,
            None => PathBuf::from(format!(
                "photofold-report.{}",
                self.report_format.extension()
            )),
        };

        Ok(Config {
            sources,
            output,
            dry_run: self.dry_run,
            verbose: self.verbose,
            workers,
            hash_distance: self.hash_distance,
            report_path,
            report_format: self.report_format,
            copy_mode: self.copy_mode,
            // Videos are included unless the user explicitly opts out.
            include_videos: !self.no_videos,
            live_photos: !self.no_live_photos,
            cache_path: if self.no_cache {
                None
            } else {
                Some(self.cache_path.unwrap_or_else(crate::cache::default_path))
            },
        })
    }
}

impl Config {
    /// Source roots that should actually be walked.
    ///
    /// If one source folder sits inside another, walking both would visit its
    /// files twice. Only the outermost roots are walked; [`Config::owner_of`]
    /// still attributes each file to the most specific source.
    pub fn walk_roots(&self) -> Vec<&SourceRoot> {
        self.sources
            .iter()
            .filter(|candidate| {
                !self.sources.iter().any(|other| {
                    other.path != candidate.path && candidate.path.starts_with(&other.path)
                })
            })
            .collect()
    }

    /// The deepest source folder containing `path`.
    ///
    /// With nested inputs, a file belongs to the innermost folder the user named,
    /// which is what makes `--prefer-source` on a subfolder behave sensibly.
    pub fn owner_of(&self, path: &Path) -> Option<&SourceRoot> {
        self.sources
            .iter()
            .filter(|s| path.starts_with(&s.path))
            .max_by_key(|s| s.path.components().count())
    }

    /// Whether the output folder lives inside one of the input folders.
    ///
    /// When it does, the scanner must prune it, or a second run would treat the
    /// previous run's output as more source material.
    pub fn output_is_nested(&self) -> bool {
        self.sources
            .iter()
            .any(|s| self.output.starts_with(&s.path))
    }
}

/// Make a path absolute and free of `.` and `..`, without requiring it to exist.
///
/// [`std::fs::canonicalize`] fails on a path that is not there yet, and the
/// output folder usually is not. So the deepest existing ancestor is
/// canonicalised and the remaining names are appended.
fn resolve(path: &Path) -> Result<PathBuf> {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return Ok(canonical);
    }

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|source| Error::ResolvePath {
                path: path.to_path_buf(),
                source,
            })?
            .join(path)
    };

    // Walk up to the first ancestor that exists, canonicalise it, then rebuild.
    let mut trailing: Vec<&std::ffi::OsStr> = Vec::new();
    let mut cursor: &Path = &absolute;
    loop {
        if let Ok(canonical) = std::fs::canonicalize(cursor) {
            let mut result = canonical;
            for name in trailing.iter().rev() {
                result.push(name);
            }
            return Ok(result);
        }
        match (cursor.file_name(), cursor.parent()) {
            (Some(name), Some(parent)) => {
                trailing.push(name);
                cursor = parent;
            }
            // Nothing on the path exists; fall back to a lexical cleanup.
            _ => return Ok(lexical_normalize(&absolute)),
        }
    }
}

/// Remove `.` and `..` components without touching the filesystem.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_normalize_removes_dot_segments() {
        assert_eq!(
            lexical_normalize(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
    }

    #[test]
    fn resolve_handles_a_path_that_does_not_exist_yet() {
        let dir = tempfile::tempdir().unwrap();
        let wanted = dir.path().join("not-created-yet/deeper");
        let resolved = resolve(&wanted).unwrap();
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("not-created-yet/deeper"));
    }
}
