//! Choosing which pairs of files are worth comparing.
//!
//! Comparing every image against every other image is quadratic: 100,000 photos
//! would need five billion comparisons. Instead each file is filed into a few
//! buckets, and only files that share a bucket are ever compared.
//!
//! Three independent bucket keys are used, so a file that lacks one signal can
//! still be found through another:
//!
//! * **Fingerprint bands.** The 256-bit block hash is cut into nine bands. Two
//!   hashes that differ in at most eight bits must agree exactly on at least one
//!   band, because eight differences cannot cover nine bands. Matching on any
//!   band therefore finds every pair the matcher would merge at the default
//!   setting.
//! * **Capture time.** Files taken within the same short window.
//! * **File name.** Files whose names agree once export suffixes are removed.

use std::collections::{HashMap, HashSet};

use crate::cli::Config;
use crate::matching;
use crate::models::{FileId, Library, MediaKind};

/// Byte ranges of the nine block-hash bands. Together they cover all 32 bytes.
///
/// Nine bands catch every pair differing by eight bits or fewer, which is the
/// default merge threshold. A larger `--hash-distance` widens what the matcher
/// accepts but not what the bands find, so the extra recall then comes from the
/// capture-time and file-name buckets.
const BANDS: [(usize, usize); 9] = [
    (0, 4),
    (4, 8),
    (8, 12),
    (12, 16),
    (16, 20),
    (20, 24),
    (24, 28),
    (28, 30),
    (30, 32),
];

/// Capture-time bucket width, in seconds. A file also joins the next bucket, so
/// a pair straddling a boundary is still found.
const TIME_WINDOW_SECONDS: i64 = 10;

/// A bucket larger than this is not compared pairwise.
///
/// Buckets this big come from files that share a placeholder timestamp or a
/// featureless fingerprint, and comparing them would cost more than the rest of
/// the run. Skipping is safe: it can only miss a duplicate, never invent one.
/// Every skipped bucket is logged and counted.
const MAX_BUCKET: usize = 400;

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct BucketStats {
    pub buckets: u64,
    pub pairs: u64,
    /// Buckets that were too large to compare pairwise.
    pub oversized_buckets: u64,
    /// Files in those buckets, which may therefore have missed a match.
    pub files_in_oversized_buckets: u64,
}

/// All pairs worth comparing, sorted and free of repeats.
pub fn candidates(library: &Library, config: &Config) -> (Vec<(FileId, FileId)>, BucketStats) {
    let mut buckets: HashMap<BucketKey, Vec<FileId>> = HashMap::new();

    for file in &library.files {
        // Videos are matched by exact hash only in this version.
        if file.kind != MediaKind::Image {
            continue;
        }
        let Some(fingerprint) = file.fingerprint.as_ref() else {
            // No fingerprint means no visual comparison. The file is still kept
            // and still copied; it simply cannot be merged visually.
            continue;
        };

        for (index, band) in bands(&fingerprint.blockhash).into_iter().enumerate() {
            buckets
                .entry(BucketKey::Band(index as u8, band))
                .or_default()
                .push(file.id);
        }

        if let Some(captured) = file.metadata.capture_time {
            let window = captured.div_euclid(TIME_WINDOW_SECONDS);
            buckets
                .entry(BucketKey::Time(window))
                .or_default()
                .push(file.id);
            buckets
                .entry(BucketKey::Time(window + 1))
                .or_default()
                .push(file.id);
        }

        buckets
            .entry(BucketKey::Name(matching::normalised_stem(file)))
            .or_default()
            .push(file.id);
    }

    let mut stats = BucketStats {
        buckets: buckets.len() as u64,
        ..BucketStats::default()
    };
    let mut pairs: HashSet<(FileId, FileId)> = HashSet::new();

    for (key, members) in buckets {
        if members.len() < 2 {
            continue;
        }
        if members.len() > MAX_BUCKET {
            stats.oversized_buckets += 1;
            stats.files_in_oversized_buckets += members.len() as u64;
            tracing::warn!(
                bucket = ?key,
                files = members.len(),
                limit = MAX_BUCKET,
                "bucket too large to compare pairwise; some duplicates in it may be missed"
            );
            continue;
        }
        for (index, &a) in members.iter().enumerate() {
            for &b in &members[index + 1..] {
                pairs.insert(if a < b { (a, b) } else { (b, a) });
            }
        }
    }

    let mut pairs: Vec<(FileId, FileId)> = pairs.into_iter().collect();
    pairs.sort_unstable();
    stats.pairs = pairs.len() as u64;
    let _ = config;
    (pairs, stats)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum BucketKey {
    /// Band index and the bits in that band.
    Band(u8, u64),
    /// Capture-time window number.
    Time(i64),
    /// File name with export suffixes removed.
    Name(String),
}

/// Cut a block hash into the bands described at the top of this module.
fn bands(hash: &[u8; 32]) -> [u64; BANDS.len()] {
    let mut out = [0u64; BANDS.len()];
    for (slot, (start, end)) in out.iter_mut().zip(BANDS) {
        let mut packed = 0u64;
        for byte in &hash[start..end] {
            packed = (packed << 8) | u64::from(*byte);
        }
        *slot = packed;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    #[test]
    fn the_bands_cover_every_byte_exactly_once() {
        let mut covered = [0u8; 32];
        for (start, end) in BANDS {
            for byte in covered.iter_mut().take(end).skip(start) {
                *byte += 1;
            }
        }
        assert!(covered.iter().all(|count| *count == 1));
    }

    /// The guarantee the whole design rests on: at most eight differing bits
    /// cannot touch all nine bands, so at least one band still matches exactly.
    #[test]
    fn any_eight_bit_difference_still_shares_a_band() {
        let base: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(37).wrapping_add(11));

        // Walk the differing bits through every band boundary rather than trying
        // all C(256,8) combinations, which is far too many to enumerate.
        for start in 0..256usize {
            let mut other = base;
            for step in 0..8usize {
                let bit = (start + step * 29) % 256;
                other[bit / 8] ^= 1 << (bit % 8);
            }
            let shared = bands(&base)
                .iter()
                .zip(bands(&other).iter())
                .any(|(x, y)| x == y);
            assert!(
                shared,
                "missed a pair with eight differing bits from {start}"
            );
        }
    }

    #[test]
    fn near_identical_photos_become_a_candidate_pair() {
        let (library, config) = testing::library(&[
            testing::file("/a/IMG_1.HEIC")
                .dimensions(4032, 3024)
                .captured_at(1_700_000_000)
                .blockhash([7; 32]),
            testing::file("/b/IMG_1.JPG")
                .dimensions(4032, 3024)
                .captured_at(1_700_000_001)
                // One bit different.
                .blockhash({
                    let mut hash = [7u8; 32];
                    hash[0] ^= 1;
                    hash
                }),
        ]);
        let (pairs, _) = candidates(&library, &config);
        assert_eq!(pairs, vec![(0, 1)]);
    }

    #[test]
    fn files_without_a_fingerprint_are_never_paired() {
        let (library, config) = testing::library(&[
            testing::file("/a/IMG_1.HEIC").dimensions(100, 100),
            testing::file("/b/IMG_1.HEIC").dimensions(100, 100),
        ]);
        let (pairs, _) = candidates(&library, &config);
        assert!(pairs.is_empty());
    }

    #[test]
    fn videos_are_left_to_exact_matching() {
        let (library, config) = testing::library(&[
            testing::file("/a/IMG_1.MOV").blockhash([1; 32]),
            testing::file("/b/IMG_1.MOV").blockhash([1; 32]),
        ]);
        let (pairs, _) = candidates(&library, &config);
        assert!(pairs.is_empty());
    }
}
