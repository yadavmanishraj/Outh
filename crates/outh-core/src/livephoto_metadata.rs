//! Live Photo metadata readers — near line-by-line port of gotohp
//! `core/livephoto_metadata.go` (565 lines, HEAD 97a5dc0).
//!
//! Two formats, both parsed by hand with `std` only (mirroring Go, which
//! deliberately hand-parses instead of using an EXIF/MP4 library):
//!
//! * **Stills (HEIC/HEIF/JPEG):** the Apple MakerNote is located by scanning
//!   the raw file bytes for the `Apple iOS\0` header (Go does not parse the
//!   JPEG/TIFF segment structure either); MakerNote tag 17 is the asset
//!   content identifier — a canonical UUID shared with the paired MOV.
//! * **Videos (MOV):** an MP4/QuickTime box walk collecting the
//!   `com.apple.quicktime.content.identifier` value from the `meta` box
//!   (`keys`/`ilst`) and detecting a still-image-time track via the `stbl`
//!   sample table (`stsd`/`stsz`/`stco`/`co64` + the timed-metadata sample).
//!
//! Error model, mapped to Rust (read this before consuming):
//! Go returns the sentinel `ErrContentIdentifierMissing` for "no identifier".
//! Here, "missing" for a still is `Ok(None)` from
//! [`content_identifier_still`]. For a video, Go returns the partially filled
//! metadata *together with* the missing error, and `livephoto.go` still uses
//! `has_still_image_time` in that case (ignore-metadata mode) — so
//! [`content_identifier_mov`] returns `Ok(Some(MovInfo))` for every
//! successfully walked file and the missing case is
//! `MovInfo::content_identifier == None`. Everywhere else, Go's `error`
//! returns become `Err` here: malformed structure is an error, never a
//! panic, and never silently "missing" — except the specific tolerances
//! noted inline (each mirrors a Go behaviour).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::{Error, Result};

// Constants from livephoto_metadata.go.
const APPLE_MAKER_NOTE_HEADER: &[u8] = b"Apple iOS\0";
const MAX_MAKER_NOTE_SCAN_SIZE: u64 = 64 << 20;
const MAX_CONTENT_IDENTIFIER: usize = 256;
const MAX_MP4_BOX_DEPTH: u32 = 32;

/// Video metadata — port of Go's `LivePhotoMetadata`. `content_identifier`
/// is `None` where Go would pair the value with
/// `ErrContentIdentifierMissing`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MovInfo {
    pub content_identifier: Option<String>,
    pub has_still_image_time: bool,
}

// ---------------------------------------------------------------------------
// Still photos: Apple MakerNote tag 17
// ---------------------------------------------------------------------------

/// Port of Go's `ReadPhotoContentIdentifier`.
///
/// Returns `Ok(Some(uuid))` when a valid content identifier is found,
/// `Ok(None)` when the file carries none (Go's
/// `ErrContentIdentifierMissing`), and `Err` for I/O failures and for a
/// MakerNote that is present but malformed.
pub fn content_identifier_still(path: &Path) -> Result<Option<String>> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    read_photo_content_identifier(&mut file, size)
}

fn read_photo_content_identifier(file: &mut File, size: u64) -> Result<Option<String>> {
    if size == 0 {
        // Go: size <= 0 → ErrContentIdentifierMissing.
        return Ok(None);
    }
    let scan_size = size.min(MAX_MAKER_NOTE_SCAN_SIZE) as usize;
    let mut data = vec![0u8; scan_size];
    // Go reads with ReadAt and tolerates io.EOF (a short read leaves the
    // tail zeroed); mirror that by keeping whatever prefix could be read.
    file.seek(SeekFrom::Start(0))?;
    let mut filled = 0usize;
    while filled < scan_size {
        match file.read(&mut data[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::Io(e)),
        }
    }
    data.truncate(filled);

    // Go scans for every occurrence of the header: a failed parse is
    // remembered and the scan resumes after that header; the LAST failure
    // (error or missing) is the result if no occurrence parses.
    let mut search_from = 0usize;
    let mut last_error: Option<Error> = None;
    loop {
        let Some(relative) = find_subslice(&data[search_from..], APPLE_MAKER_NOTE_HEADER) else {
            break;
        };
        let marker_offset = search_from + relative;
        match parse_apple_maker_note_identifier(&data[marker_offset..]) {
            Ok(Some(identifier)) => return Ok(Some(identifier)),
            // Go overwrites parseErr with ErrContentIdentifierMissing here,
            // discarding any earlier real error; mirror by clearing.
            Ok(None) => last_error = None,
            Err(e) => last_error = Some(e),
        }
        search_from = marker_offset + APPLE_MAKER_NOTE_HEADER.len();
        if search_from >= data.len() {
            break;
        }
    }
    match last_error {
        Some(e) => Err(e),
        None => Ok(None),
    }
}

/// Parses one MakerNote starting at the `Apple iOS\0` header.
/// `Ok(None)` mirrors Go returning `ErrContentIdentifierMissing` when the
/// note has no tag 17.
fn parse_apple_maker_note_identifier(data: &[u8]) -> Result<Option<String>> {
    if data.len() < 16 || &data[..APPLE_MAKER_NOTE_HEADER.len()] != APPLE_MAKER_NOTE_HEADER {
        return Err(Error::Protocol(
            "invalid Apple Maker Note header".to_string(),
        ));
    }

    // Bytes 12..14 select the byte order of every following field.
    let big_endian = match &data[12..14] {
        b"MM" => true,
        b"II" => false,
        _ => {
            return Err(Error::Protocol(
                "invalid Apple Maker Note byte order".to_string(),
            ))
        }
    };
    let u16_at = |slice: &[u8]| -> u16 {
        if big_endian {
            u16::from_be_bytes([slice[0], slice[1]])
        } else {
            u16::from_le_bytes([slice[0], slice[1]])
        }
    };
    let u32_at = |slice: &[u8]| -> u32 {
        if big_endian {
            u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]])
        } else {
            u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]])
        }
    };

    let entry_count = u16_at(&data[14..16]) as usize;
    let entries_end = 16 + entry_count * 12;
    if entry_count == 0 || entries_end > data.len() {
        return Err(Error::Protocol(
            "invalid Apple Maker Note entry table".to_string(),
        ));
    }

    // Apple Maker Note tag 17 is the asset content identifier shared with
    // the paired QuickTime file; it is the pairing key rather than the
    // filenames.
    for index in 0..entry_count {
        let entry = &data[16 + index * 12..16 + (index + 1) * 12];
        if u16_at(&entry[0..2]) != 17 {
            continue;
        }
        if u16_at(&entry[2..4]) != 2 {
            return Err(Error::Protocol(
                "Apple content identifier has unexpected type".to_string(),
            ));
        }
        let value_length = u32_at(&entry[4..8]) as usize;
        if value_length == 0 || value_length > MAX_CONTENT_IDENTIFIER {
            return Err(Error::Protocol(
                "Apple content identifier has invalid length".to_string(),
            ));
        }
        // Values of up to 4 bytes are inline in the entry; longer values
        // live at an offset relative to the start of the MakerNote (i.e.
        // of `data` here — Go slices from the header before parsing).
        let value: &[u8] = if value_length <= 4 {
            &entry[8..8 + value_length]
        } else {
            let value_offset = u32_at(&entry[8..12]) as usize;
            if value_offset > data.len() || value_length > data.len() - value_offset {
                return Err(Error::Protocol(
                    "Apple content identifier points outside Maker Note".to_string(),
                ));
            }
            &data[value_offset..value_offset + value_length]
        };

        let identifier = trim_identifier_bytes(value);
        if !is_canonical_uuid(identifier) {
            return Err(Error::Protocol(
                "Apple content identifier is not a canonical UUID".to_string(),
            ));
        }
        // The canonical-UUID check guarantees plain ASCII, so this never
        // fails; map it anyway instead of unwrapping.
        let identifier = String::from_utf8(identifier.to_vec()).map_err(|_| {
            Error::Protocol("Apple content identifier is not a canonical UUID".to_string())
        })?;
        return Ok(Some(identifier));
    }
    Ok(None)
}

/// Go: `strings.Trim(value, "\x00 \t\r\n")` — trims the cut set at both ends.
fn trim_identifier_bytes(value: &[u8]) -> &[u8] {
    let is_cut = |b: u8| matches!(b, 0 | b' ' | b'\t' | b'\r' | b'\n');
    let mut start = 0;
    let mut end = value.len();
    while start < end && is_cut(value[start]) {
        start += 1;
    }
    while end > start && is_cut(value[end - 1]) {
        end -= 1;
    }
    &value[start..end]
}

fn is_canonical_uuid(value: &[u8]) -> bool {
    if value.len() != 36 {
        return false;
    }
    for (index, &byte) in value.iter().enumerate() {
        match index {
            8 | 13 | 18 | 23 => {
                if byte != b'-' {
                    return false;
                }
            }
            _ => {
                if !byte.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// ---------------------------------------------------------------------------
// Videos: QuickTime/MP4 box walk
// ---------------------------------------------------------------------------

/// Port of Go's `ReadVideoLivePhotoMetadata`. See the module docs for why
/// the result is `Some` even when the identifier is missing.
pub fn content_identifier_mov(path: &Path) -> Result<Option<MovInfo>> {
    Ok(Some(read_video_metadata(path)?))
}

/// The classifier-facing form: Go's `(LivePhotoMetadata, error)` pair, with
/// the missing-identifier case carried in `MovInfo::content_identifier`.
pub(crate) fn read_video_metadata(path: &Path) -> Result<MovInfo> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut reader = ReaderAt { file };
    read_video_live_photo_metadata(&mut reader, size)
}

/// Minimal random-access reader over a `File` — Go's `io.ReaderAt`.
/// All offsets reaching this type have already been bounds-checked against
/// the file size by the box logic, exactly as in Go.
struct ReaderAt {
    file: File,
}

impl ReaderAt {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(buf)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct Mp4Box {
    typ: [u8; 4],
    payload_start: u64,
    end: u64,
}

fn read_video_live_photo_metadata(reader: &mut ReaderAt, size: u64) -> Result<MovInfo> {
    if size < 8 {
        return Err(Error::Protocol("invalid QuickTime file size".to_string()));
    }
    let mut metadata = MovInfo::default();
    walk_mp4_boxes(reader, 0, size, 0, &mut metadata)?;
    Ok(metadata)
}

fn walk_mp4_boxes(
    reader: &mut ReaderAt,
    start: u64,
    end: u64,
    depth: u32,
    metadata: &mut MovInfo,
) -> Result<()> {
    if depth > MAX_MP4_BOX_DEPTH {
        return Err(Error::Protocol(format!(
            "QuickTime box nesting exceeds {MAX_MP4_BOX_DEPTH} levels"
        )));
    }
    let mut offset = start;
    while offset < end {
        let mp4_box = read_mp4_box(reader, offset, end)?;
        // Visitor (Go's walkMP4Boxes callback): runs for every box at
        // every depth, before descending into containers.
        match &mp4_box.typ {
            b"meta" => {
                if let Some(identifier) = read_quicktime_content_identifier(reader, &mp4_box)? {
                    metadata.content_identifier = Some(identifier);
                }
            }
            b"stbl" => {
                let has = read_still_image_time_track(reader, &mp4_box)?;
                metadata.has_still_image_time = metadata.has_still_image_time || has;
            }
            _ => {}
        }
        if is_mp4_container(&mp4_box.typ) {
            let mut child_start = mp4_box.payload_start;
            if &mp4_box.typ == b"meta" {
                child_start = quicktime_meta_children_start(reader, &mp4_box)?;
            }
            if child_start > mp4_box.end {
                return Err(Error::Protocol(format!(
                    "invalid {} container",
                    String::from_utf8_lossy(&mp4_box.typ)
                )));
            }
            walk_mp4_boxes(reader, child_start, mp4_box.end, depth + 1, metadata)?;
        }
        offset = mp4_box.end;
    }
    Ok(())
}

fn read_mp4_box(reader: &mut ReaderAt, offset: u64, parent_end: u64) -> Result<Mp4Box> {
    let mut header = [0u8; 16];
    if parent_end < offset || parent_end - offset < 8 {
        return Err(Error::Protocol(format!(
            "truncated QuickTime box header at {offset}"
        )));
    }
    reader.read_at(offset, &mut header[..8])?;

    let size32 = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    let (box_size, header_size): (u64, u64) = match size32 {
        1 => {
            if parent_end - offset < 16 {
                return Err(Error::Protocol(format!(
                    "truncated extended QuickTime box header at {offset}"
                )));
            }
            reader.read_at(offset + 8, &mut header[8..16])?;
            (
                u64::from_be_bytes([
                    header[8], header[9], header[10], header[11], header[12], header[13],
                    header[14], header[15],
                ]),
                16,
            )
        }
        0 => (parent_end - offset, 8),
        s => (s as u64, 8),
    };
    if box_size < header_size || box_size > parent_end - offset {
        return Err(Error::Protocol(format!(
            "invalid QuickTime box size at {offset}"
        )));
    }

    let mut typ = [0u8; 4];
    typ.copy_from_slice(&header[4..8]);
    Ok(Mp4Box {
        typ,
        payload_start: offset + header_size,
        end: offset + box_size,
    })
}

fn is_mp4_container(typ: &[u8; 4]) -> bool {
    matches!(
        typ,
        b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"udta" | b"meta"
    )
}

// QuickTime permits a headerless meta atom, while ISO BMFF meta boxes
// begin with four version/flags bytes. Detect the form from the first
// child header. (Comment carried from Go.)
fn quicktime_meta_children_start(reader: &mut ReaderAt, mp4_box: &Mp4Box) -> Result<u64> {
    let payload_size = mp4_box.end - mp4_box.payload_start;
    if payload_size < 8 {
        return Err(Error::Protocol(
            "truncated QuickTime meta box".to_string(),
        ));
    }
    let mut header = [0u8; 8];
    reader.read_at(mp4_box.payload_start, &mut header)?;
    let first_child_size = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
    if first_child_size >= 8 && first_child_size <= payload_size {
        return Ok(mp4_box.payload_start);
    }
    if payload_size < 12 {
        return Err(Error::Protocol("truncated ISO meta box".to_string()));
    }
    Ok(mp4_box.payload_start + 4)
}

fn read_direct_mp4_boxes(reader: &mut ReaderAt, start: u64, end: u64) -> Result<Vec<Mp4Box>> {
    let mut boxes = Vec::new();
    let mut offset = start;
    while offset < end {
        let mp4_box = read_mp4_box(reader, offset, end)?;
        boxes.push(mp4_box);
        offset = mp4_box.end;
    }
    Ok(boxes)
}

fn read_box_payload(reader: &mut ReaderAt, mp4_box: &Mp4Box, max_size: u64) -> Result<Vec<u8>> {
    let size = mp4_box.end - mp4_box.payload_start;
    if size > max_size {
        return Err(Error::Protocol(format!(
            "{} box payload is too large",
            String::from_utf8_lossy(&mp4_box.typ)
        )));
    }
    let mut payload = vec![0u8; size as usize];
    if !payload.is_empty() {
        reader.read_at(mp4_box.payload_start, &mut payload)?;
    }
    Ok(payload)
}

fn read_quicktime_content_identifier(
    reader: &mut ReaderAt,
    meta: &Mp4Box,
) -> Result<Option<String>> {
    let children_start = quicktime_meta_children_start(reader, meta)?;
    let children = read_direct_mp4_boxes(reader, children_start, meta.end)?;
    // Go keeps the LAST keys / ilst child seen; the scan below does the
    // same by overwriting the selection on every match.
    let mut keys_box: Option<&Mp4Box> = None;
    let mut item_list_box: Option<&Mp4Box> = None;
    for child in &children {
        match &child.typ {
            b"keys" => keys_box = Some(child),
            b"ilst" => item_list_box = Some(child),
            _ => {}
        }
    }
    let (Some(keys_box), Some(item_list_box)) = (keys_box, item_list_box) else {
        return Ok(None);
    };

    let keys = read_quicktime_keys(reader, keys_box)?;
    let mut content_index: u32 = 0;
    for (index, key) in keys.iter().enumerate() {
        if key == "com.apple.quicktime.content.identifier" {
            content_index = (index + 1) as u32;
            break;
        }
    }
    if content_index == 0 {
        return Ok(None);
    }

    let Some(identifier) = read_quicktime_item_value(reader, item_list_box, content_index)?
    else {
        return Ok(None);
    };
    let identifier = identifier.trim_matches(|c| matches!(c, '\0' | ' ' | '\t' | '\r' | '\n'));
    if !is_canonical_uuid(identifier.as_bytes()) {
        return Err(Error::Protocol(
            "QuickTime content identifier is not a canonical UUID".to_string(),
        ));
    }
    Ok(Some(identifier.to_string()))
}

fn read_quicktime_keys(reader: &mut ReaderAt, mp4_box: &Mp4Box) -> Result<Vec<String>> {
    let payload = read_box_payload(reader, mp4_box, 4 << 20)?;
    if payload.len() < 8 {
        return Err(Error::Protocol(
            "truncated QuickTime keys box".to_string(),
        ));
    }
    let entry_count = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]) as usize;
    if entry_count > (payload.len() - 8) / 8 {
        return Err(Error::Protocol(
            "invalid QuickTime metadata key count".to_string(),
        ));
    }
    let mut offset = 8usize;
    let mut keys = Vec::with_capacity(entry_count);
    for _ in 0..entry_count {
        if payload.len() - offset < 8 {
            return Err(Error::Protocol(
                "truncated QuickTime metadata key".to_string(),
            ));
        }
        let entry_size =
            u32::from_be_bytes([payload[offset], payload[offset + 1], payload[offset + 2], payload[offset + 3]])
                as usize;
        if entry_size < 8 || entry_size > payload.len() - offset {
            return Err(Error::Protocol(
                "invalid QuickTime metadata key size".to_string(),
            ));
        }
        if payload[offset + 4..offset + 8] == *b"mdta" {
            keys.push(
                String::from_utf8_lossy(&payload[offset + 8..offset + entry_size]).into_owned(),
            );
        } else {
            // Go stores "" for non-mdta entries; the index still counts.
            keys.push(String::new());
        }
        offset += entry_size;
    }
    Ok(keys)
}

fn read_quicktime_item_value(
    reader: &mut ReaderAt,
    item_list: &Mp4Box,
    wanted_index: u32,
) -> Result<Option<String>> {
    let items = read_direct_mp4_boxes(reader, item_list.payload_start, item_list.end)?;
    for item in &items {
        // Item boxes are named by their 1-based key index as a big-endian
        // u32 type — not an ASCII fourcc.
        if u32::from_be_bytes(item.typ) != wanted_index {
            continue;
        }
        let values = read_direct_mp4_boxes(reader, item.payload_start, item.end)?;
        for value in &values {
            if &value.typ != b"data" || value.end - value.payload_start < 8 {
                continue;
            }
            let payload = read_box_payload(reader, value, (MAX_CONTENT_IDENTIFIER + 8) as u64)?;
            // payload[..8] is the data type indicator + locale; the value
            // follows. Go returns it undecoded; non-UTF-8 bytes fail the
            // caller's UUID check, matching Go's string handling.
            return Ok(Some(
                String::from_utf8_lossy(&payload[8..]).into_owned(),
            ));
        }
    }
    Ok(None)
}

fn read_still_image_time_track(reader: &mut ReaderAt, sample_table: &Mp4Box) -> Result<bool> {
    let children =
        read_direct_mp4_boxes(reader, sample_table.payload_start, sample_table.end)?;
    let mut sample_description: Option<&Mp4Box> = None;
    let mut sample_sizes: Option<&Mp4Box> = None;
    let mut chunk_offsets: Option<&Mp4Box> = None;
    for child in &children {
        match &child.typ {
            b"stsd" => sample_description = Some(child),
            b"stsz" => sample_sizes = Some(child),
            b"stco" | b"co64" => chunk_offsets = Some(child),
            _ => {}
        }
    }
    let (Some(sample_description), Some(sample_sizes), Some(chunk_offsets)) =
        (sample_description, sample_sizes, chunk_offsets)
    else {
        return Ok(false);
    };

    if !sample_description_contains_still_image_time(reader, sample_description)? {
        return Ok(false);
    }
    let sample_size = read_first_sample_size(reader, sample_sizes)?;
    let sample_offset = read_first_chunk_offset(reader, chunk_offsets)?;
    if sample_size < 9 || sample_size > (1 << 20) {
        return Ok(false);
    }
    let mut sample = vec![0u8; sample_size as usize];
    reader.read_at(sample_offset, &mut sample)?;
    sample_contains_still_image_time(&sample)
}

fn sample_description_contains_still_image_time(
    reader: &mut ReaderAt,
    mp4_box: &Mp4Box,
) -> Result<bool> {
    let payload = read_box_payload(reader, mp4_box, 4 << 20)?;
    if payload.len() < 8 {
        return Err(Error::Protocol(
            "truncated sample description".to_string(),
        ));
    }
    let entry_count = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let needle: &[u8] = b"com.apple.quicktime.still-image-time";
    let mut offset = 8usize;
    for _ in 0..entry_count {
        if payload.len() - offset < 8 {
            return Err(Error::Protocol(
                "truncated sample description entry".to_string(),
            ));
        }
        let entry_size = u32::from_be_bytes([
            payload[offset],
            payload[offset + 1],
            payload[offset + 2],
            payload[offset + 3],
        ]) as usize;
        if entry_size < 8 || entry_size > payload.len() - offset {
            return Err(Error::Protocol(
                "invalid sample description entry size".to_string(),
            ));
        }
        if payload[offset + 4..offset + 8] == *b"mebx"
            && find_subslice(&payload[offset + 8..offset + entry_size], needle).is_some()
        {
            return Ok(true);
        }
        offset += entry_size;
    }
    Ok(false)
}

fn read_first_sample_size(reader: &mut ReaderAt, mp4_box: &Mp4Box) -> Result<u32> {
    let payload = read_box_payload(reader, mp4_box, 1 << 20)?;
    if payload.len() < 12 {
        return Err(Error::Protocol(
            "truncated sample size box".to_string(),
        ));
    }
    let fixed_size = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let sample_count = u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]);
    if sample_count == 0 {
        return Err(Error::Protocol(
            "still-image-time track has no samples".to_string(),
        ));
    }
    if fixed_size != 0 {
        return Ok(fixed_size);
    }
    if payload.len() < 16 {
        return Err(Error::Protocol(
            "truncated variable sample size box".to_string(),
        ));
    }
    Ok(u32::from_be_bytes([
        payload[12],
        payload[13],
        payload[14],
        payload[15],
    ]))
}

fn read_first_chunk_offset(reader: &mut ReaderAt, mp4_box: &Mp4Box) -> Result<u64> {
    let payload = read_box_payload(reader, mp4_box, 1 << 20)?;
    if payload.len() < 12
        || u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]) == 0
    {
        return Err(Error::Protocol(
            "still-image-time track has no chunks".to_string(),
        ));
    }
    if &mp4_box.typ == b"co64" {
        if payload.len() < 16 {
            return Err(Error::Protocol(
                "truncated 64-bit chunk offset box".to_string(),
            ));
        }
        let offset = u64::from_be_bytes([
            payload[8], payload[9], payload[10], payload[11], payload[12], payload[13],
            payload[14], payload[15],
        ]);
        if offset > i64::MAX as u64 {
            return Err(Error::Protocol("chunk offset is too large".to_string()));
        }
        return Ok(offset);
    }
    Ok(u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]) as u64)
}

fn sample_contains_still_image_time(sample: &[u8]) -> Result<bool> {
    // A timed metadata sample may contain several length-prefixed values,
    // so walk every record instead of assuming the still-image-time value
    // fills the sample. (Comment carried from Go.)
    let mut offset = 0usize;
    while offset < sample.len() {
        if sample.len() - offset < 8 {
            return Err(Error::Protocol(
                "truncated timed metadata value".to_string(),
            ));
        }
        let value_size = u32::from_be_bytes([
            sample[offset],
            sample[offset + 1],
            sample[offset + 2],
            sample[offset + 3],
        ]) as usize;
        if value_size < 9 || value_size > sample.len() - offset {
            return Err(Error::Protocol(
                "invalid timed metadata value size".to_string(),
            ));
        }
        let key_index = u32::from_be_bytes([
            sample[offset + 4],
            sample[offset + 5],
            sample[offset + 6],
            sample[offset + 7],
        ]);
        if key_index == 1 && sample[offset + 8] == 0xff {
            return Ok(true);
        }
        offset += value_size;
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    const UUID: &str = "1A2B3C4D-5E6F-4A5B-8C9D-0E1F2A3B4C5D";

    fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "outh-livephoto-metadata-test-{}-{}",
            std::process::id(),
            name
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join(name);
        fs::write(&path, bytes).expect("write temp file");
        path
    }

    // --- still fixtures ---------------------------------------------------

    /// Builds a minimal MakerNote: header + order mark + one entry.
    /// The file around it is arbitrary bytes — Go scans for the header
    /// anywhere in the file, and so does the port.
    fn maker_note(big_endian: bool, tag: u16, typ: u16, value: &[u8]) -> Vec<u8> {
        let mut note = Vec::new();
        note.extend_from_slice(b"Apple iOS\0");
        note.extend_from_slice(&[0, 0]); // bytes 10..12 are unused by the parser
        let (u16v, u32v): (
            Box<dyn Fn(u16) -> [u8; 2]>,
            Box<dyn Fn(u32) -> [u8; 4]>,
        ) = if big_endian {
            (Box::new(u16::to_be_bytes), Box::new(u32::to_be_bytes))
        } else {
            (Box::new(u16::to_le_bytes), Box::new(u32::to_le_bytes))
        };
        note.extend_from_slice(if big_endian { b"MM" } else { b"II" });
        note.extend_from_slice(&u16v(1)); // entry count
        let entries_end = 16 + 12;
        note.extend_from_slice(&u16v(tag));
        note.extend_from_slice(&u16v(typ));
        note.extend_from_slice(&u32v(value.len() as u32));
        if value.len() <= 4 {
            let mut inline = [0u8; 4];
            inline[..value.len()].copy_from_slice(value);
            note.extend_from_slice(&inline);
        } else {
            note.extend_from_slice(&u32v(entries_end as u32));
            assert_eq!(note.len(), entries_end);
            note.extend_from_slice(value);
        }
        note
    }

    fn still_file(big_endian: bool, tag: u16, typ: u16, value: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x10]; // SOI + APP1-ish prefix
        bytes.extend_from_slice(&maker_note(big_endian, tag, typ, value));
        bytes.extend_from_slice(&[0xFF, 0xD9]);
        bytes
    }

    #[test]
    fn still_identifier_big_endian() {
        let mut value = UUID.as_bytes().to_vec();
        value.push(0); // NUL-terminated, as written by Apple
        let path = temp_file("still-be.jpg", &still_file(true, 17, 2, &value));
        assert_eq!(
            content_identifier_still(&path).expect("parse"),
            Some(UUID.to_string())
        );
    }

    #[test]
    fn still_identifier_little_endian() {
        let mut value = UUID.as_bytes().to_vec();
        value.push(0);
        let path = temp_file("still-le.heic", &still_file(false, 17, 2, &value));
        assert_eq!(
            content_identifier_still(&path).expect("parse"),
            Some(UUID.to_string())
        );
    }

    #[test]
    fn still_without_maker_note_is_missing() {
        let path = temp_file("plain.jpg", &[0xFF, 0xD8, 1, 2, 3, 4, 0xFF, 0xD9]);
        assert_eq!(content_identifier_still(&path).expect("parse"), None);
    }

    #[test]
    fn still_empty_file_is_missing() {
        let path = temp_file("empty.jpg", &[]);
        assert_eq!(content_identifier_still(&path).expect("parse"), None);
    }

    #[test]
    fn still_other_tag_is_missing() {
        let path = temp_file("other-tag.jpg", &still_file(true, 23, 2, b"1234567890"));
        assert_eq!(content_identifier_still(&path).expect("parse"), None);
    }

    #[test]
    fn still_wrong_type_is_error() {
        let mut value = UUID.as_bytes().to_vec();
        value.push(0);
        let path = temp_file("wrong-type.jpg", &still_file(true, 17, 3, &value));
        assert!(content_identifier_still(&path).is_err());
    }

    #[test]
    fn still_non_uuid_is_error() {
        let path = temp_file(
            "non-uuid.jpg",
            &still_file(true, 17, 2, b"not-a-uuid-at-all-padding-0123456789"),
        );
        assert!(content_identifier_still(&path).is_err());
    }

    #[test]
    fn still_second_maker_note_wins_after_bad_first() {
        // Go keeps scanning after a failed MakerNote parse.
        let mut bytes = still_file(true, 17, 2, b"garbage-garbage-garbage-garbage-0000");
        let mut good = UUID.as_bytes().to_vec();
        good.push(0);
        bytes.extend_from_slice(&maker_note(true, 17, 2, &good));
        let path = temp_file("two-notes.jpg", &bytes);
        assert_eq!(
            content_identifier_still(&path).expect("parse"),
            Some(UUID.to_string())
        );
    }

    // --- MOV fixtures ------------------------------------------------------

    fn bx(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + payload.len());
        out.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
        out.extend_from_slice(typ);
        out.extend_from_slice(payload);
        out
    }

    fn keys_box() -> Vec<u8> {
        let key = b"com.apple.quicktime.content.identifier";
        let mut entry = Vec::new();
        entry.extend_from_slice(&((8 + key.len()) as u32).to_be_bytes());
        entry.extend_from_slice(b"mdta");
        entry.extend_from_slice(key);
        let mut payload = vec![0, 0, 0, 0]; // version/flags
        payload.extend_from_slice(&1u32.to_be_bytes());
        payload.extend_from_slice(&entry);
        bx(b"keys", &payload)
    }

    fn ilst_box(identifier: &str) -> Vec<u8> {
        let mut data_payload = vec![0u8; 8]; // data type + locale
        data_payload.extend_from_slice(identifier.as_bytes());
        let data_box = bx(b"data", &data_payload);
        let item = bx(&1u32.to_be_bytes(), &data_box); // item type = key index 1
        bx(b"ilst", &item)
    }

    fn meta_box(iso_style: bool, identifier: Option<&str>) -> Vec<u8> {
        let mut payload = Vec::new();
        if iso_style {
            payload.extend_from_slice(&[0, 0, 0, 0]); // version/flags
        }
        payload.extend_from_slice(&keys_box());
        if let Some(id) = identifier {
            payload.extend_from_slice(&ilst_box(id));
        }
        bx(b"meta", &payload)
    }

    /// Builds an stbl with a still-image-time stsd, an stsz, and an stco
    /// whose value is supplied by the caller (two-pass assembly: the
    /// sample lives after the moov box, so its offset depends on the
    /// final file length).
    fn stbl_box(chunk_offset: u32, sample_size: u32, still_time_entry: bool) -> Vec<u8> {
        // stsd: version/flags + count + one mebx entry carrying the key.
        let mut entry = Vec::new();
        let content: &[u8] = if still_time_entry {
            b"com.apple.quicktime.still-image-time"
        } else {
            b"com.apple.quicktime.something-else"
        };
        entry.extend_from_slice(&((8 + content.len()) as u32).to_be_bytes());
        entry.extend_from_slice(b"mebx");
        entry.extend_from_slice(content);
        let mut stsd_payload = vec![0, 0, 0, 0];
        stsd_payload.extend_from_slice(&1u32.to_be_bytes());
        stsd_payload.extend_from_slice(&entry);
        let stsd = bx(b"stsd", &stsd_payload);

        // stsz: version/flags + fixed size 0 + count 1 + first size.
        let mut stsz_payload = vec![0, 0, 0, 0];
        stsz_payload.extend_from_slice(&0u32.to_be_bytes());
        stsz_payload.extend_from_slice(&1u32.to_be_bytes());
        stsz_payload.extend_from_slice(&sample_size.to_be_bytes());
        let stsz = bx(b"stsz", &stsz_payload);

        // stco: version/flags + count 1 + the sample's file offset.
        let mut stco_payload = vec![0, 0, 0, 0];
        stco_payload.extend_from_slice(&1u32.to_be_bytes());
        stco_payload.extend_from_slice(&chunk_offset.to_be_bytes());
        let stco = bx(b"stco", &stco_payload);

        let mut stbl_payload = Vec::new();
        stbl_payload.extend_from_slice(&stsd);
        stbl_payload.extend_from_slice(&stsz);
        stbl_payload.extend_from_slice(&stco);
        bx(b"stbl", &stbl_payload)
    }

    fn mov_file(identifier: Option<&str>, with_still_track: bool) -> Vec<u8> {
        // Timed-metadata sample: one record [size=9][key index 1][0xff].
        let mut sample = Vec::new();
        sample.extend_from_slice(&9u32.to_be_bytes());
        sample.extend_from_slice(&1u32.to_be_bytes());
        sample.push(0xff);

        // Pass 1: measure the moov length with a placeholder offset.
        let build = |chunk_offset: u32| -> Vec<u8> {
            let mut moov_payload = Vec::new();
            moov_payload.extend_from_slice(&meta_box(false, identifier));
            if with_still_track {
                let minf = bx(b"minf", &stbl_box(chunk_offset, sample.len() as u32, true));
                let mdia = bx(b"mdia", &minf);
                let trak = bx(b"trak", &mdia);
                moov_payload.extend_from_slice(&trak);
            }
            bx(b"moov", &moov_payload)
        };
        let moov = build(0);
        let sample_offset = moov.len() as u32;
        let mut file = build(sample_offset);
        file.extend_from_slice(&sample);
        file
    }

    #[test]
    fn mov_identifier_and_still_image_time() {
        let path = temp_file("live.mov", &mov_file(Some(UUID), true));
        let info = read_video_metadata(&path).expect("walk");
        assert_eq!(info.content_identifier.as_deref(), Some(UUID));
        assert!(info.has_still_image_time);
    }

    #[test]
    fn mov_identifier_without_still_track() {
        let path = temp_file("plain.mov", &mov_file(Some(UUID), false));
        let info = read_video_metadata(&path).expect("walk");
        assert_eq!(info.content_identifier.as_deref(), Some(UUID));
        assert!(!info.has_still_image_time);
    }

    #[test]
    fn mov_without_meta_has_no_identifier() {
        let mut moov_payload = Vec::new();
        moov_payload.extend_from_slice(&bx(b"free", &[1, 2, 3, 4]));
        let file = bx(b"moov", &moov_payload);
        let path = temp_file("nometa.mov", &file);
        let info = read_video_metadata(&path).expect("walk");
        assert_eq!(info.content_identifier, None);
        assert!(!info.has_still_image_time);
    }

    #[test]
    fn mov_iso_style_meta() {
        let mut moov_payload = Vec::new();
        moov_payload.extend_from_slice(&meta_box(true, Some(UUID)));
        let file = bx(b"moov", &moov_payload);
        let path = temp_file("iso.mov", &file);
        let info = read_video_metadata(&path).expect("walk");
        assert_eq!(info.content_identifier.as_deref(), Some(UUID));
    }

    #[test]
    fn mov_too_small_is_error() {
        let path = temp_file("tiny.mov", &[0, 0, 0]);
        assert!(read_video_metadata(&path).is_err());
    }

    #[test]
    fn mov_bad_box_size_is_error() {
        // Declared size larger than the file.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&9999u32.to_be_bytes());
        bytes.extend_from_slice(b"moov");
        let path = temp_file("badsize.mov", &bytes);
        assert!(read_video_metadata(&path).is_err());
    }

    #[test]
    fn sample_record_walk() {
        // Second record carries the still-image-time marker.
        let mut sample = Vec::new();
        sample.extend_from_slice(&10u32.to_be_bytes());
        sample.extend_from_slice(&2u32.to_be_bytes());
        sample.extend_from_slice(&[0xAA, 0xBB]);
        sample.extend_from_slice(&9u32.to_be_bytes());
        sample.extend_from_slice(&1u32.to_be_bytes());
        sample.push(0xff);
        assert!(sample_contains_still_image_time(&sample).expect("walk"));
    }

    #[test]
    fn uuid_validation() {
        assert!(is_canonical_uuid(UUID.as_bytes()));
        assert!(!is_canonical_uuid(b"1A2B3C4D5E6F4A5B8C9D0E1F2A3B4C5D00")); // no hyphens
        assert!(!is_canonical_uuid(b"1A2B3C4D-5E6F-4A5B-8C9D-0E1F2A3B4C5")); // short
        assert!(!is_canonical_uuid(b"ZA2B3C4D-5E6F-4A5B-8C9D-0E1F2A3B4C5D")); // bad hex
    }
}
