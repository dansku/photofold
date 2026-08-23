//! Reading Apple's Live Photo content identifier.
//!
//! A Live Photo is a still and a movie that share one identifier. Apple stores
//! it in two completely different places, and no Rust crate exposes either by
//! name, so both parsers live here:
//!
//! * **Stills** keep it in the Apple MakerNote, tag `0x0011`. The MakerNote is a
//!   TIFF-style directory embedded in EXIF, with its own byte order and its own
//!   offset base.
//! * **Movies** keep it in `moov/meta`, under the key
//!   `com.apple.quicktime.content.identifier`.
//!
//! Measured against a real Google Photos export during development: the stills
//! keep their identifier, but the exported movies have no `meta` box at all.
//! Identifier pairing is therefore a bonus, not something to rely on, and
//! [`crate::live_photo`] falls back to file names.

use std::fs::File;
use std::path::Path;

use crate::isobmff::{BoxSpan, find_box, meta_body_start, read_span};

/// The MakerNote tag that holds the Live Photo identifier.
const CONTENT_IDENTIFIER_TAG: u16 = 0x0011;

/// The QuickTime metadata key that holds the same value.
const CONTENT_IDENTIFIER_KEY: &[u8] = b"com.apple.quicktime.content.identifier";

/// Pull the content identifier out of an Apple MakerNote blob.
///
/// Layout, confirmed against an iPhone 17 Pro HEIC:
///
/// ```text
/// 0    "Apple iOS\0"
/// 10   version (2 bytes)
/// 12   byte order, "MM" or "II"
/// 14   entry count (2 bytes), then 12 bytes per entry
/// ```
///
/// Entry values longer than four bytes are stored at an offset counted from the
/// start of the blob, not from the start of the file.
pub fn content_identifier_from_makernote(blob: &[u8]) -> Option<String> {
    if blob.len() < 16 || !blob.starts_with(b"Apple iOS\0") {
        return None;
    }
    let big_endian = match &blob[12..14] {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };

    let read_u16 = |at: usize| -> Option<u16> {
        let bytes: [u8; 2] = blob.get(at..at + 2)?.try_into().ok()?;
        Some(if big_endian {
            u16::from_be_bytes(bytes)
        } else {
            u16::from_le_bytes(bytes)
        })
    };
    let read_u32 = |at: usize| -> Option<u32> {
        let bytes: [u8; 4] = blob.get(at..at + 4)?.try_into().ok()?;
        Some(if big_endian {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        })
    };

    let entries = read_u16(14)? as usize;
    for index in 0..entries {
        let entry = 16 + index * 12;
        if read_u16(entry)? != CONTENT_IDENTIFIER_TAG {
            continue;
        }
        // Type 2 is ASCII, one byte per element.
        if read_u16(entry + 2)? != 2 {
            return None;
        }
        let length = read_u32(entry + 4)? as usize;
        let bytes = if length <= 4 {
            blob.get(entry + 8..entry + 8 + length)?
        } else {
            let offset = read_u32(entry + 8)? as usize;
            blob.get(offset..offset.checked_add(length)?)?
        };
        return clean(bytes);
    }
    None
}

/// Pull the content identifier out of a QuickTime or MP4 movie.
///
/// Returns `None` whenever the movie has no Apple metadata, which is the normal
/// case for anything an exporter has been through.
pub fn content_identifier_from_movie(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();

    let moov = find_box(&mut file, 0, length, b"moov", 0)?;
    let meta = find_box(&mut file, moov.body, moov.end, b"meta", 1)
        // Some writers put `meta` inside `udta` instead of directly in `moov`.
        .or_else(|| {
            let udta = find_box(&mut file, moov.body, moov.end, b"udta", 1)?;
            find_box(&mut file, udta.body, udta.end, b"meta", 1)
        })?;

    // QuickTime `meta` is a plain container. ISO `meta` puts four bytes of
    // version and flags first. Guessing wrong misaligns everything below, so the
    // start is probed rather than assumed.
    let meta_body = meta_body_start(&mut file, meta.body, meta.end)?;

    let keys = find_box(&mut file, meta_body, meta.end, b"keys", 1)?;
    let index = key_index(&mut file, &keys)?;

    let ilst = find_box(&mut file, meta_body, meta.end, b"ilst", 1)?;
    value_at_index(&mut file, &ilst, index)
}

/// One-based position of the content-identifier key inside a `keys` box.
fn key_index(file: &mut File, keys: &BoxSpan) -> Option<u32> {
    let body = read_span(file, keys.body, keys.end)?;
    // Four bytes of version and flags, then a four-byte entry count.
    if body.len() < 8 {
        return None;
    }
    let count = u32::from_be_bytes(body[4..8].try_into().ok()?);

    let mut cursor = 8usize;
    for index in 1..=count {
        let size = u32::from_be_bytes(body.get(cursor..cursor + 4)?.try_into().ok()?) as usize;
        if size < 8 || cursor + size > body.len() {
            return None;
        }
        // Bytes 4..8 are the key namespace, usually `mdta`.
        if &body[cursor + 8..cursor + size] == CONTENT_IDENTIFIER_KEY {
            return Some(index);
        }
        cursor += size;
    }
    None
}

/// Read the text value stored at `index` inside an `ilst` box.
fn value_at_index(file: &mut File, ilst: &BoxSpan, index: u32) -> Option<String> {
    let body = read_span(file, ilst.body, ilst.end)?;
    let wanted = index.to_be_bytes();

    let mut cursor = 0usize;
    while cursor + 8 <= body.len() {
        let size = u32::from_be_bytes(body.get(cursor..cursor + 4)?.try_into().ok()?) as usize;
        if size < 8 || cursor + size > body.len() {
            return None;
        }
        if body[cursor + 4..cursor + 8] == wanted {
            // The value lives in a `data` child: four bytes of type indicator,
            // four of locale, then the payload.
            let inner = &body[cursor + 8..cursor + size];
            if inner.len() >= 16 && &inner[4..8] == b"data" {
                return clean(&inner[16..]);
            }
            return None;
        }
        cursor += size;
    }
    None
}

/// Turn raw bytes into an identifier, dropping padding and rejecting nonsense.
fn clean(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim_end_matches('\0').trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return None;
    }
    Some(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build an Apple MakerNote holding one ASCII entry at tag 0x0011.
    fn makernote(identifier: &str) -> Vec<u8> {
        let mut blob = Vec::new();
        blob.extend_from_slice(b"Apple iOS\0");
        blob.extend_from_slice(&[0x00, 0x01]); // version
        blob.extend_from_slice(b"MM"); // big endian
        blob.extend_from_slice(&1u16.to_be_bytes()); // one entry

        // The value sits after the directory, so its offset is known up front.
        let value_offset = 16 + 12 + 4;
        blob.extend_from_slice(&CONTENT_IDENTIFIER_TAG.to_be_bytes());
        blob.extend_from_slice(&2u16.to_be_bytes()); // ASCII
        blob.extend_from_slice(&((identifier.len() + 1) as u32).to_be_bytes());
        blob.extend_from_slice(&(value_offset as u32).to_be_bytes());
        blob.extend_from_slice(&0u32.to_be_bytes()); // next-directory pointer

        assert_eq!(blob.len(), value_offset);
        blob.extend_from_slice(identifier.as_bytes());
        blob.push(0);
        blob
    }

    #[test]
    fn a_real_makernote_layout_yields_the_identifier() {
        let id = "F0652AEA-5229-4BF7-A366-B4C79E90CA1C";
        assert_eq!(
            content_identifier_from_makernote(&makernote(id)),
            Some(id.to_string())
        );
    }

    #[test]
    fn a_makernote_from_another_maker_is_ignored() {
        assert_eq!(content_identifier_from_makernote(b"Nikon\0stuff"), None);
        assert_eq!(content_identifier_from_makernote(b""), None);
    }

    #[test]
    fn a_truncated_makernote_does_not_panic() {
        let full = makernote("ABC-123");
        for cut in 0..full.len() {
            let _ = content_identifier_from_makernote(&full[..cut]);
        }
    }

    /// Wrap `payload` in a box with the given name.
    fn atom(name: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(name);
        out.extend_from_slice(payload);
        out
    }

    /// Build a movie file whose `meta` box holds one keyed value.
    ///
    /// `iso_style` adds the four version-and-flags bytes that MP4 uses and
    /// QuickTime does not, so both layouts can be tested.
    fn movie(identifier: &str, iso_style: bool, inside_udta: bool) -> Vec<u8> {
        let mut keys = 0u32.to_be_bytes().to_vec(); // version and flags
        keys.extend_from_slice(&1u32.to_be_bytes()); // one key
        let mut key_entry = ((CONTENT_IDENTIFIER_KEY.len() + 8) as u32)
            .to_be_bytes()
            .to_vec();
        key_entry.extend_from_slice(b"mdta");
        key_entry.extend_from_slice(CONTENT_IDENTIFIER_KEY);
        keys.extend_from_slice(&key_entry);

        let mut data = 1u32.to_be_bytes().to_vec(); // type indicator: UTF-8
        data.extend_from_slice(&0u32.to_be_bytes()); // locale
        data.extend_from_slice(identifier.as_bytes());
        let item = atom(&1u32.to_be_bytes(), &atom(b"data", &data));

        let mut meta_body = Vec::new();
        if iso_style {
            meta_body.extend_from_slice(&0u32.to_be_bytes());
        }
        meta_body.extend_from_slice(&atom(b"hdlr", &[0u8; 24]));
        meta_body.extend_from_slice(&atom(b"keys", &keys));
        meta_body.extend_from_slice(&atom(b"ilst", &item));

        let meta = atom(b"meta", &meta_body);
        let inner = if inside_udta {
            atom(b"udta", &meta)
        } else {
            meta
        };

        let mut file = atom(b"ftyp", b"qt  \0\0\x02\0qt  ");
        file.extend_from_slice(&atom(b"mdat", &[0u8; 32]));
        file.extend_from_slice(&atom(b"moov", &inner));
        file
    }

    fn write_temp(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mov");
        File::create(&path).unwrap().write_all(bytes).unwrap();
        (dir, path)
    }

    #[test]
    fn a_quicktime_movie_yields_the_identifier() {
        let id = "F0652AEA-5229-4BF7-A366-B4C79E90CA1C";
        let (_dir, path) = write_temp(&movie(id, false, false));
        assert_eq!(content_identifier_from_movie(&path), Some(id.to_string()));
    }

    /// The four version-and-flags bytes in an MP4 `meta` box must not throw the
    /// walker off by four.
    #[test]
    fn an_mp4_style_meta_box_also_works() {
        let id = "AAAA1111-2222-3333-4444-555566667777";
        let (_dir, path) = write_temp(&movie(id, true, false));
        assert_eq!(content_identifier_from_movie(&path), Some(id.to_string()));
    }

    #[test]
    fn a_meta_box_nested_in_udta_also_works() {
        let id = "BBBB1111-2222-3333-4444-555566667777";
        let (_dir, path) = write_temp(&movie(id, false, true));
        assert_eq!(content_identifier_from_movie(&path), Some(id.to_string()));
    }

    #[test]
    fn a_movie_without_apple_metadata_returns_nothing() {
        let mut bytes = atom(b"ftyp", b"qt  \0\0\x02\0qt  ");
        bytes.extend_from_slice(&atom(b"moov", &atom(b"mvhd", &[0u8; 100])));
        let (_dir, path) = write_temp(&bytes);
        assert_eq!(content_identifier_from_movie(&path), None);
    }

    #[test]
    fn a_truncated_movie_does_not_panic() {
        let full = movie("ABC", false, false);
        for cut in (0..full.len()).step_by(3) {
            let (_dir, path) = write_temp(&full[..cut]);
            let _ = content_identifier_from_movie(&path);
        }
    }
}
