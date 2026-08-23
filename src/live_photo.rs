//! Keeping Live Photos in one piece.
//!
//! An Apple Live Photo is a still and a short movie that belong together. If the
//! two were deduplicated separately, a run could easily keep one source's still
//! and another source's movie, and the pair would be broken in the output. So
//! the rule here is simple and strict:
//!
//! **The still decides. The movie follows.**
//!
//! A paired movie takes no part in duplicate detection at all. It is copied when
//! its still is copied, dropped when its still is dropped, and given the same
//! output name either way.
//!
//! Pairing evidence, strongest first:
//!
//! 1. Both files carry the same Apple content identifier.
//! 2. Same folder, same file name, and the movie is short enough to be the
//!    motion part of a Live Photo, or the two were captured at the same moment.
//!
//! The second rule needs that corroboration. Name agreement on its own would
//! happily pair a two-minute holiday video with an unrelated photo, and since a
//! paired movie shares its still's fate, a wrong pairing could throw the video
//! away.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::models::{FileId, Library, MediaKind};

/// Longest movie still plausible as the motion part of a Live Photo.
///
/// Apple records about three seconds. The limit is generous because some
/// exporters pad the clip.
const MAX_MOTION_MILLISECONDS: u64 = 15_000;

/// How far apart a still and its movie may be recorded, when the movie's length
/// is unknown.
const MAX_CAPTURE_GAP_SECONDS: i64 = 60;

/// Which stills own which movies.
#[derive(Debug, Default)]
pub struct Pairing {
    still_to_movie: HashMap<FileId, FileId>,
    movie_to_still: HashMap<FileId, FileId>,
    /// Pairs found through the Apple content identifier rather than file names.
    pub matched_by_identifier: usize,
}

impl Pairing {
    pub fn movie_of(&self, still: FileId) -> Option<FileId> {
        self.still_to_movie.get(&still).copied()
    }

    pub fn still_of(&self, movie: FileId) -> Option<FileId> {
        self.movie_to_still.get(&movie).copied()
    }

    /// True when this movie belongs to a still and must not be judged alone.
    pub fn is_paired_movie(&self, file: FileId) -> bool {
        self.movie_to_still.contains_key(&file)
    }

    pub fn len(&self) -> usize {
        self.still_to_movie.len()
    }

    pub fn is_empty(&self) -> bool {
        self.still_to_movie.is_empty()
    }
}

/// Find every Live Photo pair in the library.
pub fn pair(library: &Library) -> Pairing {
    let mut pairing = Pairing::default();
    let mut taken: HashSet<FileId> = HashSet::new();

    let stills: Vec<FileId> = library
        .files
        .iter()
        .filter(|f| f.kind == MediaKind::Image)
        .map(|f| f.id)
        .collect();

    // Movies indexed both ways, so neither lookup has to scan the library.
    let mut by_identifier: HashMap<&str, Vec<FileId>> = HashMap::new();
    let mut by_location: HashMap<(&Path, String), Vec<FileId>> = HashMap::new();
    for file in library.files.iter().filter(|f| f.kind == MediaKind::Video) {
        if let Some(id) = file.metadata.live_photo_id.as_deref() {
            by_identifier.entry(id).or_default().push(file.id);
        }
        let folder = file.path.parent().unwrap_or(Path::new(""));
        by_location
            .entry((folder, file.stem()))
            .or_default()
            .push(file.id);
    }

    // Files are visited in id order, which the scanner sorted by path, so the
    // same library always produces the same pairing.
    for still in stills {
        let file = library.get(still);

        let by_id = file
            .metadata
            .live_photo_id
            .as_deref()
            .and_then(|id| by_identifier.get(id))
            .and_then(|movies| movies.iter().copied().find(|m| !taken.contains(m)));

        if let Some(movie) = by_id {
            taken.insert(movie);
            pairing.still_to_movie.insert(still, movie);
            pairing.movie_to_still.insert(movie, still);
            pairing.matched_by_identifier += 1;
            tracing::debug!(
                still = %file.path.display(),
                movie = %library.get(movie).path.display(),
                "Live Photo paired by content identifier"
            );
            continue;
        }

        let folder = file.path.parent().unwrap_or(Path::new(""));
        let by_name = by_location.get(&(folder, file.stem())).and_then(|movies| {
            movies
                .iter()
                .copied()
                .find(|m| !taken.contains(m) && looks_like_motion(library, still, *m))
        });

        if let Some(movie) = by_name {
            taken.insert(movie);
            pairing.still_to_movie.insert(still, movie);
            pairing.movie_to_still.insert(movie, still);
            tracing::debug!(
                still = %file.path.display(),
                movie = %library.get(movie).path.display(),
                "Live Photo paired by file name"
            );
        }
    }

    pairing
}

/// Whether this movie is plausibly the motion part of that still.
fn looks_like_motion(library: &Library, still: FileId, movie: FileId) -> bool {
    let still = library.get(still);
    let movie = library.get(movie);

    if let Some(duration) = movie.metadata.video.as_ref().and_then(|v| v.duration_ms) {
        return duration <= MAX_MOTION_MILLISECONDS;
    }
    // No duration available, so fall back to the capture times.
    match (still.metadata.capture_time, movie.metadata.capture_time) {
        (Some(a), Some(b)) => (a - b).abs() <= MAX_CAPTURE_GAP_SECONDS,
        // Nothing corroborates the name, so do not pair. A wrongly paired movie
        // could be discarded along with a still it never belonged to.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    fn short_movie(path: &str) -> testing::FileSpec {
        testing::file(path).duration_ms(1_880)
    }

    #[test]
    fn a_still_and_its_movie_pair_by_file_name() {
        let (library, _) = testing::library(&[
            testing::file("/phone/IMG_1234.HEIC"),
            short_movie("/phone/IMG_1234.MOV"),
        ]);
        let pairing = pair(&library);
        assert_eq!(pairing.movie_of(0), Some(1));
        assert_eq!(pairing.still_of(1), Some(0));
        assert!(pairing.is_paired_movie(1));
    }

    /// Google Takeout renames the movie to `.MP4` but keeps the stem.
    #[test]
    fn a_google_export_pairs_a_heic_with_an_mp4() {
        let (library, _) = testing::library(&[
            testing::file("/google/IMG_8990.HEIC"),
            short_movie("/google/IMG_8990.MP4"),
        ]);
        assert_eq!(pair(&library).movie_of(0), Some(1));
    }

    #[test]
    fn a_long_video_is_never_paired_with_a_photo() {
        let (library, _) = testing::library(&[
            testing::file("/phone/IMG_1234.HEIC").captured_at(1_700_000_000),
            // Two minutes: a holiday clip that happens to share a name.
            testing::file("/phone/IMG_1234.MOV")
                .duration_ms(120_000)
                .captured_at(1_700_000_000),
        ]);
        assert!(pair(&library).is_empty());
    }

    #[test]
    fn without_a_duration_or_a_timestamp_nothing_is_paired() {
        let (library, _) = testing::library(&[
            testing::file("/phone/IMG_1234.HEIC"),
            testing::file("/phone/IMG_1234.MOV"),
        ]);
        assert!(
            pair(&library).is_empty(),
            "a shared name alone is not enough evidence"
        );
    }

    #[test]
    fn the_content_identifier_beats_the_file_name() {
        let (library, _) = testing::library(&[
            testing::file("/phone/IMG_1234.HEIC").live_photo_id("ABC"),
            short_movie("/phone/IMG_1234.MOV"),
            short_movie("/phone/OTHER.MOV").live_photo_id("ABC"),
        ]);
        let pairing = pair(&library);
        assert_eq!(pairing.movie_of(0), Some(2));
        assert_eq!(pairing.matched_by_identifier, 1);
        assert!(
            !pairing.is_paired_movie(1),
            "the name match must not also fire"
        );
    }

    #[test]
    fn one_movie_is_never_claimed_by_two_stills() {
        let (library, _) = testing::library(&[
            testing::file("/phone/IMG_1234.HEIC").live_photo_id("ABC"),
            testing::file("/phone/IMG_1234.JPG").live_photo_id("ABC"),
            short_movie("/phone/IMG_1234.MOV").live_photo_id("ABC"),
        ]);
        let pairing = pair(&library);
        assert_eq!(pairing.movie_of(0), Some(2));
        assert_eq!(pairing.movie_of(1), None);
    }

    #[test]
    fn a_movie_in_another_folder_is_not_paired_by_name() {
        let (library, _) = testing::library(&[
            testing::file("/phone/IMG_1234.HEIC"),
            short_movie("/google/IMG_1234.MOV"),
        ]);
        assert!(pair(&library).is_empty());
    }
}
