//! Choosing a destination path for every file that will be kept.
//!
//! Planning is separated from copying for three reasons: `--dry-run` needs the
//! destinations without writing anything, name collisions must be resolved
//! before any bytes move, and a future `--organize year/month` only has to
//! change this module.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::cli::Config;
use crate::models::{FileId, Library};

/// A file to keep, together with any files that must travel with it.
///
/// A Live Photo is one unit: the still is the primary and its movie is a
/// companion, so the two can never be separated or given different names.
#[derive(Debug, Clone)]
pub struct KeepUnit {
    pub primary: FileId,
    pub companions: Vec<FileId>,
}

impl KeepUnit {
    pub fn single(primary: FileId) -> Self {
        Self {
            primary,
            companions: Vec::new(),
        }
    }

    fn members(&self) -> impl Iterator<Item = FileId> + '_ {
        std::iter::once(self.primary).chain(self.companions.iter().copied())
    }
}

#[derive(Debug, Clone)]
pub struct CopyEntry {
    pub file: FileId,
    pub destination: PathBuf,
    /// True when the original file name was already taken by a different photo.
    pub renamed: bool,
}

#[derive(Debug, Default)]
pub struct CopyPlan {
    pub entries: Vec<CopyEntry>,
}

impl CopyPlan {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn total_bytes(&self, library: &Library) -> u64 {
        self.entries
            .iter()
            .map(|entry| library.get(entry.file).size)
            .sum()
    }
}

/// Give every kept file a destination that is guaranteed not to exist yet.
///
/// Names already present in the output folder are reserved first, so a second
/// run into the same folder cannot overwrite the first run's work.
pub fn plan(library: &Library, units: &[KeepUnit], config: &Config) -> CopyPlan {
    let mut taken = existing_names(&config.output);
    let mut entries = Vec::with_capacity(units.len());

    for unit in units {
        let primary = library.get(unit.primary);
        let base_stem = primary
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("photo")
            .to_string();

        // Every member of a unit shares one stem, so the still and its movie stay
        // paired even when the stem had to change.
        let (stem, renamed) = free_stem(&base_stem, unit, library, &taken);

        for id in unit.members() {
            let name = match extension_of(&library.get(id).path) {
                Some(ext) => format!("{stem}.{ext}"),
                None => stem.clone(),
            };
            taken.insert(name.to_ascii_lowercase());
            entries.push(CopyEntry {
                file: id,
                destination: config.output.join(name),
                renamed,
            });
        }
    }

    CopyPlan { entries }
}

/// Find a stem where no member of the unit collides with an existing name.
fn free_stem(
    base: &str,
    unit: &KeepUnit,
    library: &Library,
    taken: &HashSet<String>,
) -> (String, bool) {
    let extensions: Vec<Option<String>> = unit
        .members()
        .map(|id| extension_of(&library.get(id).path))
        .collect();

    let fits = |stem: &str| {
        extensions.iter().all(|ext| {
            let name = match ext {
                Some(ext) => format!("{stem}.{ext}"),
                None => stem.to_string(),
            };
            !taken.contains(&name.to_ascii_lowercase())
        })
    };

    if fits(base) {
        return (base.to_string(), false);
    }
    // IMG_1234 -> IMG_1234_2 -> IMG_1234_3 ...
    for suffix in 2u32..u32::MAX {
        let candidate = format!("{base}_{suffix}");
        if fits(&candidate) {
            return (candidate, true);
        }
    }
    unreachable!("a free file name always exists below u32::MAX suffixes");
}

/// The file extension in its original spelling, so `.HEIC` never becomes `.heic`.
fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_string())
}

/// Names already in the output folder, lowercased.
///
/// Lowercasing matters: macOS and Windows filesystems are case-insensitive by
/// default, so `IMG_1.JPG` and `img_1.jpg` are the same file there.
fn existing_names(output: &Path) -> HashSet<String> {
    let Ok(entries) = std::fs::read_dir(output) else {
        return HashSet::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(|n| n.to_ascii_lowercase()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    #[test]
    fn different_photos_with_the_same_name_get_numbered_suffixes() {
        let (library, config) = testing::library(&[
            testing::file("/a/IMG_1234.HEIC"),
            testing::file("/b/IMG_1234.HEIC"),
            testing::file("/c/IMG_1234.HEIC"),
        ]);
        let units = [
            KeepUnit::single(0),
            KeepUnit::single(1),
            KeepUnit::single(2),
        ];
        let plan = plan(&library, &units, &config);
        let names: Vec<String> = plan
            .entries
            .iter()
            .map(|e| {
                e.destination
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            names,
            ["IMG_1234.HEIC", "IMG_1234_2.HEIC", "IMG_1234_3.HEIC"]
        );
        assert_eq!(
            plan.entries.iter().filter(|e| e.renamed).count(),
            2,
            "only the later two were renamed"
        );
    }

    #[test]
    fn collisions_are_case_insensitive() {
        let (library, config) = testing::library(&[
            testing::file("/a/IMG_1234.HEIC"),
            testing::file("/b/img_1234.heic"),
        ]);
        let units = [KeepUnit::single(0), KeepUnit::single(1)];
        let plan = plan(&library, &units, &config);
        assert!(
            plan.entries[1].destination.to_string_lossy().contains("_2"),
            "a case-only difference is still a collision on macOS"
        );
    }

    #[test]
    fn a_live_photo_pair_keeps_one_shared_stem() {
        let (library, config) = testing::library(&[
            // Takes the plain IMG_1234 stem for the HEIC first.
            testing::file("/a/IMG_1234.HEIC"),
            testing::file("/b/IMG_1234.HEIC"),
            testing::file("/b/IMG_1234.MOV"),
        ]);
        let units = [
            KeepUnit::single(0),
            KeepUnit {
                primary: 1,
                companions: vec![2],
            },
        ];
        let plan = plan(&library, &units, &config);
        assert_eq!(
            plan.entries[1].destination.file_name().unwrap(),
            "IMG_1234_2.HEIC"
        );
        assert_eq!(
            plan.entries[2].destination.file_name().unwrap(),
            "IMG_1234_2.MOV",
            "the movie must follow its still"
        );
    }

    #[test]
    fn a_pair_renames_together_even_when_only_one_part_collides() {
        let (library, config) = testing::library(&[
            // Only the movie name is taken; the still's name is still free.
            testing::file("/a/IMG_1234.MOV"),
            testing::file("/b/IMG_1234.HEIC"),
            testing::file("/b/IMG_1234.MOV"),
        ]);
        let units = [
            KeepUnit::single(0),
            KeepUnit {
                primary: 1,
                companions: vec![2],
            },
        ];
        let plan = plan(&library, &units, &config);
        assert_eq!(
            plan.entries[1].destination.file_name().unwrap(),
            "IMG_1234_2.HEIC",
            "the still moves aside so the pair keeps one stem"
        );
        assert_eq!(
            plan.entries[2].destination.file_name().unwrap(),
            "IMG_1234_2.MOV"
        );
    }

    #[test]
    fn extension_case_is_preserved() {
        let (library, config) = testing::library(&[testing::file("/a/IMG_1.HEIC")]);
        let plan = plan(&library, &[KeepUnit::single(0)], &config);
        assert_eq!(plan.entries[0].destination.extension().unwrap(), "HEIC");
    }
}
