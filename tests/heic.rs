//! HEIC tests. Only built when the `heic` feature is on, which is the default.
//!
//! The fixtures are encoded here rather than committed, so the repository stays
//! free of large binary files. If the installed libheif has no HEIC encoder the
//! test says so and stops, rather than failing for a reason that has nothing to
//! do with photofold.

#![cfg(feature = "heic")]

mod common;

use common::{Workspace, config, run};
use libheif_rs::{
    Channel, ColorSpace, CompressionFormat, EncoderQuality, HeifContext, Image, LibHeif, RgbChroma,
};

/// Encode a generated picture as a real HEIC file.
///
/// Returns `None` when this machine's libheif cannot write HEIC.
fn heic(seed: u32, width: u32, height: u32) -> Option<Vec<u8>> {
    let picture = common::scene(seed, width, height);

    let library = LibHeif::new();
    let mut encoder = library.encoder_for_format(CompressionFormat::Hevc).ok()?;
    encoder.set_quality(EncoderQuality::Lossy(90)).ok()?;

    let mut image = Image::new(width, height, ColorSpace::Rgb(RgbChroma::Rgb)).ok()?;
    image
        .create_plane(Channel::Interleaved, width, height, 8)
        .ok()?;
    {
        let mut planes = image.planes_mut();
        let plane = planes.interleaved.as_mut()?;
        for y in 0..height as usize {
            let row = y * plane.stride;
            for x in 0..width as usize {
                let pixel = picture.get_pixel(x as u32, y as u32);
                plane.data[row + x * 3] = pixel[0];
                plane.data[row + x * 3 + 1] = pixel[1];
                plane.data[row + x * 3 + 2] = pixel[2];
            }
        }
    }

    let mut context = HeifContext::new().ok()?;
    context.encode_image(&image, &mut encoder, None).ok()?;
    context.write_to_bytes().ok()
}

#[test]
fn a_heic_and_its_jpeg_export_are_merged_and_the_heic_is_kept() {
    let Some(heic_bytes) = heic(101, 320, 240) else {
        eprintln!("skipped: this libheif build cannot encode HEIC");
        return;
    };

    let workspace = Workspace::new();
    workspace.write("phone/IMG_2001.HEIC", &heic_bytes);
    // What an exporter produces from the same photo: a JPEG under the same name.
    workspace.write("google/IMG_2001.JPG", &common::jpeg(101, 320, 240, 88));

    let config = config(
        &workspace,
        &[
            &workspace.path().join("phone").to_string_lossy(),
            &workspace.path().join("google").to_string_lossy(),
        ],
    );
    let outcome = run(&config);

    assert_eq!(
        outcome.report.summary.visual_duplicate_files, 1,
        "a HEIC and its JPEG export are one photo: {:#?}",
        outcome.report.groups
    );
    assert_eq!(
        workspace.output_names(),
        ["IMG_2001.HEIC"],
        "the HEIC original should win, and must not be converted"
    );
    assert_eq!(
        std::fs::read(workspace.path().join("out/IMG_2001.HEIC")).unwrap(),
        heic_bytes,
        "the kept file must be the original bytes, untouched"
    );
}

#[test]
fn heic_dimensions_come_from_the_container() {
    let Some(heic_bytes) = heic(102, 200, 160) else {
        eprintln!("skipped: this libheif build cannot encode HEIC");
        return;
    };

    let workspace = Workspace::new();
    workspace.write("a/IMG_2002.HEIC", &heic_bytes);

    let config = config(
        &workspace,
        &["--dry-run", &workspace.path().join("a").to_string_lossy()],
    );
    let outcome = run(&config);

    assert!(
        outcome.report.errors.is_empty(),
        "{:?}",
        outcome.report.errors
    );
    assert_eq!(outcome.report.summary.unique_photos, 1);
}
