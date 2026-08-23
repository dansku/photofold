//! Shared helpers for the end-to-end tests.
//!
//! Each test file includes this module separately, so any helper a given test
//! file does not use looks unused to the compiler. That is expected here.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser;
use photofold::cli::{Cli, Config};
use photofold::pipeline::{self, RunOutcome};
use photofold::progress::Progress;

/// A throwaway workspace with source folders and an output folder.
pub struct Workspace {
    pub root: tempfile::TempDir,
}

impl Workspace {
    pub fn new() -> Self {
        Self {
            root: tempfile::tempdir().expect("failed to create a temporary folder"),
        }
    }

    pub fn path(&self) -> &Path {
        self.root.path()
    }

    /// Write a file, creating parent folders as needed.
    pub fn write(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.root.path().join(relative);
        fs::create_dir_all(path.parent().expect("a file always has a parent"))
            .expect("failed to create a folder");
        fs::write(&path, contents).expect("failed to write a test file");
        path
    }

    pub fn dir(&self, relative: &str) -> PathBuf {
        let path = self.root.path().join(relative);
        fs::create_dir_all(&path).expect("failed to create a folder");
        path
    }

    /// File names in the output folder, sorted.
    pub fn output_names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.root.path().join("out"))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

/// A small picture with enough structure for a perceptual hash to mean
/// something. The same `seed` always gives the same picture.
pub fn scene(seed: u32, width: u32, height: u32) -> image::RgbImage {
    // Smooth gradients plus large blocks. JPEG stores this well at any quality,
    // so re-encoding it tests the matcher rather than the encoder. The slopes
    // and the block pattern both depend on the seed, so two seeds differ in
    // structure and not only in brightness.
    let slope_x = 1 + seed % 5;
    let slope_y = 1 + (seed * 7) % 5;
    let mut picture = image::RgbImage::new(width, height);
    for (x, y, pixel) in picture.enumerate_pixels_mut() {
        let r = ((x * slope_x) % 256) as u8;
        let g = ((y * slope_y) % 256) as u8;
        let b = (((x / 16 + y / 16 + seed) % 4) * 60) as u8;
        *pixel = image::Rgb([r, g, b]);
    }
    picture
}

/// Encode a generated picture as JPEG at the given quality.
///
/// Two different qualities give the same picture stored differently, which is
/// exactly the case the visual matcher exists to handle.
pub fn jpeg(seed: u32, width: u32, height: u32, quality: u8) -> Vec<u8> {
    let picture = scene(seed, width, height);
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(
        &mut std::io::Cursor::new(&mut bytes),
        quality,
    )
    .encode_image(&image::DynamicImage::ImageRgb8(picture))
    .expect("encoding a generated JPEG should not fail");
    bytes
}

/// The same picture as [`jpeg`], shrunk first.
///
/// This is what an exporter does when it produces a smaller copy, so the two
/// files really are the same photo at two resolutions.
pub fn jpeg_downscaled(
    seed: u32,
    width: u32,
    height: u32,
    new_width: u32,
    new_height: u32,
    quality: u8,
) -> Vec<u8> {
    let small = image::DynamicImage::ImageRgb8(scene(seed, width, height)).resize_exact(
        new_width,
        new_height,
        image::imageops::FilterType::Triangle,
    );
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(
        &mut std::io::Cursor::new(&mut bytes),
        quality,
    )
    .encode_image(&small)
    .expect("encoding a generated JPEG should not fail");
    bytes
}

/// Encode a generated picture as PNG.
pub fn png(seed: u32, width: u32, height: u32) -> Vec<u8> {
    let picture = scene(seed, width, height);
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(picture)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .expect("encoding a generated PNG should not fail");
    bytes
}

/// Wrap `payload` in an ISO base media box.
fn atom(name: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(name);
    out.extend_from_slice(payload);
    out
}

/// A small but genuine QuickTime movie: correct `ftyp`, a `mvhd` carrying the
/// duration, and optionally Apple's Live Photo content identifier.
///
/// Real bytes matter here. photofold identifies files by their contents, so a
/// text file named `.MOV` would correctly be reported as not a video at all.
pub fn mov(duration_ms: u64, content_identifier: Option<&str>) -> Vec<u8> {
    const TIMESCALE: u32 = 1000;

    let mut mvhd = vec![0u8; 4]; // version 0, no flags
    mvhd.extend_from_slice(&0u32.to_be_bytes()); // creation time
    mvhd.extend_from_slice(&0u32.to_be_bytes()); // modification time
    mvhd.extend_from_slice(&TIMESCALE.to_be_bytes());
    mvhd.extend_from_slice(&(duration_ms as u32).to_be_bytes());
    mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
    mvhd.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
    mvhd.extend_from_slice(&[0u8; 10]); // reserved
    // Unity transformation matrix.
    for value in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        mvhd.extend_from_slice(&value.to_be_bytes());
    }
    mvhd.extend_from_slice(&[0u8; 24]); // predefined
    mvhd.extend_from_slice(&2u32.to_be_bytes()); // next track id

    let mut moov_body = atom(b"mvhd", &mvhd);

    if let Some(identifier) = content_identifier {
        const KEY: &[u8] = b"com.apple.quicktime.content.identifier";

        let mut keys = 0u32.to_be_bytes().to_vec(); // version and flags
        keys.extend_from_slice(&1u32.to_be_bytes()); // one key
        let mut entry = ((KEY.len() + 8) as u32).to_be_bytes().to_vec();
        entry.extend_from_slice(b"mdta");
        entry.extend_from_slice(KEY);
        keys.extend_from_slice(&entry);

        let mut data = 1u32.to_be_bytes().to_vec(); // UTF-8
        data.extend_from_slice(&0u32.to_be_bytes()); // locale
        data.extend_from_slice(identifier.as_bytes());
        let item = atom(&1u32.to_be_bytes(), &atom(b"data", &data));

        let mut meta = atom(b"hdlr", &[0u8; 24]);
        meta.extend_from_slice(&atom(b"keys", &keys));
        meta.extend_from_slice(&atom(b"ilst", &item));
        moov_body.extend_from_slice(&atom(b"meta", &meta));
    }

    let mut file = atom(b"ftyp", b"qt  \0\0\x02\0qt  ");
    file.extend_from_slice(&atom(b"mdat", &[0u8; 64]));
    file.extend_from_slice(&atom(b"moov", &moov_body));
    file
}

/// Build a [`Config`] the same way the real command line would.
pub fn config(workspace: &Workspace, extra: &[&str]) -> Config {
    let output = workspace.root.path().join("out");
    let report = workspace.root.path().join("report.json");

    let mut argv: Vec<String> = vec!["photofold".to_string()];
    argv.extend(extra.iter().map(|s| s.to_string()));
    argv.push("--output".to_string());
    argv.push(output.to_string_lossy().into_owned());
    argv.push("--report".to_string());
    argv.push(report.to_string_lossy().into_owned());
    // Keep every test's cache inside its own workspace. Without this the tests
    // would share the user's real cache and interfere with each other.
    if !extra
        .iter()
        .any(|arg| *arg == "--no-cache" || *arg == "--cache-path")
    {
        argv.push("--cache-path".to_string());
        argv.push(
            workspace
                .root
                .path()
                .join("cache.sqlite")
                .to_string_lossy()
                .into_owned(),
        );
    }

    Cli::parse_from(argv)
        .into_config()
        .expect("the test arguments should be valid")
}

/// Build a [`Config`] from a complete argument list, output and report included.
pub fn config_from(argv: &[String]) -> Config {
    let mut full: Vec<String> = vec!["photofold".to_string()];
    full.extend(argv.iter().cloned());
    // These tests are about other behaviour; keep them off the shared cache.
    if !argv.iter().any(|arg| arg == "--cache-path") {
        full.push("--no-cache".to_string());
    }
    Cli::parse_from(full)
        .into_config()
        .expect("the test arguments should be valid")
}

pub fn run(config: &Config) -> RunOutcome {
    pipeline::run(config, &Progress::new(false)).expect("the run should succeed")
}
