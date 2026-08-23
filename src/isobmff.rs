//! A minimal reader for the ISO base media file format.
//!
//! MOV and MP4 files are trees of length-prefixed boxes. Only a few small pieces
//! of metadata are needed from them, so a full parser would be a large
//! dependency for very little gain. This walks the tree by seeking, never
//! reading more than one box body at a time, which matters when the file is a
//! four-gigabyte video.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

/// Stop walking a container this deep. Real files nest three or four levels.
pub const MAX_BOX_DEPTH: u32 = 8;

/// Refuse to buffer a metadata box larger than this. Guards against a corrupt
/// length field turning into a huge allocation.
pub const MAX_METADATA_BOX: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct BoxSpan {
    /// First byte after the header.
    pub body: u64,
    /// First byte after the whole box.
    pub end: u64,
}

/// Find a direct child box with the given four-character name.
pub fn find_box(
    file: &mut File,
    start: u64,
    limit: u64,
    name: &[u8; 4],
    depth: u32,
) -> Option<BoxSpan> {
    if depth > MAX_BOX_DEPTH {
        return None;
    }
    let mut cursor = start;
    while cursor + 8 <= limit {
        let (size, found, body) = read_box_header(file, cursor)?;
        let end = cursor.checked_add(size)?;
        if end > limit {
            return None;
        }
        if &found == name {
            return Some(BoxSpan { body, end });
        }
        cursor = end;
    }
    None
}

/// Returns the box size, its name, and the offset of its first content byte.
pub fn read_box_header(file: &mut File, at: u64) -> Option<(u64, [u8; 4], u64)> {
    file.seek(SeekFrom::Start(at)).ok()?;
    let mut header = [0u8; 8];
    file.read_exact(&mut header).ok()?;

    let mut size = u64::from(u32::from_be_bytes(header[..4].try_into().ok()?));
    let mut body = at + 8;
    let name: [u8; 4] = header[4..8].try_into().ok()?;

    if size == 1 {
        // A 64-bit size follows the header.
        let mut extended = [0u8; 8];
        file.read_exact(&mut extended).ok()?;
        size = u64::from_be_bytes(extended);
        body += 8;
    }
    if size < body - at {
        return None;
    }
    Some((size, name, body))
}

/// Work out where a `meta` box's children really begin.
pub fn meta_body_start(file: &mut File, body: u64, end: u64) -> Option<u64> {
    for candidate in [body, body + 4] {
        if candidate + 8 > end {
            continue;
        }
        if let Some((size, name, _)) = read_box_header(file, candidate)
            && size >= 8
            && candidate + size <= end
            && name.iter().all(|byte| byte.is_ascii_graphic())
        {
            return Some(candidate);
        }
    }
    None
}

pub fn read_span(file: &mut File, start: u64, end: u64) -> Option<Vec<u8>> {
    let length = end.checked_sub(start)?;
    if length == 0 || length > MAX_METADATA_BOX {
        return None;
    }
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buffer = vec![0u8; length as usize];
    file.read_exact(&mut buffer).ok()?;
    Some(buffer)
}
