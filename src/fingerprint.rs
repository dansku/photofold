//! Building a small, comparable description of what a picture looks like.
//!
//! Two hashes are produced for every image, because one is not enough:
//!
//! * A **gradient (difference) hash** of 64 bits. It survives re-encoding and
//!   format changes, and it is cheap to index, so it is what finds candidates.
//! * A **block-mean hash** of 256 bits. It is far more selective. It is not used
//!   to find candidates, only to confirm them, which is what keeps burst shots
//!   and other similar-but-different pictures apart.
//!
//! Both are computed from the same fixed 64x64 grey grid. Fixing the grid
//! matters: without it, a HEIC thumbnail of 512 pixels and a JPEG thumbnail of
//! 504 pixels would produce slightly different hashes for the same photo.

use std::path::Path;
use std::sync::OnceLock;

use image::{DynamicImage, imageops::FilterType};
use image_hasher::{HashAlg, Hasher, HasherConfig};

use crate::decode;
use crate::models::{FileFormat, VisualFingerprint};

/// Side of the square grid both hashes are computed from.
const GRID: u32 = 64;

fn gradient_hasher() -> &'static Hasher {
    static HASHER: OnceLock<Hasher> = OnceLock::new();
    HASHER.get_or_init(|| {
        HasherConfig::new()
            .hash_size(8, 8)
            .hash_alg(HashAlg::Gradient)
            .to_hasher()
    })
}

fn block_hasher() -> &'static Hasher {
    static HASHER: OnceLock<Hasher> = OnceLock::new();
    HASHER.get_or_init(|| {
        HasherConfig::new()
            .hash_size(16, 16)
            .hash_alg(HashAlg::Blockhash)
            .to_hasher()
    })
}

/// Decode `path` small and fingerprint it.
pub fn compute(
    path: &Path,
    format: FileFormat,
    orientation: Option<u16>,
) -> Result<VisualFingerprint, String> {
    let thumbnail = decode::thumbnail(path, format, decode::TARGET_EDGE)?;
    // Some decoders turn the picture upright themselves. Applying the EXIF
    // orientation on top of that would rotate it a second time.
    let orientation = if decode::orientation_applied_by_decoder(format) {
        None
    } else {
        orientation
    };
    Ok(from_image(&thumbnail, orientation))
}

/// Fingerprint a picture that is already in memory.
///
/// Split out from [`compute`] so that the hashing rules can be tested on
/// generated images without touching the disk.
pub fn from_image(image: &DynamicImage, orientation: Option<u16>) -> VisualFingerprint {
    // Some exporters rotate the pixels and drop the orientation tag, while the
    // camera original leaves the pixels alone and records the tag. Both are
    // turned upright here, or the same photo would fingerprint differently.
    let upright = apply_orientation(image, orientation);
    let grid = DynamicImage::ImageLuma8(
        upright
            .resize_exact(GRID, GRID, FilterType::Triangle)
            .to_luma8(),
    );

    VisualFingerprint {
        dhash: to_u64(gradient_hasher().hash_image(&grid).as_bytes()),
        blockhash: to_array(block_hasher().hash_image(&grid).as_bytes()),
    }
}

/// Turn a picture upright according to its EXIF orientation tag (1 to 8).
fn apply_orientation(image: &DynamicImage, orientation: Option<u16>) -> DynamicImage {
    match orientation.unwrap_or(1) {
        2 => image.fliph(),
        3 => image.rotate180(),
        4 => image.flipv(),
        5 => image.rotate90().fliph(),
        6 => image.rotate90(),
        7 => image.rotate270().fliph(),
        8 => image.rotate270(),
        // 1 is upright; anything else is a broken tag and is left alone.
        _ => image.clone(),
    }
}

/// First eight bytes of a hash as a `u64`. Missing bytes count as zero.
fn to_u64(bytes: &[u8]) -> u64 {
    let mut buffer = [0u8; 8];
    let taken = bytes.len().min(8);
    buffer[..taken].copy_from_slice(&bytes[..taken]);
    u64::from_le_bytes(buffer)
}

/// First 32 bytes of a hash. Missing bytes count as zero.
fn to_array(bytes: &[u8]) -> [u8; 32] {
    let mut buffer = [0u8; 32];
    let taken = bytes.len().min(32);
    buffer[..taken].copy_from_slice(&bytes[..taken]);
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    /// A picture with enough structure that a hash of it means something.
    fn scene(seed: u32) -> DynamicImage {
        let mut image = RgbImage::new(160, 120);
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            let r = ((x * 3 + seed * 37) % 256) as u8;
            let g = ((y * 5 + seed * 11) % 256) as u8;
            let b = (((x ^ y) + seed * 91) % 256) as u8;
            *pixel = Rgb([r, g, b]);
        }
        DynamicImage::ImageRgb8(image)
    }

    #[test]
    fn the_hashes_have_the_widths_the_matcher_assumes() {
        let bytes = gradient_hasher().hash_image(&scene(1)).as_bytes().len();
        assert_eq!(bytes, 8, "the gradient hash must be 64 bits");
        let bytes = block_hasher().hash_image(&scene(1)).as_bytes().len();
        assert_eq!(bytes, 32, "the block hash must be 256 bits");
    }

    #[test]
    fn the_same_picture_at_a_different_size_fingerprints_the_same() {
        let original = scene(7);
        let smaller = original.resize_exact(80, 60, FilterType::Triangle);

        let a = from_image(&original, None);
        let b = from_image(&smaller, None);
        assert!(
            a.dhash_distance(&b) <= 2,
            "rescaling must not change the fingerprint much: distance {}",
            a.dhash_distance(&b)
        );
    }

    #[test]
    fn different_pictures_fingerprint_differently() {
        let a = from_image(&scene(1), None);
        let b = from_image(&scene(2), None);
        assert!(
            a.dhash_distance(&b) > 4,
            "unrelated pictures must not collide: distance {}",
            a.dhash_distance(&b)
        );
    }

    #[test]
    fn a_rotated_copy_matches_once_its_orientation_tag_is_applied() {
        let upright = scene(3);
        let rotated = upright.rotate90();

        let a = from_image(&upright, None);
        // Orientation 8 means "rotate 270 to make it upright", which undoes the
        // 90-degree rotation above.
        let b = from_image(&rotated, Some(8));
        assert!(
            a.dhash_distance(&b) <= 4,
            "orientation must be undone before hashing: distance {}",
            a.dhash_distance(&b)
        );
    }
}
