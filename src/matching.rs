//! Deciding whether two files are the same photo.
//!
//! Comparison never returns a bare yes or no. It gathers [`Evidence`], then runs
//! a small ordered table of rules over it. The first rule that fits sets the
//! confidence. Keeping the rules in a table rather than in a chain of `if`
//! statements means a rule can be read, changed, and tested on its own.
//!
//! Only [`MatchConfidence::Exact`] and [`MatchConfidence::VeryHigh`] ever remove
//! a file from the output, so a rule that is too generous costs a photo, and a
//! rule that is too strict only costs some disk space. The thresholds below are
//! set accordingly.
//!
//! **The 256-bit block hash decides, not the 64-bit gradient hash.** Measured on
//! a real iPhone HEIC and the JPEG exported from it, the block hashes differed
//! by 2 bits while the gradient hashes differed by 7. The gradient hash is
//! computed from an 8x8 grid, so it is very sensitive to which decoder produced
//! the thumbnail; the block hash averages over 16x16 cells and is not. The
//! gradient hash is still reported, because it is a readable "how different do
//! these look" number, but nothing is merged on its say-so.

use crate::cli::Config;
use crate::models::{MatchConfidence, MediaFile};

/// How much looser the report-only threshold is than the merge threshold.
const PROBABLE_MULTIPLIER: u32 = 3;

/// The block hash is 256 bits, so no distance can exceed this.
const MAX_BLOCKHASH_DISTANCE: u32 = 256;

/// Aspect ratios closer than this count as the same shape. It allows for a few
/// rows of pixels lost to a re-encoder, but not a real crop.
const ASPECT_TOLERANCE: f32 = 0.01;

/// Capture times further apart than this cannot be the same photo, whatever the
/// pixels say. Two visually identical files taken a month apart are a wallpaper,
/// a screenshot, or a logo, not a duplicate.
const IMPOSSIBLE_TIME_GAP_SECONDS: i64 = 30 * 24 * 60 * 60;

/// Something observed about a pair of files.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum Evidence {
    IdenticalBytes,
    VisualDistance(u32),
    DetailDistance(u32),
    SameDimensions,
    SameAspectRatio,
    CaptureTimeDelta(i64),
    CaptureTimeMissing,
    SameCamera,
    SameBaseFilename,
    SameLivePhotoId,
    SameLocation,
    DifferentFormat,
}

/// The outcome of comparing two files.
#[derive(Debug, Clone)]
pub struct Verdict {
    pub confidence: MatchConfidence,
    /// Which rule decided it, for `--verbose` output and for the report.
    pub rule: &'static str,
    pub evidence: Vec<Evidence>,
    /// Perceptual distance, when both files had a fingerprint.
    pub distance: Option<u32>,
}

/// The facts a rule can test. Computed once per pair.
#[derive(Debug, Clone, Copy, Default)]
struct Facts {
    /// Largest block-hash difference that may still merge, from `--hash-distance`.
    merge_limit: u32,
    /// Largest block-hash difference worth reporting at all.
    report_limit: u32,
    visual: Option<u32>,
    detail: Option<u32>,
    same_dimensions: bool,
    same_aspect: bool,
    capture_delta: Option<i64>,
    same_camera: bool,
    same_stem: bool,
    same_live_photo_id: bool,
    same_location: bool,
    /// The two files are stored in different container formats, for example
    /// HEIC and JPEG. One of them was therefore made from the other.
    different_format: bool,
}

impl Facts {
    /// The pictures agree closely enough to act on.
    fn same_picture(&self) -> bool {
        self.detail.is_some_and(|d| d <= self.merge_limit)
    }

    /// The pictures are similar enough to be worth mentioning.
    fn similar_picture(&self) -> bool {
        self.detail.is_some_and(|d| d <= self.report_limit)
    }

    fn captured_within(&self, seconds: i64) -> bool {
        self.capture_delta.is_some_and(|d| d <= seconds)
    }

    /// True when at least one file has no capture time, so time proves nothing.
    fn time_unknown(&self) -> bool {
        self.capture_delta.is_none()
    }
}

struct Rule {
    name: &'static str,
    confidence: MatchConfidence,
    holds: fn(&Facts) -> bool,
}

/// Ordered from most to least confident. The first rule that holds decides.
///
/// Every rule that can remove a file demands **structural** agreement on top of
/// visual agreement: a shared Live Photo identifier, a shared file name, or one
/// capture stored in two different formats. Visual likeness alone is never
/// enough, because a burst of shots is visually alike and is not a duplicate.
///
/// This was learned from real photos. In a 1,096-file library, five pairs of
/// consecutive frames (`IMG_6691.JPG` and `IMG_6692.JPG` and so on) had block
/// hashes within the merge threshold, the same dimensions, the same camera, and
/// capture times one second apart. They were separate shutter presses. An
/// earlier rule that accepted "same size, same camera, seconds apart" merged
/// them and would have deleted five photographs.
const RULES: &[Rule] = &[
    Rule {
        name: "the same Live Photo identifier, and the same picture",
        confidence: MatchConfidence::VeryHigh,
        holds: |f| f.same_live_photo_id && f.same_picture() && f.same_aspect,
    },
    Rule {
        name: "the same picture under the same file name",
        confidence: MatchConfidence::VeryHigh,
        holds: |f| {
            f.same_picture()
                && f.same_aspect
                && f.same_stem
                && (f.time_unknown() || f.captured_within(60))
        },
    },
    Rule {
        name: "one capture stored in two formats, with the same timestamp",
        confidence: MatchConfidence::VeryHigh,
        // A camera writes every frame of a burst in the same format, so a format
        // difference means one of the two files was made from the other.
        holds: |f| f.same_picture() && f.same_aspect && f.different_format && f.captured_within(0),
    },
    Rule {
        name: "a similar picture of the same size taken at the same moment",
        confidence: MatchConfidence::Probable,
        holds: |f| f.similar_picture() && f.same_dimensions && f.captured_within(2),
    },
    Rule {
        name: "a similar picture taken at almost the same time",
        confidence: MatchConfidence::Probable,
        holds: |f| f.similar_picture() && f.same_aspect && f.captured_within(10),
    },
    Rule {
        name: "a similar picture of the same size",
        confidence: MatchConfidence::Probable,
        holds: |f| f.similar_picture() && f.same_dimensions,
    },
];

/// Compare two files.
///
/// Returns `None` when the two are not related closely enough to be worth
/// mentioning at all.
pub fn compare(a: &MediaFile, b: &MediaFile, config: &Config) -> Option<Verdict> {
    if let (Some(ha), Some(hb)) = (a.exact_hash, b.exact_hash)
        && ha == hb
    {
        return Some(Verdict {
            confidence: MatchConfidence::Exact,
            rule: "byte-for-byte identical",
            evidence: vec![Evidence::IdenticalBytes],
            distance: Some(0),
        });
    }

    let facts = gather(a, b, config);

    // Too different to be worth reporting at all.
    if !facts.similar_picture() {
        return None;
    }

    // A shape change is a crop or a different picture, never a re-encode.
    if !facts.same_aspect && !facts.same_dimensions {
        return None;
    }

    let mut confidence = MatchConfidence::Uncertain;
    let mut rule = "similar picture, but not enough agreeing evidence";
    for candidate in RULES {
        if (candidate.holds)(&facts) {
            confidence = candidate.confidence;
            rule = candidate.name;
            break;
        }
    }

    // A veto, applied after the rules so that no rule has to repeat it: pixels
    // never outrank two capture times that are far apart.
    if let Some(delta) = facts.capture_delta
        && delta > IMPOSSIBLE_TIME_GAP_SECONDS
        && confidence > MatchConfidence::Uncertain
    {
        confidence = MatchConfidence::Uncertain;
        rule = "looks the same, but the capture times are far apart";
    }

    Some(Verdict {
        confidence,
        rule,
        evidence: describe(&facts),
        distance: facts.visual,
    })
}

fn gather(a: &MediaFile, b: &MediaFile, config: &Config) -> Facts {
    let mut facts = Facts {
        merge_limit: config.hash_distance,
        report_limit: (config.hash_distance * PROBABLE_MULTIPLIER).min(MAX_BLOCKHASH_DISTANCE),
        ..Facts::default()
    };

    if let (Some(fa), Some(fb)) = (a.fingerprint.as_ref(), b.fingerprint.as_ref()) {
        facts.visual = Some(fa.dhash_distance(fb));
        facts.detail = Some(fa.blockhash_distance(fb));
    }

    facts.same_dimensions = match (a.metadata.pixels(), b.metadata.pixels()) {
        (Some(_), Some(_)) => {
            // Compare the sorted pair, so a rotated copy still counts as the
            // same size.
            sorted_dimensions(a) == sorted_dimensions(b)
        }
        _ => false,
    };

    facts.same_aspect = match (a.metadata.aspect_ratio(), b.metadata.aspect_ratio()) {
        (Some(ra), Some(rb)) => (ra - rb).abs() <= ASPECT_TOLERANCE,
        // Unknown dimensions must not silently pass as "same shape".
        _ => false,
    };

    facts.capture_delta = match (a.metadata.capture_time, b.metadata.capture_time) {
        (Some(ta), Some(tb)) => Some((ta - tb).abs()),
        _ => None,
    };

    facts.same_camera = match (
        a.metadata.camera_model.as_deref(),
        b.metadata.camera_model.as_deref(),
    ) {
        (Some(ma), Some(mb)) => ma.eq_ignore_ascii_case(mb),
        _ => false,
    };

    facts.same_stem = normalised_stem(a) == normalised_stem(b);

    facts.same_live_photo_id = match (
        a.metadata.live_photo_id.as_deref(),
        b.metadata.live_photo_id.as_deref(),
    ) {
        (Some(ia), Some(ib)) => ia == ib,
        _ => false,
    };

    facts.different_format = a.format != b.format;

    facts.same_location = match (a.metadata.gps, b.metadata.gps) {
        // Roughly 11 metres, which is finer than consumer GPS accuracy.
        (Some(ga), Some(gb)) => {
            (ga.latitude - gb.latitude).abs() < 0.0001
                && (ga.longitude - gb.longitude).abs() < 0.0001
        }
        _ => false,
    };

    facts
}

fn sorted_dimensions(file: &MediaFile) -> (u32, u32) {
    let width = file.metadata.width.unwrap_or(0);
    let height = file.metadata.height.unwrap_or(0);
    if width >= height {
        (width, height)
    } else {
        (height, width)
    }
}

/// A file name reduced to the part an exporter would not have changed.
///
/// `IMG_1234.HEIC`, `img_1234.jpg` and `IMG_1234(1).JPG` all reduce to
/// `img_1234`.
pub fn normalised_stem(file: &MediaFile) -> String {
    let stem = file.stem();

    // Remove a trailing export counter such as `(1)`, but nothing else. Digits
    // elsewhere in the name are the frame number and must be kept, or every
    // photo from one camera would look like every other.
    let without_counter = match stem.rfind('(') {
        Some(open)
            if open > 0
                && stem.ends_with(')')
                && stem[open + 1..stem.len() - 1]
                    .chars()
                    .all(|c| c.is_ascii_digit()) =>
        {
            &stem[..open]
        }
        _ => stem.as_str(),
    };

    let trimmed = without_counter.trim_end_matches(['-', '_', ' ']);
    // `1234.jpg` must not collapse to an empty string that matches everything.
    if trimmed.is_empty() {
        stem
    } else {
        trimmed.to_string()
    }
}

fn describe(facts: &Facts) -> Vec<Evidence> {
    let mut evidence = Vec::new();
    if let Some(distance) = facts.visual {
        evidence.push(Evidence::VisualDistance(distance));
    }
    if let Some(distance) = facts.detail {
        evidence.push(Evidence::DetailDistance(distance));
    }
    if facts.same_dimensions {
        evidence.push(Evidence::SameDimensions);
    }
    if facts.same_aspect {
        evidence.push(Evidence::SameAspectRatio);
    }
    match facts.capture_delta {
        Some(delta) => evidence.push(Evidence::CaptureTimeDelta(delta)),
        None => evidence.push(Evidence::CaptureTimeMissing),
    }
    if facts.same_camera {
        evidence.push(Evidence::SameCamera);
    }
    if facts.same_stem {
        evidence.push(Evidence::SameBaseFilename);
    }
    if facts.same_live_photo_id {
        evidence.push(Evidence::SameLivePhotoId);
    }
    if facts.same_location {
        evidence.push(Evidence::SameLocation);
    }
    if facts.different_format {
        evidence.push(Evidence::DifferentFormat);
    }
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    fn verdict(specs: &[testing::FileSpec]) -> Option<Verdict> {
        let (library, config) = testing::library(specs);
        compare(library.get(0), library.get(1), &config)
    }

    #[test]
    fn a_heic_and_its_google_jpeg_export_merge() {
        let result = verdict(&[
            testing::file("/phone/IMG_1234.HEIC")
                .dimensions(4032, 3024)
                .captured_at(1_700_000_000)
                .camera("Apple", "iPhone 15 Pro")
                .blockhash([1; 32]),
            testing::file("/google/IMG_1234.JPG")
                .dimensions(4032, 3024)
                .captured_at(1_700_000_001)
                .camera("Apple", "iPhone 15 Pro")
                .blockhash([1; 32]),
        ])
        .expect("these should be compared");
        assert_eq!(result.confidence, MatchConfidence::VeryHigh);
    }

    /// A block hash differing from all-zero in exactly `bits` places.
    fn blockhash_with_bits(bits: u32) -> [u8; 32] {
        let mut hash = [0u8; 32];
        for bit in 0..bits as usize {
            hash[bit / 8] |= 1 << (bit % 8);
        }
        hash
    }

    #[test]
    fn burst_frames_are_not_merged() {
        // Same instant, same camera, same size, but the pictures really do
        // differ, and the block hash says so.
        let result = verdict(&[
            testing::file("/phone/IMG_1234.HEIC")
                .dimensions(4032, 3024)
                .captured_at(1_700_000_000)
                .camera("Apple", "iPhone 15 Pro")
                .blockhash([0x00; 32]),
            testing::file("/phone/IMG_1235.HEIC")
                .dimensions(4032, 3024)
                .captured_at(1_700_000_000)
                .camera("Apple", "iPhone 15 Pro")
                .blockhash(blockhash_with_bits(20)),
        ])
        .expect("these should be compared");
        assert!(
            !result.confidence.is_mergeable(),
            "a burst must never be merged: {result:?}"
        );
    }

    /// Regression test built from real photographs. `IMG_6691.JPG` and
    /// `IMG_6692.JPG` came off one iPhone one second apart: same camera, same
    /// dimensions, block hashes eight bits apart. They are two photographs, and
    /// an earlier version of these rules merged them.
    #[test]
    fn consecutive_shots_of_one_scene_are_never_merged() {
        let result = verdict(&[
            testing::file("/google/IMG_6691.JPG")
                .dimensions(4032, 3024)
                .captured_at(1_787_341_899)
                .camera("Apple", "iPhone 16 Pro")
                .blockhash([0x00; 32]),
            testing::file("/google/IMG_6692.JPG")
                .dimensions(4032, 3024)
                .captured_at(1_787_341_900)
                .camera("Apple", "iPhone 16 Pro")
                .blockhash(blockhash_with_bits(8)),
        ])
        .expect("these should still be reported");
        assert!(
            !result.confidence.is_mergeable(),
            "two shutter presses are two photographs: {result:?}"
        );
    }

    /// Visual likeness alone must never merge, even at distance zero.
    #[test]
    fn identical_looking_frames_with_no_shared_name_or_format_are_kept() {
        let result = verdict(&[
            testing::file("/a/IMG_0001.JPG")
                .dimensions(4032, 3024)
                .captured_at(1_700_000_000)
                .camera("Apple", "iPhone 15 Pro")
                .blockhash([0x00; 32]),
            testing::file("/a/IMG_0002.JPG")
                .dimensions(4032, 3024)
                .captured_at(1_700_000_000)
                .camera("Apple", "iPhone 15 Pro")
                .blockhash([0x00; 32]),
        ])
        .expect("these should still be reported");
        assert!(!result.confidence.is_mergeable(), "{result:?}");
    }

    /// One capture written as HEIC and as JPEG shares its timestamp exactly, and
    /// a camera never writes a burst in two different formats.
    #[test]
    fn one_capture_in_two_formats_merges_even_under_different_names() {
        let result = verdict(&[
            testing::file("/phone/IMG_1234.HEIC")
                .dimensions(4032, 3024)
                .captured_at(1_700_000_000)
                .camera("Apple", "iPhone 15 Pro")
                .blockhash([0x00; 32]),
            testing::file("/google/20221114_093320.jpg")
                .dimensions(4032, 3024)
                .captured_at(1_700_000_000)
                .camera("Apple", "iPhone 15 Pro")
                .blockhash(blockhash_with_bits(3)),
        ])
        .expect("these should be compared");
        assert_eq!(result.confidence, MatchConfidence::VeryHigh, "{result:?}");
    }

    /// The case that drove the design: a real iPhone HEIC and the JPEG exported
    /// from it had gradient hashes 7 bits apart but block hashes only 2 apart.
    #[test]
    fn a_heic_matches_its_jpeg_export_even_when_the_gradient_hashes_disagree() {
        let result = verdict(&[
            testing::file("/phone/IMG_9460.HEIC")
                .dimensions(3024, 4032)
                .captured_at(1_787_407_667)
                .camera("Apple", "iPhone 17 Pro")
                .fingerprint(0x104b_c6ec_2e72_e240)
                .blockhash([0x00; 32]),
            testing::file("/google/IMG_9460.JPG")
                .dimensions(3024, 4032)
                .captured_at(1_787_407_667)
                .camera("Apple", "iPhone 17 Pro")
                .fingerprint(0x5853_c6ec_2e62_c340)
                .blockhash(blockhash_with_bits(2)),
        ])
        .expect("these should be compared");
        assert_eq!(result.confidence, MatchConfidence::VeryHigh, "{result:?}");
    }

    #[test]
    fn the_same_looking_picture_years_apart_stays_uncertain() {
        let result = verdict(&[
            testing::file("/a/wallpaper.jpg")
                .dimensions(1920, 1080)
                .captured_at(1_400_000_000)
                .blockhash([0; 32]),
            testing::file("/b/wallpaper.jpg")
                .dimensions(1920, 1080)
                .captured_at(1_700_000_000)
                .blockhash([0; 32]),
        ])
        .expect("these should be compared");
        assert_eq!(result.confidence, MatchConfidence::Uncertain);
    }

    #[test]
    fn a_crop_is_not_a_duplicate() {
        assert!(
            verdict(&[
                testing::file("/a/IMG_1.JPG")
                    .dimensions(4032, 3024)
                    .blockhash([0; 32]),
                testing::file("/b/IMG_1.JPG")
                    .dimensions(3024, 3024)
                    .blockhash([0; 32]),
            ])
            .is_none(),
            "a different shape means a different picture"
        );
    }

    #[test]
    fn unrelated_pictures_are_not_even_candidates() {
        assert!(
            verdict(&[
                testing::file("/a/IMG_1.JPG")
                    .dimensions(100, 100)
                    .blockhash([0x00; 32]),
                testing::file("/b/IMG_2.JPG")
                    .dimensions(100, 100)
                    .blockhash([0xFF; 32]),
            ])
            .is_none()
        );
    }

    #[test]
    fn identical_bytes_win_before_anything_is_decoded() {
        let (mut library, config) = testing::library(&[
            testing::file("/a/one.jpg"),
            testing::file("/b/two-renamed.jpg"),
        ]);
        library.files[0].exact_hash = Some([9; 32]);
        library.files[1].exact_hash = Some([9; 32]);
        let result = compare(library.get(0), library.get(1), &config).unwrap();
        assert_eq!(result.confidence, MatchConfidence::Exact);
    }

    #[test]
    fn export_suffixes_are_stripped_from_the_compared_name() {
        let (library, _) = testing::library(&[
            testing::file("/a/IMG_1234.HEIC"),
            testing::file("/b/IMG_1234(1).JPG"),
            testing::file("/c/1234.jpg"),
        ]);
        assert_eq!(normalised_stem(library.get(0)), "img_1234");
        assert_eq!(normalised_stem(library.get(1)), "img_1234");
        assert_eq!(
            normalised_stem(library.get(2)),
            "1234",
            "an all-digit name must not collapse to nothing"
        );
    }
}
