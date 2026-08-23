//! Core data types shared by every pipeline stage.
//!
//! Files live in one arena ([`Library::files`]) and are referred to elsewhere by
//! [`FileId`], which is an index into that arena. Later stages therefore move
//! small integers around instead of cloning paths and metadata.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Index into [`Library::files`].
pub type FileId = usize;

/// Index into [`Library::sources`].
pub type SourceId = usize;

/// What a file fundamentally is. Videos are never treated as images.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Image,
    Video,
    /// A metadata companion such as a Google Takeout `.json` file.
    Sidecar,
}

/// A recognised container format, derived from the file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileFormat {
    Jpeg,
    Png,
    Webp,
    Heif,
    Tiff,
    Dng,
    Cr2,
    Cr3,
    Nef,
    Arw,
    Mov,
    Mp4,
    M4v,
    JsonSidecar,
}

impl FileFormat {
    /// Every extension the scanner accepts, lowercase and without the dot.
    pub const ALL_EXTENSIONS: &'static [&'static str] = &[
        "jpg", "jpeg", "png", "webp", "heic", "heif", "tif", "tiff", "dng", "cr2", "cr3", "nef",
        "arw", "mov", "mp4", "m4v",
    ];

    pub fn from_extension(ext: &str) -> Option<Self> {
        // Callers may pass any case; normalise once here.
        let lower = ext.to_ascii_lowercase();
        Some(match lower.as_str() {
            "jpg" | "jpeg" => Self::Jpeg,
            "png" => Self::Png,
            "webp" => Self::Webp,
            "heic" | "heif" => Self::Heif,
            "tif" | "tiff" => Self::Tiff,
            "dng" => Self::Dng,
            "cr2" => Self::Cr2,
            "cr3" => Self::Cr3,
            "nef" => Self::Nef,
            "arw" => Self::Arw,
            "mov" => Self::Mov,
            "mp4" => Self::Mp4,
            "m4v" => Self::M4v,
            "json" => Self::JsonSidecar,
            _ => return None,
        })
    }

    pub fn from_path(path: &Path) -> Option<Self> {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(Self::from_extension)
    }

    /// Number of leading bytes [`FileFormat::sniff`] needs.
    pub const SNIFF_BYTES: usize = 16;

    /// Work out the real format from the first bytes of the file.
    ///
    /// This is not optional politeness. A Google Photos export can contain files
    /// named `IMG_1234.HEIC` that are plain JPEG inside, and in one real export
    /// checked during development 344 of 347 `.HEIC` files were JPEG. Trusting
    /// the extension there would send every one of them to the HEIC decoder,
    /// fail, and hand the JPEG a "HEIC is the original" bonus it has not earned.
    ///
    /// Returns `None` when the bytes match nothing known.
    pub fn sniff(header: &[u8]) -> Option<Self> {
        if header.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return Some(Self::Jpeg);
        }
        if header.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Some(Self::Png);
        }
        if header.len() >= 12 && &header[..4] == b"RIFF" && &header[8..12] == b"WEBP" {
            return Some(Self::Webp);
        }
        if header.starts_with(b"II*\0") || header.starts_with(b"MM\0*") {
            // Camera raw formats are TIFF containers too. The caller keeps the
            // extension's more specific answer in that case.
            return Some(Self::Tiff);
        }
        if header.len() >= 12 && &header[4..8] == b"ftyp" {
            return Some(match &header[8..12] {
                b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevm" | b"hevs" | b"mif1"
                | b"msf1" => Self::Heif,
                b"qt  " => Self::Mov,
                _ => Self::Mp4,
            });
        }
        None
    }

    /// Reconcile what the extension claims with what the bytes say.
    ///
    /// The bytes win, with two exceptions where the extension is more specific
    /// than the magic number can be:
    ///
    /// * Camera raw files are TIFF containers, so `.CR2` stays `Cr2`.
    /// * QuickTime and MP4 share a container, so `.MOV` stays `Mov`.
    pub fn reconcile(declared: Self, sniffed: Option<Self>) -> Self {
        let Some(sniffed) = sniffed else {
            return declared;
        };
        if sniffed == declared {
            return declared;
        }
        if sniffed == Self::Tiff && declared.is_raw() {
            return declared;
        }
        if sniffed.kind() == MediaKind::Video && declared.kind() == MediaKind::Video {
            return declared;
        }
        sniffed
    }

    pub fn kind(self) -> MediaKind {
        match self {
            Self::Mov | Self::Mp4 | Self::M4v => MediaKind::Video,
            Self::JsonSidecar => MediaKind::Sidecar,
            _ => MediaKind::Image,
        }
    }

    /// True for camera raw formats, which are never re-encoded by an exporter.
    pub fn is_raw(self) -> bool {
        matches!(
            self,
            Self::Dng | Self::Cr2 | Self::Cr3 | Self::Nef | Self::Arw
        )
    }

    /// True when the format stores pixels without generation loss.
    pub fn is_lossless(self) -> bool {
        matches!(self, Self::Png | Self::Tiff) || self.is_raw()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Png => "png",
            Self::Webp => "webp",
            Self::Heif => "heif",
            Self::Tiff => "tiff",
            Self::Dng => "dng",
            Self::Cr2 => "cr2",
            Self::Cr3 => "cr3",
            Self::Nef => "nef",
            Self::Arw => "arw",
            Self::Mov => "mov",
            Self::Mp4 => "mp4",
            Self::M4v => "m4v",
            Self::JsonSidecar => "json",
        }
    }
}

/// Metadata read from EXIF or from the container itself.
///
/// Dimensions and bit depth deliberately come from metadata or the container,
/// never from a decoder's output buffer: some decoders downconvert (10-bit HEIC
/// to 8-bit, for example) and would corrupt the quality score.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MediaMetadata {
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Capture time as a Unix timestamp in seconds.
    pub capture_time: Option<i64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    /// EXIF orientation tag, 1..=8.
    pub orientation: Option<u16>,
    pub bit_depth: Option<u8>,
    pub gps: Option<GpsCoord>,
    /// The `Software` EXIF tag. Its presence hints at an edited or re-exported file.
    pub software: Option<String>,
    /// Apple Live Photo content identifier, shared by a still and its movie.
    pub live_photo_id: Option<String>,
    /// Number of metadata tags found. Used as a "metadata richness" signal.
    pub tag_count: u32,
    pub video: Option<VideoMetadata>,
}

impl MediaMetadata {
    /// Total pixel count, if both dimensions are known.
    pub fn pixels(&self) -> Option<u64> {
        match (self.width, self.height) {
            (Some(w), Some(h)) => Some(u64::from(w) * u64::from(h)),
            _ => None,
        }
    }

    /// Aspect ratio, normalised so that rotating an image does not change it.
    pub fn aspect_ratio(&self) -> Option<f32> {
        match (self.width, self.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => {
                let (long, short) = if w >= h { (w, h) } else { (h, w) };
                Some(long as f32 / short as f32)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GpsCoord {
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VideoMetadata {
    pub duration_ms: Option<u64>,
    pub codec: Option<String>,
    pub bitrate: Option<u64>,
}

/// A compact, decode-free description of what an image looks like.
///
/// 40 bytes per image, so 200,000 images cost about 8 MB. Full pixel buffers are
/// dropped as soon as the fingerprint is computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisualFingerprint {
    /// 8x8 difference hash. Cheap, and robust to re-encoding.
    pub dhash: u64,
    /// 16x16 block-mean hash. Slower to compare, but far more selective, so it
    /// is used to confirm a dHash candidate rather than to find one.
    pub blockhash: [u8; 32],
}

impl VisualFingerprint {
    pub fn dhash_distance(&self, other: &Self) -> u32 {
        (self.dhash ^ other.dhash).count_ones()
    }

    pub fn blockhash_distance(&self, other: &Self) -> u32 {
        self.blockhash
            .iter()
            .zip(other.blockhash.iter())
            .map(|(a, b)| (a ^ b).count_ones())
            .sum()
    }
}

/// One file on disk, plus everything the pipeline learned about it.
#[derive(Debug, Clone)]
pub struct MediaFile {
    pub id: FileId,
    pub path: PathBuf,
    pub source: SourceId,
    pub kind: MediaKind,
    pub format: FileFormat,
    pub size: u64,
    /// Modification time as a Unix timestamp in seconds. Part of the cache key.
    pub modified_at: Option<i64>,
    /// Modification time in nanoseconds, for a cache key that survives fast edits.
    pub modified_nanos: Option<u32>,
    pub exact_hash: Option<[u8; 32]>,
    pub metadata: MediaMetadata,
    pub fingerprint: Option<VisualFingerprint>,
    /// Set when analysis failed. The file still travels through the pipeline and
    /// is still copied unless it is a confirmed duplicate.
    pub analysis_failed: bool,
    /// True when every measurement above came from the cache, so the hashing and
    /// analysis stages can skip this file entirely.
    pub from_cache: bool,
}

impl MediaFile {
    /// File name without the extension, lowercased.
    ///
    /// Used to pair `IMG_1234.HEIC` with `IMG_1234.JPG` and `IMG_1234.MOV`.
    pub fn stem(&self) -> String {
        self.path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
    }

    pub fn file_name(&self) -> &str {
        self.path.file_name().and_then(|s| s.to_str()).unwrap_or("")
    }
}

/// A source directory supplied on the command line.
#[derive(Debug, Clone)]
pub struct SourceRoot {
    pub id: SourceId,
    pub path: PathBuf,
    /// Higher wins ties. Zero means the user expressed no preference.
    pub preference: u32,
}

/// How confident the program is that two files are the same photo.
///
/// Declared in ascending order so that the derived [`Ord`] means "more
/// confident". Only `VeryHigh` and `Exact` cause an automatic merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchConfidence {
    Uncertain,
    Probable,
    VeryHigh,
    Exact,
}

impl MatchConfidence {
    /// Whether a match at this confidence may remove a file from the output.
    pub fn is_mergeable(self) -> bool {
        matches!(self, Self::Exact | Self::VeryHigh)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uncertain => "uncertain",
            Self::Probable => "probable",
            Self::VeryHigh => "very_high",
            Self::Exact => "exact",
        }
    }
}

/// A set of files judged to be the same photo, with the chosen winner.
#[derive(Debug, Clone)]
pub struct DuplicateGroup {
    pub id: String,
    /// All members, winner included.
    pub files: Vec<FileId>,
    pub winner: FileId,
    pub confidence: MatchConfidence,
    /// Human-readable justification, generated by the quality scorer.
    pub winner_reason: Vec<String>,
}

impl DuplicateGroup {
    pub fn losers(&self) -> impl Iterator<Item = FileId> + '_ {
        self.files.iter().copied().filter(|id| *id != self.winner)
    }
}

/// Everything the program scanned, in one place.
#[derive(Debug, Default)]
pub struct Library {
    pub sources: Vec<SourceRoot>,
    pub files: Vec<MediaFile>,
    /// AppleDouble resource forks (`._IMG_1234.HEIC`) that were skipped.
    pub skipped_appledouble: usize,
}

impl Library {
    pub fn get(&self, id: FileId) -> &MediaFile {
        &self.files[id]
    }

    pub fn source_of(&self, id: FileId) -> &SourceRoot {
        &self.sources[self.files[id].source]
    }

    pub fn iter_kind(&self, kind: MediaKind) -> impl Iterator<Item = &MediaFile> {
        self.files.iter().filter(move |f| f.kind == kind)
    }

    pub fn count_kind(&self, kind: MediaKind) -> usize {
        self.iter_kind(kind).count()
    }

    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

#[cfg(test)]
mod format_tests {
    use super::*;

    #[test]
    fn a_jpeg_named_heic_is_recognised_as_a_jpeg() {
        // Exactly what a Google Photos export produces.
        let header = b"\xFF\xD8\xFF\xE0\x00\x10JFIF\x00\x01";
        assert_eq!(FileFormat::sniff(header), Some(FileFormat::Jpeg));
        assert_eq!(
            FileFormat::reconcile(FileFormat::Heif, FileFormat::sniff(header)),
            FileFormat::Jpeg
        );
    }

    #[test]
    fn a_real_heic_is_recognised() {
        let header = b"\x00\x00\x00\x34ftypheic\x00\x00\x00\x00";
        assert_eq!(FileFormat::sniff(header), Some(FileFormat::Heif));
    }

    #[test]
    fn a_raw_file_keeps_its_specific_extension() {
        // CR2 files are TIFF containers, so the magic number cannot tell them apart.
        let header = b"II*\0\x10\0\0\0CR\x02\0\0\0\0\0\0";
        assert_eq!(
            FileFormat::reconcile(FileFormat::Cr2, FileFormat::sniff(header)),
            FileFormat::Cr2
        );
    }

    #[test]
    fn a_mov_keeps_its_extension_when_the_brand_says_mp4() {
        let header = b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00";
        assert_eq!(
            FileFormat::reconcile(FileFormat::Mov, FileFormat::sniff(header)),
            FileFormat::Mov
        );
    }

    #[test]
    fn unknown_bytes_leave_the_extension_alone() {
        assert_eq!(FileFormat::sniff(b"not a real header"), None);
        assert_eq!(
            FileFormat::reconcile(FileFormat::Jpeg, None),
            FileFormat::Jpeg
        );
    }

    #[test]
    fn png_and_webp_are_recognised() {
        assert_eq!(
            FileFormat::sniff(b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0d"),
            Some(FileFormat::Png)
        );
        assert_eq!(
            FileFormat::sniff(b"RIFF\x24\x00\x00\x00WEBPVP8 "),
            Some(FileFormat::Webp)
        );
    }
}
