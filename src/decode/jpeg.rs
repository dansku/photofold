//! JPEG decoding at reduced scale.
//!
//! `jpeg-decoder` can stop part-way through the inverse transform and emit the
//! image at 1/8, 1/4 or 1/2 size. That is far cheaper than decoding a 48 MP
//! photo and shrinking it afterwards, and it is the reason this crate is used
//! directly instead of going through `image`.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use image::{DynamicImage, GrayImage, RgbImage};
use jpeg_decoder::{Decoder, PixelFormat};

pub fn thumbnail(path: &Path, target: u32) -> Result<DynamicImage, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut decoder = Decoder::new(BufReader::new(file));
    decoder.read_info().map_err(|e| e.to_string())?;

    let target = target.min(u32::from(u16::MAX)) as u16;
    // The decoder picks the nearest scale it can actually produce and reports
    // the size it settled on.
    let (width, height) = decoder.scale(target, target).map_err(|e| e.to_string())?;
    let pixels = decoder.decode().map_err(|e| e.to_string())?;
    let info = decoder
        .info()
        .ok_or_else(|| "the JPEG had no image information".to_string())?;

    let (width, height) = (u32::from(width), u32::from(height));
    to_image(pixels, width, height, info.pixel_format)
}

fn to_image(
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    format: PixelFormat,
) -> Result<DynamicImage, String> {
    let expected = |channels: usize| width as usize * height as usize * channels;

    match format {
        PixelFormat::L8 => {
            if pixels.len() < expected(1) {
                return Err("the JPEG produced fewer pixels than its size claims".to_string());
            }
            GrayImage::from_raw(width, height, pixels)
                .map(DynamicImage::ImageLuma8)
                .ok_or_else(|| "could not build a grey image from the JPEG".to_string())
        }
        PixelFormat::L16 => {
            // Keep the high byte of each sample; the fingerprint does not need
            // more precision than that.
            let (samples, _) = pixels.as_chunks::<2>();
            let eight_bit: Vec<u8> = samples.iter().map(|sample| sample[1]).collect();
            GrayImage::from_raw(width, height, eight_bit)
                .map(DynamicImage::ImageLuma8)
                .ok_or_else(|| "could not build a grey image from the JPEG".to_string())
        }
        PixelFormat::RGB24 => {
            if pixels.len() < expected(3) {
                return Err("the JPEG produced fewer pixels than its size claims".to_string());
            }
            RgbImage::from_raw(width, height, pixels)
                .map(DynamicImage::ImageRgb8)
                .ok_or_else(|| "could not build a colour image from the JPEG".to_string())
        }
        PixelFormat::CMYK32 => {
            if pixels.len() < expected(4) {
                return Err("the JPEG produced fewer pixels than its size claims".to_string());
            }
            // Adobe CMYK JPEGs store inverted values. This is a rough
            // conversion, which is all a perceptual hash needs.
            let (samples, _) = pixels.as_chunks::<4>();
            let rgb: Vec<u8> = samples
                .iter()
                .flat_map(|p| {
                    let k = u32::from(p[3]);
                    [
                        (u32::from(p[0]) * k / 255) as u8,
                        (u32::from(p[1]) * k / 255) as u8,
                        (u32::from(p[2]) * k / 255) as u8,
                    ]
                })
                .collect();
            RgbImage::from_raw(width, height, rgb)
                .map(DynamicImage::ImageRgb8)
                .ok_or_else(|| "could not build a colour image from the JPEG".to_string())
        }
    }
}
