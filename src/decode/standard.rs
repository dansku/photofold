//! PNG, WebP and TIFF decoding.
//!
//! These formats have no scaled-decode mode, so the full image is decoded once
//! and then shrunk immediately. The full buffer is dropped when this function
//! returns, so only one worker's worth of pixels is ever live.

use std::path::Path;

use image::{DynamicImage, imageops::FilterType};

pub fn thumbnail(path: &Path, target: u32) -> Result<DynamicImage, String> {
    let image = image::open(path).map_err(|e| e.to_string())?;
    Ok(shrink(image, target))
}

/// Scale down so the longest edge is at most `target`. Never scales up.
pub fn shrink(image: DynamicImage, target: u32) -> DynamicImage {
    let longest = image.width().max(image.height());
    if longest <= target {
        return image;
    }
    // Triangle filtering is good enough for a hash and much faster than Lanczos.
    image.resize(target, target, FilterType::Triangle)
}
