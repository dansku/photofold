//! HEIC and HEIF support through the system `libheif`.
//!
//! Enabled by the `heic` feature, which is on by default. Building it needs
//! libheif and pkg-config on the machine; the README explains how to install
//! them. Without the feature, HEIC files are still scanned, hashed, copied and
//! read for EXIF. They simply get no visual fingerprint, so they are never
//! merged visually. That is the safe direction to fail in.
//!
//! Two details matter here:
//!
//! * iPhone HEIC files carry a small embedded thumbnail. Using it avoids
//!   decoding a 12-megapixel master for a 64x64 hash.
//! * libheif applies the file's own rotation and mirroring while decoding, so
//!   the caller must not apply the EXIF orientation again.

use std::path::Path;

use image::{DynamicImage, RgbImage};
use libheif_rs::{ColorSpace, DecodingOptions, HeifContext, ImageHandle, LibHeif, RgbChroma};

use crate::decode::ProbeInfo;

/// Smallest embedded thumbnail worth using instead of the full image.
///
/// The fingerprint grid is 64 pixels square, so a thumbnail somewhat larger than
/// that still carries every detail the hash can see.
const MIN_USABLE_THUMBNAIL: u32 = 96;

/// Read the real dimensions and bit depth without decoding any pixels.
///
/// The size comes from the container's `ispe` box, which is authoritative. EXIF
/// in a HEIC can disagree with it.
pub fn probe(path: &Path) -> Result<ProbeInfo, String> {
    let context = open(path)?;
    let handle = context
        .primary_image_handle()
        .map_err(|e| format!("could not read the HEIC image: {e}"))?;
    Ok(ProbeInfo {
        width: handle.width(),
        height: handle.height(),
        bit_depth: Some(handle.luma_bits_per_pixel()),
    })
}

pub fn thumbnail(path: &Path, target: u32) -> Result<DynamicImage, String> {
    let context = open(path)?;
    let handle = context
        .primary_image_handle()
        .map_err(|e| format!("could not read the HEIC image: {e}"))?;

    let source = embedded_thumbnail(&handle).unwrap_or(handle);
    let library = LibHeif::new();

    let mut options =
        DecodingOptions::new().ok_or_else(|| "could not set up the HEIC decoder".to_string())?;
    // A 10-bit HDR photo would otherwise arrive as 16-bit samples.
    options.set_convert_hdr_to_8bit(true);

    let decoded = library
        .decode(&source, ColorSpace::Rgb(RgbChroma::Rgb), Some(options))
        .map_err(|e| format!("could not decode the HEIC image: {e}"))?;

    let image = to_rgb_image(&decoded)?;
    Ok(crate::decode::standard::shrink(image, target))
}

fn open(path: &Path) -> Result<HeifContext<'static>, String> {
    let path = path
        .to_str()
        .ok_or_else(|| "the file name is not valid text".to_string())?;
    HeifContext::read_from_file(path).map_err(|e| format!("could not open the HEIC file: {e}"))
}

/// The largest embedded thumbnail that is still big enough to hash.
fn embedded_thumbnail(handle: &ImageHandle) -> Option<ImageHandle> {
    let count = handle.number_of_thumbnails();
    if count == 0 {
        return None;
    }
    let mut ids = vec![0; count];
    handle.thumbnail_ids(&mut ids);

    ids.into_iter()
        .filter_map(|id| handle.thumbnail(id).ok())
        .filter(|thumbnail| thumbnail.width().min(thumbnail.height()) >= MIN_USABLE_THUMBNAIL)
        .max_by_key(|thumbnail| thumbnail.width() * thumbnail.height())
}

/// Copy libheif's interleaved plane into an `image` buffer, honouring the stride.
///
/// libheif pads each row to an alignment boundary, so the rows cannot simply be
/// concatenated.
fn to_rgb_image(decoded: &libheif_rs::Image) -> Result<DynamicImage, String> {
    let planes = decoded.planes();
    let plane = planes
        .interleaved
        .ok_or_else(|| "the decoded HEIC image had no pixel data".to_string())?;

    let width = plane.width as usize;
    let height = plane.height as usize;
    let mut pixels = Vec::with_capacity(width * height * 3);
    for row in 0..height {
        let start = row * plane.stride;
        let end = start + width * 3;
        let Some(slice) = plane.data.get(start..end) else {
            return Err("the decoded HEIC image ended early".to_string());
        };
        pixels.extend_from_slice(slice);
    }

    RgbImage::from_raw(plane.width, plane.height, pixels)
        .map(DynamicImage::ImageRgb8)
        .ok_or_else(|| "could not build an image from the decoded HEIC data".to_string())
}
