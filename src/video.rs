//! Reading what a movie file is made of.
//!
//! `nom-exif` supplies a movie's duration, dimensions and creation time, but not
//! its codec. The codec is a four-character code in the sample description box,
//! which is a short walk down the container, so it is read here rather than by
//! adding a dependency for one field.
//!
//! Near-duplicate video matching is deliberately not implemented. Videos are
//! deduplicated on their exact hash only. The fields collected here describe a
//! video in the report and give a later version something to compare on.

use std::fs::File;
use std::path::Path;

use crate::isobmff::{find_box, read_box_header, read_span};
use crate::models::VideoMetadata;

/// Sample description entries that are not really a video codec.
const NON_VIDEO_CODECS: &[&[u8; 4]] = &[b"mp4a", b"sowt", b"twos", b"alac", b"tmcd", b"mebx"];

/// Fill in the fields `nom-exif` does not provide.
///
/// Everything here is best effort. A movie that cannot be walked keeps whatever
/// the EXIF reader already found.
pub fn describe(path: &Path, size: u64, metadata: &mut VideoMetadata) {
    metadata.codec = codec(path);
    metadata.bitrate = match metadata.duration_ms {
        // Bits per second over the whole file, container overhead included. It
        // is an estimate, and only ever used to describe a file, never to decide
        // that two files are the same.
        Some(duration) if duration > 0 => Some(size.saturating_mul(8_000) / duration),
        _ => None,
    };
}

/// The four-character code of the first video track's sample description.
fn codec(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();

    let moov = find_box(&mut file, 0, length, b"moov", 0)?;

    // A file has one box per track, and the first is not always the video, so
    // every track is tried until one yields a video codec.
    let mut cursor = moov.body;
    while cursor + 8 <= moov.end {
        let (size, name, body) = read_box_header(&mut file, cursor)?;
        let end = cursor.checked_add(size)?;
        if end > moov.end {
            return None;
        }
        if &name == b"trak"
            && let Some(found) = codec_in_track(&mut file, body, end)
        {
            return Some(found);
        }
        cursor = end;
    }
    None
}

fn codec_in_track(file: &mut File, start: u64, end: u64) -> Option<String> {
    let mdia = find_box(file, start, end, b"mdia", 1)?;
    let minf = find_box(file, mdia.body, mdia.end, b"minf", 2)?;
    let stbl = find_box(file, minf.body, minf.end, b"stbl", 3)?;
    let stsd = find_box(file, stbl.body, stbl.end, b"stsd", 4)?;

    // Four bytes of version and flags, four of entry count, then the entries,
    // each of which starts with its own size and four-character format.
    let body = read_span(file, stsd.body, stsd.end)?;
    let format: [u8; 4] = body.get(12..16)?.try_into().ok()?;

    if NON_VIDEO_CODECS.contains(&&format) {
        return None;
    }
    let text: String = format
        .iter()
        .map(|byte| *byte as char)
        .filter(|c| c.is_ascii_graphic())
        .collect();
    if text.len() == 4 { Some(text) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn atom(name: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(name);
        out.extend_from_slice(payload);
        out
    }

    /// A movie with an audio track first and a video track second, which is the
    /// order that trips up a reader that only looks at the first track.
    fn movie(codecs: &[&[u8; 4]]) -> Vec<u8> {
        let mut moov = atom(b"mvhd", &[0u8; 100]);
        for format in codecs {
            let mut stsd = 0u32.to_be_bytes().to_vec(); // version and flags
            stsd.extend_from_slice(&1u32.to_be_bytes()); // one entry
            stsd.extend_from_slice(&40u32.to_be_bytes()); // entry size
            stsd.extend_from_slice(*format);
            stsd.extend_from_slice(&[0u8; 28]);

            let stbl = atom(b"stbl", &atom(b"stsd", &stsd));
            let minf = atom(b"minf", &stbl);
            let mdia = atom(b"mdia", &minf);
            moov.extend_from_slice(&atom(b"trak", &mdia));
        }

        let mut file = atom(b"ftyp", b"qt  \0\0\x02\0qt  ");
        file.extend_from_slice(&atom(b"moov", &moov));
        file
    }

    fn write_temp(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mov");
        File::create(&path).unwrap().write_all(bytes).unwrap();
        (dir, path)
    }

    #[test]
    fn the_video_codec_is_found_past_an_audio_track() {
        let (_dir, path) = write_temp(&movie(&[b"mp4a", b"hvc1"]));
        assert_eq!(codec(&path).as_deref(), Some("hvc1"));
    }

    #[test]
    fn an_audio_only_file_reports_no_video_codec() {
        let (_dir, path) = write_temp(&movie(&[b"mp4a"]));
        assert_eq!(codec(&path), None);
    }

    #[test]
    fn bitrate_is_estimated_from_size_and_duration() {
        let mut metadata = VideoMetadata {
            duration_ms: Some(2_000),
            ..VideoMetadata::default()
        };
        let (_dir, path) = write_temp(&movie(&[b"avc1"]));
        describe(&path, 1_000_000, &mut metadata);
        assert_eq!(metadata.codec.as_deref(), Some("avc1"));
        assert_eq!(metadata.bitrate, Some(4_000_000));
    }

    #[test]
    fn a_zero_length_movie_reports_no_bitrate() {
        let mut metadata = VideoMetadata {
            duration_ms: Some(0),
            ..VideoMetadata::default()
        };
        let (_dir, path) = write_temp(&movie(&[b"avc1"]));
        describe(&path, 1_000, &mut metadata);
        assert_eq!(metadata.bitrate, None);
    }

    #[test]
    fn a_truncated_movie_does_not_panic() {
        let full = movie(&[b"avc1"]);
        for cut in (0..full.len()).step_by(5) {
            let (_dir, path) = write_temp(&full[..cut]);
            let _ = codec(&path);
        }
    }
}
