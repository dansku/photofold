//! Turning a photo on disk into a small picture, as cheaply as the format allows.
//!
//! Nothing in the program needs a full-resolution image. Perceptual hashing
//! works on a 64x64 grid, so a 48-megapixel photo must never become a
//! 48-megapixel buffer. Each backend therefore asks its decoder for the smallest
//! picture it can produce:
//!
//! * JPEG uses the decoder's own 1/8, 1/4 and 1/2 scaling, which skips most of
//!   the work instead of doing it and throwing the result away.
//! * HEIC uses the embedded thumbnail when there is one.
//! * PNG, WebP and TIFF are decoded and then resized. These formats have no
//!   scaled-decode mode.
//!
//! Adding a format means adding a file here, not editing the pipeline.

#[cfg(feature = "heic")]
mod heif;
mod jpeg;
pub(crate) mod standard;

use std::path::Path;

use image::DynamicImage;

use crate::models::FileFormat;

/// Longest edge of the picture the decoders aim for.
///
/// Large enough that the detail hash still sees real structure, small enough
/// that a worker never holds more than a few hundred kilobytes of pixels.
pub const TARGET_EDGE: u32 = 256;

/// What a container says about an image, read without decoding pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeInfo {
    pub width: u32,
    pub height: u32,
    pub bit_depth: Option<u8>,
}

/// Read dimensions and bit depth from the container.
///
/// Returns `None` for formats whose header this build cannot read. Callers fall
/// back to EXIF in that case.
pub fn probe(path: &Path, format: FileFormat) -> Option<ProbeInfo> {
    match format {
        #[cfg(feature = "heic")]
        FileFormat::Heif => match heif::probe(path) {
            Ok(info) => Some(info),
            Err(message) => {
                tracing::debug!(path = %path.display(), %message, "could not probe the HEIC header");
                None
            }
        },
        FileFormat::Jpeg | FileFormat::Png | FileFormat::Webp | FileFormat::Tiff => {
            // Reads the header only; no pixels are decoded.
            image::image_dimensions(path)
                .ok()
                .map(|(width, height)| ProbeInfo {
                    width,
                    height,
                    bit_depth: None,
                })
        }
        _ => None,
    }
}

/// Decode a small version of `path`.
///
/// The returned picture is approximately [`TARGET_EDGE`] on its longest side.
/// The exact size varies by format and is not relied on: callers resize to a
/// fixed grid before hashing.
pub fn thumbnail(path: &Path, format: FileFormat, target: u32) -> Result<DynamicImage, String> {
    match format {
        FileFormat::Jpeg => jpeg::thumbnail(path, target),
        FileFormat::Png | FileFormat::Webp | FileFormat::Tiff => standard::thumbnail(path, target),
        #[cfg(feature = "heic")]
        FileFormat::Heif => heif::thumbnail(path, target),
        #[cfg(not(feature = "heic"))]
        FileFormat::Heif => {
            Err("this build has no HEIC support, so the picture was not compared".to_string())
        }
        other if other.is_raw() => Err(format!(
            "{} raw files are not decoded by this build",
            other.as_str().to_uppercase()
        )),
        other => Err(format!("{} is not an image format", other.as_str())),
    }
}

/// Whether the decoder already turned the picture upright.
///
/// libheif applies the file's own rotation and mirroring while decoding, so
/// applying the EXIF orientation afterwards would rotate it twice.
pub fn orientation_applied_by_decoder(format: FileFormat) -> bool {
    matches!(format, FileFormat::Heif)
}

/// Whether [`thumbnail`] can handle this format in this build.
///
/// Used to keep the error report free of entries for formats that were never
/// going to work.
pub fn can_decode(format: FileFormat) -> bool {
    matches!(
        format,
        FileFormat::Jpeg | FileFormat::Png | FileFormat::Webp | FileFormat::Tiff
    ) || (format == FileFormat::Heif && cfg!(feature = "heic"))
}
