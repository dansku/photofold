//! Deciding which copy of a photo to keep.
//!
//! Every ranking rule lives in this module and nowhere else. Each rule that
//! fires adds points and appends a sentence, so the `winner_reason` shown in the
//! report is generated from the same code that made the decision. There is no
//! second, hand-written explanation that could drift out of step.
//!
//! Weights are chosen so that resolution dominates. A picture with more pixels
//! wins even if every other signal favours the alternative, because throwing
//! away real detail is the one mistake that cannot be undone later.

use crate::cli::Config;
use crate::models::{FileFormat, FileId, Library, MediaFile};

/// Points per megapixel. Large on purpose: one extra megapixel outranks every
/// other rule combined.
const POINTS_PER_MEGAPIXEL: f64 = 10_000.0;

const CAMERA_ORIGINAL_BONUS: i64 = 8_000;
const EDITED_PENALTY: i64 = -6_000;
const GPS_BONUS: i64 = 500;
const LIVE_PHOTO_COMPLETE_BONUS: i64 = 5_000;
const PREFERRED_SOURCE_BONUS: i64 = 1_500;
/// Points per metadata tag, capped by [`MAX_METADATA_POINTS`].
const POINTS_PER_TAG: i64 = 50;
const MAX_METADATA_POINTS: i64 = 3_000;
/// File size is a tie-break only. A recompressed export is often larger than the
/// camera original, so this must never be able to outvote format or metadata.
const MAX_SIZE_POINTS: i64 = 800;

/// A file's rank, plus the reasons that produced it.
#[derive(Debug, Clone, Default)]
pub struct QualityScore {
    pub total: i64,
    pub reasons: Vec<String>,
}

impl QualityScore {
    fn add(&mut self, points: i64, reason: impl Into<String>) {
        self.total += points;
        self.reasons.push(reason.into());
    }
}

/// Name fragments that mark a file as an export, an edit, or a copy.
const EDITED_MARKERS: &[&str] = &[
    "-edited",
    "_edited",
    "-edit",
    "(1)",
    "(2)",
    " copy",
    "-copy",
    "_copy",
    "-collage",
    "-animation",
    "-effects",
    "screenshot",
    "-smart-crop",
];

/// Prefixes that cameras and phones use when they name a file themselves.
const CAMERA_FILENAME_PREFIXES: &[&str] = &[
    "img_", "dsc", "dscn", "dscf", "_mg_", "_dsc", "imgp", "gopr", "pxl_", "p10", "p_", "mvimg_",
];

/// Score one file. Higher is better.
pub fn score(file: &MediaFile, library: &Library, _config: &Config) -> QualityScore {
    let mut score = QualityScore::default();

    if let Some(pixels) = file.metadata.pixels() {
        let megapixels = pixels as f64 / 1_000_000.0;
        score.add(
            (megapixels * POINTS_PER_MEGAPIXEL) as i64,
            format!(
                "{}x{} ({:.1} MP)",
                file.metadata.width.unwrap_or(0),
                file.metadata.height.unwrap_or(0),
                megapixels
            ),
        );
    }

    score.add(format_points(file.format), format_reason(file.format));

    if let Some(depth) = file.metadata.bit_depth
        && depth > 8
    {
        score.add(i64::from(depth - 8) * 500, format!("{depth}-bit depth"));
    }

    let edited = edit_marker(file);
    match edited {
        Some(marker) => score.add(
            EDITED_PENALTY,
            format!("looks edited or exported ({marker})"),
        ),
        None if looks_like_camera_original(file) => {
            score.add(CAMERA_ORIGINAL_BONUS, "looks like the camera original")
        }
        None => {}
    }

    let metadata_points =
        (i64::from(file.metadata.tag_count) * POINTS_PER_TAG).min(MAX_METADATA_POINTS);
    if metadata_points > 0 {
        score.add(
            metadata_points,
            format!("{} metadata tags", file.metadata.tag_count),
        );
    }
    if file.metadata.gps.is_some() {
        score.add(GPS_BONUS, "has GPS location");
    }
    if file.metadata.live_photo_id.is_some() {
        score.add(LIVE_PHOTO_COMPLETE_BONUS, "part of a Live Photo");
    }

    // Weak, capped tie-break: one point per 128 KiB, up to MAX_SIZE_POINTS.
    let size_points = ((file.size / (128 * 1024)) as i64).min(MAX_SIZE_POINTS);
    score.total += size_points;

    let preference = library.sources[file.source].preference;
    if preference > 0 {
        score.add(
            i64::from(preference) * PREFERRED_SOURCE_BONUS,
            format!(
                "from preferred source {}",
                library.sources[file.source].path.display()
            ),
        );
    }

    score
}

/// Choose the best file in a group and explain the choice.
///
/// Ties break on the source path, so two runs over the same library always pick
/// the same winner.
pub fn pick_winner(
    members: &[FileId],
    library: &Library,
    config: &Config,
) -> (FileId, Vec<String>) {
    debug_assert!(!members.is_empty(), "a duplicate group is never empty");

    let mut scored: Vec<(FileId, QualityScore)> = members
        .iter()
        .map(|&id| (id, score(library.get(id), library, config)))
        .collect();

    scored.sort_by(|a, b| {
        b.1.total
            .cmp(&a.1.total)
            .then_with(|| library.get(a.0).path.cmp(&library.get(b.0).path))
    });

    let (winner, winner_score) = &scored[0];
    let mut reasons = winner_score.reasons.clone();

    // Say what the winner beat, so the report is readable without re-deriving.
    if let Some((runner_up, runner_score)) = scored.get(1) {
        let margin = winner_score.total - runner_score.total;
        reasons.push(format!(
            "preferred over {} by {margin} points",
            library.get(*runner_up).file_name()
        ));
    }

    (*winner, reasons)
}

fn format_points(format: FileFormat) -> i64 {
    match format {
        FileFormat::Dng | FileFormat::Cr2 | FileFormat::Cr3 | FileFormat::Nef | FileFormat::Arw => {
            6_000
        }
        FileFormat::Heif => 4_000,
        FileFormat::Tiff | FileFormat::Png => 2_000,
        FileFormat::Webp => 500,
        FileFormat::Jpeg => 0,
        // Videos are only ever compared against other videos.
        FileFormat::Mov => 500,
        FileFormat::Mp4 | FileFormat::M4v => 0,
        FileFormat::JsonSidecar => 0,
    }
}

fn format_reason(format: FileFormat) -> String {
    if format.is_raw() {
        return format!("{} is a camera raw file", format.as_str().to_uppercase());
    }
    match format {
        FileFormat::Heif => "HEIC keeps more of the original data than JPEG".to_string(),
        FileFormat::Tiff | FileFormat::Png => {
            format!("{} is lossless", format.as_str().to_uppercase())
        }
        other => format!("{} format", other.as_str().to_uppercase()),
    }
}

/// Returns the marker that makes this file look like an export or an edit.
fn edit_marker(file: &MediaFile) -> Option<&'static str> {
    let name = file.file_name().to_ascii_lowercase();
    if let Some(marker) = EDITED_MARKERS.iter().find(|m| name.contains(**m)) {
        return Some(marker);
    }
    // Editors stamp their name into the Software tag; cameras write their own
    // firmware version there, which is why the camera names are excluded.
    let software = file.metadata.software.as_ref()?.to_ascii_lowercase();
    const EDITOR_SOFTWARE: &[&str] = &[
        "photoshop",
        "lightroom",
        "gimp",
        "picasa",
        "google photos",
        "snapseed",
        "instagram",
    ];
    EDITOR_SOFTWARE
        .iter()
        .find(|s| software.contains(**s))
        .copied()
}

/// Whether the file still carries the name and metadata a camera gave it.
fn looks_like_camera_original(file: &MediaFile) -> bool {
    let stem = file.stem();
    let camera_name = CAMERA_FILENAME_PREFIXES
        .iter()
        .any(|prefix| stem.starts_with(prefix));
    let has_camera_metadata =
        file.metadata.camera_make.is_some() || file.metadata.camera_model.is_some();
    let original_format = file.format.is_raw() || matches!(file.format, FileFormat::Heif);

    camera_name || has_camera_metadata || original_format
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    #[test]
    fn heic_beats_a_bigger_recompressed_jpeg_at_the_same_resolution() {
        let (library, config) = testing::library(&[
            testing::file("/phone/IMG_1234.HEIC")
                .dimensions(4032, 3024)
                .size(2_837_211)
                .camera("Apple", "iPhone 15 Pro")
                .tags(40),
            testing::file("/google/IMG_1234.JPG")
                .dimensions(4032, 3024)
                .size(5_248_212)
                .tags(12),
        ]);
        let (winner, reasons) = pick_winner(&[0, 1], &library, &config);
        assert_eq!(winner, 0, "reasons: {reasons:?}");
    }

    #[test]
    fn more_pixels_always_wins() {
        let (library, config) = testing::library(&[
            testing::file("/phone/IMG_1234.HEIC")
                .dimensions(2048, 1536)
                .size(2_000_000)
                .camera("Apple", "iPhone 15 Pro")
                .tags(60),
            testing::file("/google/IMG_1234.JPG")
                .dimensions(4032, 3024)
                .size(1_000_000)
                .tags(2),
        ]);
        let (winner, _) = pick_winner(&[0, 1], &library, &config);
        assert_eq!(winner, 1);
    }

    #[test]
    fn an_edited_export_loses_to_the_original() {
        let (library, config) = testing::library(&[
            testing::file("/google/IMG_1234-edited.jpg")
                .dimensions(4032, 3024)
                .size(6_000_000),
            testing::file("/phone/IMG_1234.JPG")
                .dimensions(4032, 3024)
                .size(3_000_000),
        ]);
        let (winner, _) = pick_winner(&[0, 1], &library, &config);
        assert_eq!(winner, 1);
    }

    #[test]
    fn source_preference_breaks_a_tie_but_does_not_beat_resolution() {
        let (library, config) = testing::library_with_preference(
            &[
                testing::file("/a/IMG_1.JPG")
                    .dimensions(1000, 1000)
                    .size(100),
                testing::file("/b/IMG_1.JPG")
                    .dimensions(1000, 1000)
                    .size(100),
            ],
            "/b",
        );
        assert_eq!(pick_winner(&[0, 1], &library, &config).0, 1);

        let (library, config) = testing::library_with_preference(
            &[
                testing::file("/a/IMG_1.JPG")
                    .dimensions(4000, 3000)
                    .size(100),
                testing::file("/b/IMG_1.JPG")
                    .dimensions(1000, 1000)
                    .size(100),
            ],
            "/b",
        );
        assert_eq!(pick_winner(&[0, 1], &library, &config).0, 0);
    }
}
