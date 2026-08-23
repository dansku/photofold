//! Print what photofold sees in a file: format, size, fingerprint.
//!
//! Useful when a pair of photos was expected to match and did not.
//!
//! ```text
//! cargo run --example inspect -- a.heic b.jpg
//! ```

use std::path::PathBuf;

use photofold::models::FileFormat;
use photofold::{decode, fingerprint, metadata};

fn main() {
    let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let mut prints = Vec::new();

    for path in &paths {
        let declared = FileFormat::from_path(path).expect("unsupported extension");
        let header = std::fs::read(path).unwrap_or_default();
        let format = FileFormat::reconcile(
            declared,
            FileFormat::sniff(&header[..header.len().min(FileFormat::SNIFF_BYTES)]),
        );
        println!("{}", path.display());
        println!(
            "  declared={} actual={}",
            declared.as_str(),
            format.as_str()
        );

        if let Some(probe) = decode::probe(path, format) {
            println!(
                "  probe: {}x{} depth={:?}",
                probe.width, probe.height, probe.bit_depth
            );
        }
        match metadata::extract(path, format) {
            Ok(meta) => println!(
                "  meta: {:?}x{:?} orient={:?} time={:?} camera={:?} tags={}",
                meta.width,
                meta.height,
                meta.orientation,
                meta.capture_time,
                meta.camera_model,
                meta.tag_count
            ),
            Err(err) => println!("  meta failed: {err}"),
        }
        if let Ok(meta) = metadata::extract(path, format) {
            println!("  live photo id: {:?}", meta.live_photo_id);
            if let Some(video) = &meta.video {
                println!(
                    "  video: {:?} ms, codec {:?}, {:?} bit/s",
                    video.duration_ms, video.codec, video.bitrate
                );
            }
        }
        match decode::thumbnail(path, format, decode::TARGET_EDGE) {
            Ok(image) => println!("  thumbnail: {}x{}", image.width(), image.height()),
            Err(err) => println!("  thumbnail failed: {err}"),
        }
        let orientation = metadata::extract(path, format)
            .ok()
            .and_then(|m| m.orientation);
        match fingerprint::compute(path, format, orientation) {
            Ok(print) => {
                println!("  dhash={:#018x}", print.dhash);
                prints.push((path.clone(), print));
            }
            Err(err) => println!("  fingerprint failed: {err}"),
        }
    }

    println!("\ndistances:");
    for i in 0..prints.len() {
        for j in (i + 1)..prints.len() {
            println!(
                "  {} vs {}: visual={} detail={}",
                prints[i].0.file_name().unwrap().to_string_lossy(),
                prints[j].0.file_name().unwrap().to_string_lossy(),
                prints[i].1.dhash_distance(&prints[j].1),
                prints[i].1.blockhash_distance(&prints[j].1),
            );
        }
    }
}
