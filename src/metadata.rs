//! Reading metadata without decoding pixels.
//!
//! Metadata is supporting evidence, never a requirement. A photo with no EXIF at
//! all is normal (most PNG and WebP files have none) and must not be treated as
//! an error, so this module falls back to the image header for dimensions and
//! returns whatever it managed to learn.
//!
//! Dimensions deliberately come from here rather than from a decoder's output
//! buffer, because some decoders downscale or downconvert, and the quality
//! ranking must see the real size.

use std::path::Path;

use nom_exif::{EntryValue, Exif, ExifTag, MediaParser, MediaSource, TrackInfo, TrackInfoTag};

use crate::apple;
use crate::models::{FileFormat, GpsCoord, MediaMetadata, VideoMetadata};

/// Read everything cheap about a file.
///
/// Returns `Err` only when the file could not be opened or read at all. A file
/// that simply has no metadata comes back as an almost-empty [`MediaMetadata`].
pub fn extract(path: &Path, format: FileFormat) -> Result<MediaMetadata, String> {
    let mut metadata = MediaMetadata::default();

    if format.kind() == crate::models::MediaKind::Video {
        read_video(path, &mut metadata)?;
    } else {
        read_image(path, &mut metadata);

        // The container is the authority on size and bit depth. EXIF in a HEIC
        // can disagree with the real `ispe` box, and the quality ranking must
        // see the true resolution.
        if let Some(probe) = crate::decode::probe(path, format) {
            metadata.width = Some(probe.width);
            metadata.height = Some(probe.height);
            metadata.bit_depth = probe.bit_depth;
        }
    }

    Ok(metadata)
}

/// Read EXIF from a still image. Silent when there is none.
fn read_image(path: &Path, metadata: &mut MediaMetadata) {
    let mut parser = MediaParser::new();
    let Ok(source) = MediaSource::open(path) else {
        return;
    };
    // A file with no EXIF at all is normal, not a failure.
    let Ok(iter) = parser.parse_exif(source) else {
        return;
    };

    if let Ok(Some(gps)) = iter.parse_gps()
        && let (Some(latitude), Some(longitude)) = (gps.latitude_decimal(), gps.longitude_decimal())
    {
        metadata.gps = Some(GpsCoord {
            latitude,
            longitude,
        });
    }

    let exif: Exif = iter.into();

    metadata.camera_make = text(&exif, ExifTag::Make);
    metadata.camera_model = text(&exif, ExifTag::Model);
    metadata.software = text(&exif, ExifTag::Software);
    metadata.orientation = exif
        .get(ExifTag::Orientation)
        .and_then(number)
        .map(|n| n as u16);

    metadata.width = exif
        .get(ExifTag::ExifImageWidth)
        .and_then(number)
        .or_else(|| exif.get(ExifTag::ImageWidth).and_then(number))
        .map(|n| n as u32);
    metadata.height = exif
        .get(ExifTag::ExifImageHeight)
        .and_then(number)
        .or_else(|| exif.get(ExifTag::ImageHeight).and_then(number))
        .map(|n| n as u32);

    // DateTimeOriginal is when the shutter fired. The others are when the file
    // was written, which an export changes, so they are only fallbacks.
    metadata.capture_time = timestamp(&exif, ExifTag::DateTimeOriginal)
        .or_else(|| timestamp(&exif, ExifTag::CreateDate))
        .or_else(|| timestamp(&exif, ExifTag::ModifyDate));

    metadata.tag_count = count_tags(&exif);

    // The Live Photo identifier hides inside the Apple MakerNote, which arrives
    // as an opaque blob that no crate unpacks for us.
    if let Some(EntryValue::Undefined(blob)) = exif.get(ExifTag::MakerNote) {
        metadata.live_photo_id = apple::content_identifier_from_makernote(blob);
    }
}

/// Read track information from a QuickTime or MP4 container.
fn read_video(path: &Path, metadata: &mut MediaMetadata) -> Result<(), String> {
    let mut parser = MediaParser::new();
    let source = MediaSource::open(path).map_err(|e| e.to_string())?;
    let info: TrackInfo = parser.parse_track(source).map_err(|e| e.to_string())?;

    metadata.width = info
        .get(TrackInfoTag::Width)
        .and_then(number)
        .map(|n| n as u32);
    metadata.height = info
        .get(TrackInfoTag::Height)
        .and_then(number)
        .map(|n| n as u32);
    metadata.camera_make = info.get(TrackInfoTag::Make).and_then(as_text);
    metadata.camera_model = info.get(TrackInfoTag::Model).and_then(as_text);
    metadata.capture_time = info.get(TrackInfoTag::CreateDate).and_then(as_timestamp);

    let mut video = VideoMetadata {
        duration_ms: info.get(TrackInfoTag::DurationMs).and_then(number),
        codec: None,
        bitrate: None,
    };
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    crate::video::describe(path, size, &mut video);
    metadata.video = Some(video);
    metadata.tag_count = 4;
    // Present on movies straight off an iPhone, absent once an exporter has
    // rewritten the file.
    metadata.live_photo_id = apple::content_identifier_from_movie(path);
    Ok(())
}

fn text(exif: &Exif, tag: ExifTag) -> Option<String> {
    exif.get(tag).and_then(as_text)
}

fn as_text(value: &EntryValue) -> Option<String> {
    match value {
        EntryValue::Text(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        _ => None,
    }
}

/// Any integer-like entry as a `u64`.
fn number(value: &EntryValue) -> Option<u64> {
    match value {
        EntryValue::U32(n) => Some(u64::from(*n)),
        EntryValue::U16(n) => Some(u64::from(*n)),
        EntryValue::U8(n) => Some(u64::from(*n)),
        EntryValue::I32(n) => u64::try_from(*n).ok(),
        EntryValue::I16(n) => u64::try_from(*n).ok(),
        EntryValue::I64(n) => u64::try_from(*n).ok(),
        EntryValue::U64(n) => Some(*n),
        _ => None,
    }
}

fn timestamp(exif: &Exif, tag: ExifTag) -> Option<i64> {
    exif.get(tag).and_then(as_timestamp)
}

fn as_timestamp(value: &EntryValue) -> Option<i64> {
    match value {
        EntryValue::DateTime(time) => Some(time.timestamp()),
        // A naive time carries no zone. Reading it as UTC is good enough here:
        // the matcher only ever compares two timestamps, and a whole-hour offset
        // is far outside the few-second windows it uses.
        EntryValue::NaiveDateTime(time) => Some(time.and_utc().timestamp()),
        _ => None,
    }
}

fn count_tags(exif: &Exif) -> u32 {
    exif.iter().count() as u32
}
