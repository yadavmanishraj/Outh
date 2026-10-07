//! Protobuf messages for the private Google Photos protocol, ported from
//! gotohp's `.proto/*.proto` + `generated/*.pb.go` as actually constructed and
//! parsed by `core/api.go` and `core/create_media_items.go`.
//!
//! Field numbers are the contract: the `.proto` files were reverse-engineered
//! from captured traffic (`.proto/readme.md`), so field *names* are mostly
//! positional (`field1`, `f2`, ...) and only the numbers — plus a few
//! human-named fields like `media_key` — carry meaning.
//!
//! Encoding follows proto3 / Go `proto.Marshal` semantics: scalar fields are
//! omitted when zero, strings/bytes when empty, message fields when `None`,
//! and fields are written in field-number order. Decoding skips unknown
//! fields so newer server responses keep parsing.
//! Each message provides `encode(&self, &mut Vec<u8>)` (CONTRACT.md §3),
//! `encode_to_vec()` and `decode(&[u8])`.

use crate::wire::{Reader, WIRE_LEN, WIRE_VARINT};
use crate::{wire, Error, Result};

// ---------------------------------------------------------------------------
// Shared device identity (identical shape/field numbers in four messages)
// ---------------------------------------------------------------------------

/// Device identity block: fields 3/4/5 in CommitUpload field 2, CreateAlbum
/// field 8, AddMediaToAlbum field 6, and CreateMediaItemsRequest field 2
/// (`UploadDeviceInfo`). Values are the spoofed Android identity from
/// core/api.go (default model "Pixel XL", make "Google", API 28).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DeviceInfo {
    pub model: String,              // 3
    pub make: String,               // 4
    pub android_api_version: i64,   // 5
}

impl DeviceInfo {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if !self.model.is_empty() {
            wire::put_string(out, 3, &self.model);
        }
        if !self.make.is_empty() {
            wire::put_string(out, 4, &self.make);
        }
        if self.android_api_version != 0 {
            wire::put_i64(out, 5, self.android_api_version);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                3 => msg.model = reader.get_string(wt)?,
                4 => msg.make = reader.get_string(wt)?,
                5 => msg.android_api_version = reader.get_i64(wt)?,
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

// ---------------------------------------------------------------------------
// GetUploadToken.proto — request only; the response is the
// X-GUploader-UploadID header (core/api.go GetUploadToken).
// ---------------------------------------------------------------------------

/// Scotty session-open request. Go (core/api.go) always sends
/// f1=2, f2=2, f3=1, f4=3 plus the file size in field 7.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GetUploadToken {
    pub f1: i32,              // 1
    pub f2: i32,              // 2
    pub f3: i32,              // 3
    pub f4: i32,              // 4
    pub file_size_bytes: i64, // 7
}

impl GetUploadToken {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if self.f1 != 0 {
            wire::put_i32(out, 1, self.f1);
        }
        if self.f2 != 0 {
            wire::put_i32(out, 2, self.f2);
        }
        if self.f3 != 0 {
            wire::put_i32(out, 3, self.f3);
        }
        if self.f4 != 0 {
            wire::put_i32(out, 4, self.f4);
        }
        if self.file_size_bytes != 0 {
            wire::put_i64(out, 7, self.file_size_bytes);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.f1 = reader.get_i32(wt)?,
                2 => msg.f2 = reader.get_i32(wt)?,
                3 => msg.f3 = reader.get_i32(wt)?,
                4 => msg.f4 = reader.get_i32(wt)?,
                7 => msg.file_size_bytes = reader.get_i64(wt)?,
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

// ---------------------------------------------------------------------------
// HashCheck.proto / RemoteMatches.proto — library dedup lookup
// (core/api.go FindRemoteMediaByHash).
// ---------------------------------------------------------------------------

/// HashCheck request: field 1 wraps { field 1: { sha1 }, field 2: {} }.
/// The empty field-2 message is part of the fingerprint and must be present.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HashCheck {
    pub field1: Option<HashCheckField1>, // 1
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct HashCheckField1 {
    pub field1: Option<HashCheckField1Field1>, // 1
    pub field2: Option<HashCheckField1Field2>, // 2 — always Some(empty) as sent by Go
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct HashCheckField1Field1 {
    pub sha1_hash: Vec<u8>, // 1
}

/// Empty message; presence on the wire is its only content.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HashCheckField1Field2 {}

impl HashCheck {
    /// Builds the exact nesting core/api.go constructs for a 20-byte SHA-1.
    pub fn new(sha1_hash: &[u8]) -> Self {
        HashCheck {
            field1: Some(HashCheckField1 {
                field1: Some(HashCheckField1Field1 {
                    sha1_hash: sha1_hash.to_vec(),
                }),
                field2: Some(HashCheckField1Field2 {}),
            }),
        }
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        if let Some(field1) = &self.field1 {
            wire::put_msg(out, 1, &field1.encode_to_vec());
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.field1 = Some(HashCheckField1::decode(reader.get_bytes(wt)?)?),
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl HashCheckField1 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if let Some(field1) = &self.field1 {
            wire::put_msg(out, 1, &field1.encode_to_vec());
        }
        if let Some(field2) = &self.field2 {
            wire::put_msg(out, 2, &field2.encode_to_vec());
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.field1 = Some(HashCheckField1Field1::decode(reader.get_bytes(wt)?)?),
                2 => msg.field2 = Some(HashCheckField1Field2::decode(reader.get_bytes(wt)?)?),
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl HashCheckField1Field1 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if !self.sha1_hash.is_empty() {
            wire::put_bytes(out, 1, &self.sha1_hash);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.sha1_hash = reader.get_bytes(wt)?.to_vec(),
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl HashCheckField1Field2 {
    pub fn encode(&self, _out: &mut Vec<u8>) {}

    pub fn encode_to_vec(&self) -> Vec<u8> {
        Vec::new()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = Reader::new(bytes);
        while let Some((_, wt)) = reader.read_tag()? {
            reader.skip_field(wt)?;
        }
        Ok(Self {})
    }
}

/// HashCheck response. The only path Go reads is the media key at
/// field1.field2.field2.media_key (generated/utils.go `GetMediaKey`);
/// the remaining projected fields are decoded for completeness.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RemoteMatches {
    pub field1: Option<RemoteMatchesField1>, // 1
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RemoteMatchesField1 {
    pub field2: Option<RemoteMatchesField1Field2>, // 2
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RemoteMatchesField1Field2 {
    pub field1: Option<RemoteMatchesField1Field2Field1>, // 1
    pub field2: Option<RemoteMatchesField1Field2Field2>, // 2
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RemoteMatchesField1Field2Field1 {
    pub sha1_hash: Vec<u8>, // 1
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RemoteMatchesField1Field2Field2 {
    pub media_key: String,                              // 1
    pub field6: Option<RemoteMatchesField6>,            // 6
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RemoteMatchesField6 {
    pub media_key: String,                       // 1
    pub field2: Option<RemoteMatchesField6Field2>, // 2
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RemoteMatchesField6Field2 {
    pub field1: String, // 1
    pub field3: String, // 3
}

impl RemoteMatches {
    /// Mirrors generated/utils.go: an absent key means "not in library".
    pub fn media_key(&self) -> Option<&str> {
        let key = self
            .field1
            .as_ref()?
            .field2
            .as_ref()?
            .field2
            .as_ref()?
            .media_key
            .as_str();
        if key.is_empty() {
            None
        } else {
            Some(key)
        }
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        if let Some(field1) = &self.field1 {
            wire::put_msg(out, 1, &field1.encode_to_vec());
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.field1 = Some(RemoteMatchesField1::decode(reader.get_bytes(wt)?)?),
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl RemoteMatchesField1 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if let Some(field2) = &self.field2 {
            wire::put_msg(out, 2, &field2.encode_to_vec());
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                2 => {
                    msg.field2 =
                        Some(RemoteMatchesField1Field2::decode(reader.get_bytes(wt)?)?)
                }
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl RemoteMatchesField1Field2 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if let Some(field1) = &self.field1 {
            wire::put_msg(out, 1, &field1.encode_to_vec());
        }
        if let Some(field2) = &self.field2 {
            wire::put_msg(out, 2, &field2.encode_to_vec());
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => {
                    msg.field1 = Some(RemoteMatchesField1Field2Field1::decode(
                        reader.get_bytes(wt)?,
                    )?)
                }
                2 => {
                    msg.field2 = Some(RemoteMatchesField1Field2Field2::decode(
                        reader.get_bytes(wt)?,
                    )?)
                }
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl RemoteMatchesField1Field2Field1 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if !self.sha1_hash.is_empty() {
            wire::put_bytes(out, 1, &self.sha1_hash);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.sha1_hash = reader.get_bytes(wt)?.to_vec(),
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl RemoteMatchesField1Field2Field2 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if !self.media_key.is_empty() {
            wire::put_string(out, 1, &self.media_key);
        }
        if let Some(field6) = &self.field6 {
            wire::put_msg(out, 6, &field6.encode_to_vec());
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.media_key = reader.get_string(wt)?,
                6 => msg.field6 = Some(RemoteMatchesField6::decode(reader.get_bytes(wt)?)?),
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl RemoteMatchesField6 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if !self.media_key.is_empty() {
            wire::put_string(out, 1, &self.media_key);
        }
        if let Some(field2) = &self.field2 {
            wire::put_msg(out, 2, &field2.encode_to_vec());
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.media_key = reader.get_string(wt)?,
                2 => {
                    msg.field2 =
                        Some(RemoteMatchesField6Field2::decode(reader.get_bytes(wt)?)?)
                }
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl RemoteMatchesField6Field2 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if !self.field1.is_empty() {
            wire::put_string(out, 1, &self.field1);
        }
        if !self.field3.is_empty() {
            wire::put_string(out, 3, &self.field3);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.field1 = reader.get_string(wt)?,
                3 => msg.field3 = reader.get_string(wt)?,
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

// ---------------------------------------------------------------------------
// CommitToken.proto + Scotty finalize token (core/scotty_token.go)
// ---------------------------------------------------------------------------

/// Decoded form of the Scotty finalize token, used only by the legacy
/// single-file CommitUpload path (core/api.go CommitUpload takes a
/// `*generated.CommitToken`). Live Photos never decode it — see
/// [`ScottyToken`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CommitToken {
    pub field1: i64,       // 1
    pub field2: Vec<u8>,   // 2
}

impl CommitToken {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if self.field1 != 0 {
            wire::put_i64(out, 1, self.field1);
        }
        if !self.field2.is_empty() {
            wire::put_bytes(out, 2, &self.field2);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.field1 = reader.get_i64(wt)?,
                2 => msg.field2 = reader.get_bytes(wt)?.to_vec(),
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

/// The Scotty finalize token as returned by the upload PUT: an opaque
/// envelope that must be preserved byte-for-byte (core/scotty_token.go —
/// "its contents … must not be interpreted … or normalized by decoding and
/// re-encoding"). CreateMediaItems embeds these raw bytes as the blueprint
/// upload token; only the legacy CommitUpload path decodes them, via
/// [`ScottyToken::commit_token`].
#[derive(Debug, Clone, PartialEq)]
pub struct ScottyToken {
    raw: Vec<u8>,
}

/// Go's name for the same type (core/scotty_token.go).
pub type ScottyFinalizeToken = ScottyToken;

/// The only envelope version observed (core/scotty_token.go
/// `scottyFinalizeTokenVersion`).
const SCOTTY_TOKEN_VERSION: u64 = 2;

impl ScottyToken {
    /// Validates and wraps raw finalize-response bytes, porting
    /// `ParseScottyFinalizeToken`: exactly one field 1 (varint, == 2) and
    /// exactly one field 2 (non-empty bytes); other fields are tolerated and
    /// skipped, exactly as Go's protowire walk tolerates them.
    pub fn parse(raw: &[u8]) -> Result<Self> {
        let mut field1_count = 0usize;
        let mut field2_count = 0usize;
        let mut reader = Reader::new(raw);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => {
                    if wt != WIRE_VARINT {
                        return Err(Error::Protocol(format!(
                            "Scotty finalize token field 1 has wire type {wt}"
                        )));
                    }
                    let version = reader.read_varint()?;
                    if version != SCOTTY_TOKEN_VERSION {
                        return Err(Error::Protocol(format!(
                            "unsupported Scotty finalize token version {version}"
                        )));
                    }
                    field1_count += 1;
                }
                2 => {
                    if wt != WIRE_LEN {
                        return Err(Error::Protocol(format!(
                            "Scotty finalize token field 2 has wire type {wt}"
                        )));
                    }
                    let opaque = reader.read_bytes()?;
                    if opaque.is_empty() {
                        return Err(Error::Protocol(
                            "Scotty finalize token field 2 is empty".to_string(),
                        ));
                    }
                    field2_count += 1;
                }
                _ => reader.skip_field(wt)?,
            }
        }
        if field1_count != 1 {
            return Err(Error::Protocol(format!(
                "Scotty finalize token contains {field1_count} field-1 values, want 1"
            )));
        }
        if field2_count != 1 {
            return Err(Error::Protocol(format!(
                "Scotty finalize token contains {field2_count} field-2 values, want 1"
            )));
        }
        Ok(ScottyToken {
            raw: raw.to_vec(),
        })
    }

    /// The raw envelope bytes, byte-for-byte as received.
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    pub fn into_raw(self) -> Vec<u8> {
        self.raw
    }

    /// Legacy path (core/scotty_token.go `legacyCommitToken`): re-validates,
    /// then decodes the envelope as a [`CommitToken`] for CommitUpload.
    pub fn commit_token(&self) -> Result<CommitToken> {
        let validated = ScottyToken::parse(&self.raw)?;
        CommitToken::decode(&validated.raw)
    }
}

// ---------------------------------------------------------------------------
// CommitUpload.proto — legacy single-file commit (core/api.go CommitUpload)
// ---------------------------------------------------------------------------

/// Legacy commit request. Field 1 is the upload item, field 2 the device
/// identity, field 3 the constant bytes `[1, 3]` as sent by Go.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CommitUpload {
    pub field1: Option<CommitUploadField1>, // 1
    pub field2: Option<DeviceInfo>,          // 2
    pub field3: Vec<u8>,                     // 3
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CommitUploadField1 {
    /// The decoded Scotty token (same shape as CommitToken.proto).
    pub field1: Option<CommitToken>,              // 1
    pub file_name: String,                        // 2
    pub sha1_hash: Vec<u8>,                       // 3
    pub field4: Option<CommitUploadField4>,       // 4
    pub quality: i64,                             // 7 — 3 = original, 1 = saver (core/api.go)
    /// Fingerprint subtree from CommitUpload.proto field 8. Go's core never
    /// constructs it (the pointer stays nil in core/api.go), so it is kept
    /// as raw embedded-message bytes: decode preserves it, encode re-emits
    /// it verbatim, and it is `None` for every request we build.
    pub field8: Option<Vec<u8>>,                  // 8 — raw, see note
    pub field10: i64,                             // 10 — Go sends 1
    pub field17: i64,                             // 17 — never set by Go's core
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CommitUploadField4 {
    pub file_last_modified_timestamp: i64, // 1
    /// Go sends the magic constant 46_000_000 (core/api.go `unknownInt`).
    pub field2: i64,                       // 2
}

impl CommitUpload {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if let Some(field1) = &self.field1 {
            wire::put_msg(out, 1, &field1.encode_to_vec());
        }
        if let Some(field2) = &self.field2 {
            wire::put_msg(out, 2, &field2.encode_to_vec());
        }
        if !self.field3.is_empty() {
            wire::put_bytes(out, 3, &self.field3);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.field1 = Some(CommitUploadField1::decode(reader.get_bytes(wt)?)?),
                2 => msg.field2 = Some(DeviceInfo::decode(reader.get_bytes(wt)?)?),
                3 => msg.field3 = reader.get_bytes(wt)?.to_vec(),
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl CommitUploadField1 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if let Some(field1) = &self.field1 {
            wire::put_msg(out, 1, &field1.encode_to_vec());
        }
        if !self.file_name.is_empty() {
            wire::put_string(out, 2, &self.file_name);
        }
        if !self.sha1_hash.is_empty() {
            wire::put_bytes(out, 3, &self.sha1_hash);
        }
        if let Some(field4) = &self.field4 {
            wire::put_msg(out, 4, &field4.encode_to_vec());
        }
        if self.quality != 0 {
            wire::put_i64(out, 7, self.quality);
        }
        if let Some(field8) = &self.field8 {
            wire::put_msg(out, 8, field8);
        }
        if self.field10 != 0 {
            wire::put_i64(out, 10, self.field10);
        }
        if self.field17 != 0 {
            wire::put_i64(out, 17, self.field17);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.field1 = Some(CommitToken::decode(reader.get_bytes(wt)?)?),
                2 => msg.file_name = reader.get_string(wt)?,
                3 => msg.sha1_hash = reader.get_bytes(wt)?.to_vec(),
                4 => msg.field4 = Some(CommitUploadField4::decode(reader.get_bytes(wt)?)?),
                7 => msg.quality = reader.get_i64(wt)?,
                8 => msg.field8 = Some(reader.get_bytes(wt)?.to_vec()),
                10 => msg.field10 = reader.get_i64(wt)?,
                17 => msg.field17 = reader.get_i64(wt)?,
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl CommitUploadField4 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if self.file_last_modified_timestamp != 0 {
            wire::put_i64(out, 1, self.file_last_modified_timestamp);
        }
        if self.field2 != 0 {
            wire::put_i64(out, 2, self.field2);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.file_last_modified_timestamp = reader.get_i64(wt)?,
                2 => msg.field2 = reader.get_i64(wt)?,
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

/// CommitUploadResponse.proto is **dead schema**: Go's generated type exists
/// but core never decodes a commit response with it — commit responses are
/// decoded as [`CreateMediaItemsResponse`] (core/api.go
/// `parseCreateMediaItemsResponse`), and unparseable 2xx bodies are treated
/// as ambiguous, never retried. This projection therefore keeps only the
/// verified media-key path (field1.field3.media_key, mirroring
/// CommitUploadResponse.proto fields 1→3→1); every other fingerprint field
/// is skipped on decode and omitted on encode.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CommitUploadResponse {
    pub field1: Option<CommitUploadResponseField1>, // 1
    pub field2: Option<CommitUploadResponseField2>, // 2
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CommitUploadResponseField1 {
    pub field1: Option<CommitToken>,               // 1 — { field1, field2 } pair
    pub field2: i64,                               // 2
    pub field3: Option<CommitUploadResponseField3>, // 3
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CommitUploadResponseField3 {
    pub media_key: String, // 1
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CommitUploadResponseField2 {
    pub field1: i64,  // 1
    pub field2: i64,  // 2
    pub field4: i64,  // 4
    pub field6: i64,  // 6
    pub field7: i64,  // 7
    pub field11: i64, // 11
}

impl CommitUploadResponse {
    pub fn media_key(&self) -> Option<&str> {
        let key = self.field1.as_ref()?.field3.as_ref()?.media_key.as_str();
        if key.is_empty() {
            None
        } else {
            Some(key)
        }
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        if let Some(field1) = &self.field1 {
            wire::put_msg(out, 1, &field1.encode_to_vec());
        }
        if let Some(field2) = &self.field2 {
            wire::put_msg(out, 2, &field2.encode_to_vec());
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => {
                    msg.field1 =
                        Some(CommitUploadResponseField1::decode(reader.get_bytes(wt)?)?)
                }
                2 => {
                    msg.field2 =
                        Some(CommitUploadResponseField2::decode(reader.get_bytes(wt)?)?)
                }
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl CommitUploadResponseField1 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if let Some(field1) = &self.field1 {
            wire::put_msg(out, 1, &field1.encode_to_vec());
        }
        if self.field2 != 0 {
            wire::put_i64(out, 2, self.field2);
        }
        if let Some(field3) = &self.field3 {
            wire::put_msg(out, 3, &field3.encode_to_vec());
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.field1 = Some(CommitToken::decode(reader.get_bytes(wt)?)?),
                2 => msg.field2 = reader.get_i64(wt)?,
                3 => {
                    msg.field3 =
                        Some(CommitUploadResponseField3::decode(reader.get_bytes(wt)?)?)
                }
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl CommitUploadResponseField3 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if !self.media_key.is_empty() {
            wire::put_string(out, 1, &self.media_key);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.media_key = reader.get_string(wt)?,
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl CommitUploadResponseField2 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if self.field1 != 0 {
            wire::put_i64(out, 1, self.field1);
        }
        if self.field2 != 0 {
            wire::put_i64(out, 2, self.field2);
        }
        if self.field4 != 0 {
            wire::put_i64(out, 4, self.field4);
        }
        if self.field6 != 0 {
            wire::put_i64(out, 6, self.field6);
        }
        if self.field7 != 0 {
            wire::put_i64(out, 7, self.field7);
        }
        if self.field11 != 0 {
            wire::put_i64(out, 11, self.field11);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.field1 = reader.get_i64(wt)?,
                2 => msg.field2 = reader.get_i64(wt)?,
                4 => msg.field4 = reader.get_i64(wt)?,
                6 => msg.field6 = reader.get_i64(wt)?,
                7 => msg.field7 = reader.get_i64(wt)?,
                11 => msg.field11 = reader.get_i64(wt)?,
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

// ---------------------------------------------------------------------------
// CreateMediaItems.proto — modern commit (Live Photos and, via response,
// every commit). Built by core/create_media_items.go.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CreateMediaItemsRequest {
    pub blueprint_array: Vec<MediaItemBlueprint>,     // 1
    pub upload_device_info: Option<DeviceInfo>,       // 2
    // Field 3 (clientCapabilityArray) is reserved/absent in the captures.
    pub result_item_mask: Vec<u8>,                    // 5 — opaque, see create_media_items.go
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MediaItemBlueprint {
    pub upload_token: Vec<u8>,                            // 1 — ScottyToken raw bytes
    pub file_name: String,                                // 2
    pub source_sha1: Vec<u8>,                             // 3
    pub filesystem_create_time: Option<UploadTimestamp>,  // 5
    pub filesystem_mod_time: Option<UploadTimestamp>,     // 6
    pub storage_policy: i64,                              // 7 — 3 unlimited / 1 saver
    pub reconcile_info: Option<ReconcileInfo>,            // 9
    pub upload_quality: i64,                              // 10
    pub live_photo_info: Option<LivePhotoInfo>,           // 24
}

/// ReconcileType enum (CreateMediaItems.proto).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReconcileType {
    #[default]
    Unknown = 0,
    Phodeo = 1,
    VideoOriginal = 2,
}

impl ReconcileType {
    pub fn from_i64(value: i64) -> Self {
        match value {
            1 => ReconcileType::Phodeo,
            2 => ReconcileType::VideoOriginal,
            _ => ReconcileType::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReconcileInfo {
    pub reconcile_type: ReconcileType,                        // 2
    pub source_sha1: Vec<u8>,                                 // 3
    pub photo_upload_blueprint: Option<Box<MediaItemBlueprint>>, // 4
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct UploadTimestamp {
    pub seconds: i64,     // 1
    pub nanoseconds: i64, // 2
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct LivePhotoInfo {
    pub video_upload_token: Vec<u8>, // 1 — ScottyToken raw bytes
    pub video_source_sha1: Vec<u8>,  // 2
    // Field 3 (videoCrc32C) is reserved/absent in the captures.
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CreateMediaItemsResponse {
    pub item: Vec<CreateMediaItemResponseItem>, // 1
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CreateMediaItemResponseItem {
    pub result_item: Option<CreateMediaItemResult>, // 3
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CreateMediaItemResult {
    pub media_key: String, // 1
}

impl CreateMediaItemsRequest {
    pub fn encode(&self, out: &mut Vec<u8>) {
        for blueprint in &self.blueprint_array {
            wire::put_msg(out, 1, &blueprint.encode_to_vec());
        }
        if let Some(device) = &self.upload_device_info {
            wire::put_msg(out, 2, &device.encode_to_vec());
        }
        if !self.result_item_mask.is_empty() {
            wire::put_bytes(out, 5, &self.result_item_mask);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg
                    .blueprint_array
                    .push(MediaItemBlueprint::decode(reader.get_bytes(wt)?)?),
                2 => msg.upload_device_info = Some(DeviceInfo::decode(reader.get_bytes(wt)?)?),
                5 => msg.result_item_mask = reader.get_bytes(wt)?.to_vec(),
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl MediaItemBlueprint {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if !self.upload_token.is_empty() {
            wire::put_bytes(out, 1, &self.upload_token);
        }
        if !self.file_name.is_empty() {
            wire::put_string(out, 2, &self.file_name);
        }
        if !self.source_sha1.is_empty() {
            wire::put_bytes(out, 3, &self.source_sha1);
        }
        if let Some(ts) = &self.filesystem_create_time {
            wire::put_msg(out, 5, &ts.encode_to_vec());
        }
        if let Some(ts) = &self.filesystem_mod_time {
            wire::put_msg(out, 6, &ts.encode_to_vec());
        }
        if self.storage_policy != 0 {
            wire::put_i64(out, 7, self.storage_policy);
        }
        if let Some(info) = &self.reconcile_info {
            wire::put_msg(out, 9, &info.encode_to_vec());
        }
        if self.upload_quality != 0 {
            wire::put_i64(out, 10, self.upload_quality);
        }
        if let Some(info) = &self.live_photo_info {
            wire::put_msg(out, 24, &info.encode_to_vec());
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.upload_token = reader.get_bytes(wt)?.to_vec(),
                2 => msg.file_name = reader.get_string(wt)?,
                3 => msg.source_sha1 = reader.get_bytes(wt)?.to_vec(),
                5 => {
                    msg.filesystem_create_time =
                        Some(UploadTimestamp::decode(reader.get_bytes(wt)?)?)
                }
                6 => {
                    msg.filesystem_mod_time =
                        Some(UploadTimestamp::decode(reader.get_bytes(wt)?)?)
                }
                7 => msg.storage_policy = reader.get_i64(wt)?,
                9 => msg.reconcile_info = Some(ReconcileInfo::decode(reader.get_bytes(wt)?)?),
                10 => msg.upload_quality = reader.get_i64(wt)?,
                24 => {
                    msg.live_photo_info = Some(LivePhotoInfo::decode(reader.get_bytes(wt)?)?)
                }
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl ReconcileInfo {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if self.reconcile_type != ReconcileType::Unknown {
            wire::put_i64(out, 2, self.reconcile_type as i64);
        }
        if !self.source_sha1.is_empty() {
            wire::put_bytes(out, 3, &self.source_sha1);
        }
        if let Some(blueprint) = &self.photo_upload_blueprint {
            wire::put_msg(out, 4, &blueprint.encode_to_vec());
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                2 => msg.reconcile_type = ReconcileType::from_i64(reader.get_i64(wt)?),
                3 => msg.source_sha1 = reader.get_bytes(wt)?.to_vec(),
                4 => {
                    msg.photo_upload_blueprint =
                        Some(Box::new(MediaItemBlueprint::decode(reader.get_bytes(wt)?)?))
                }
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl UploadTimestamp {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if self.seconds != 0 {
            wire::put_i64(out, 1, self.seconds);
        }
        if self.nanoseconds != 0 {
            wire::put_i64(out, 2, self.nanoseconds);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.seconds = reader.get_i64(wt)?,
                2 => msg.nanoseconds = reader.get_i64(wt)?,
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl LivePhotoInfo {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if !self.video_upload_token.is_empty() {
            wire::put_bytes(out, 1, &self.video_upload_token);
        }
        if !self.video_source_sha1.is_empty() {
            wire::put_bytes(out, 2, &self.video_source_sha1);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.video_upload_token = reader.get_bytes(wt)?.to_vec(),
                2 => msg.video_source_sha1 = reader.get_bytes(wt)?.to_vec(),
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl CreateMediaItemsResponse {
    /// Mirrors core/api.go `parseCreateMediaItemsResponse`: the first
    /// non-empty media key wins; absence is an error at the call site.
    pub fn first_media_key(&self) -> Option<&str> {
        for item in &self.item {
            if let Some(result) = &item.result_item {
                if !result.media_key.is_empty() {
                    return Some(&result.media_key);
                }
            }
        }
        None
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        for item in &self.item {
            wire::put_msg(out, 1, &item.encode_to_vec());
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg
                    .item
                    .push(CreateMediaItemResponseItem::decode(reader.get_bytes(wt)?)?),
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl CreateMediaItemResponseItem {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if let Some(result) = &self.result_item {
            wire::put_msg(out, 3, &result.encode_to_vec());
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                3 => {
                    msg.result_item =
                        Some(CreateMediaItemResult::decode(reader.get_bytes(wt)?)?)
                }
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl CreateMediaItemResult {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if !self.media_key.is_empty() {
            wire::put_string(out, 1, &self.media_key);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.media_key = reader.get_string(wt)?,
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

// ---------------------------------------------------------------------------
// CreateAlbum.proto / CreateAlbumResponse.proto (core/api.go CreateAlbum)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CreateAlbum {
    pub album_name: String,                      // 1
    pub timestamp: i64,                          // 2 — unix seconds
    pub field3: i64,                             // 3 — Go sends 1
    pub media_keys: Vec<CreateAlbumField4>,      // 4
    pub field6: Option<CreateAlbumField6>,       // 6 — empty, always Some in Go
    pub field7: Option<CreateAlbumField7>,       // 7 — Go sends { field1: 3 }
    pub device_info: Option<DeviceInfo>,         // 8
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CreateAlbumField4 {
    pub field1: Option<CreateAlbumField4Field1>, // 1
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CreateAlbumField4Field1 {
    pub media_key: String, // 1
}

/// Empty message; presence on the wire is its only content.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CreateAlbumField6 {}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CreateAlbumField7 {
    pub field1: i64, // 1
}

impl CreateAlbum {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if !self.album_name.is_empty() {
            wire::put_string(out, 1, &self.album_name);
        }
        if self.timestamp != 0 {
            wire::put_i64(out, 2, self.timestamp);
        }
        if self.field3 != 0 {
            wire::put_i64(out, 3, self.field3);
        }
        for key in &self.media_keys {
            wire::put_msg(out, 4, &key.encode_to_vec());
        }
        if let Some(field6) = &self.field6 {
            wire::put_msg(out, 6, &field6.encode_to_vec());
        }
        if let Some(field7) = &self.field7 {
            wire::put_msg(out, 7, &field7.encode_to_vec());
        }
        if let Some(device) = &self.device_info {
            wire::put_msg(out, 8, &device.encode_to_vec());
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.album_name = reader.get_string(wt)?,
                2 => msg.timestamp = reader.get_i64(wt)?,
                3 => msg.field3 = reader.get_i64(wt)?,
                4 => msg
                    .media_keys
                    .push(CreateAlbumField4::decode(reader.get_bytes(wt)?)?),
                6 => msg.field6 = Some(CreateAlbumField6::decode(reader.get_bytes(wt)?)?),
                7 => msg.field7 = Some(CreateAlbumField7::decode(reader.get_bytes(wt)?)?),
                8 => msg.device_info = Some(DeviceInfo::decode(reader.get_bytes(wt)?)?),
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl CreateAlbumField4 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if let Some(field1) = &self.field1 {
            wire::put_msg(out, 1, &field1.encode_to_vec());
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => {
                    msg.field1 =
                        Some(CreateAlbumField4Field1::decode(reader.get_bytes(wt)?)?)
                }
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl CreateAlbumField4Field1 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if !self.media_key.is_empty() {
            wire::put_string(out, 1, &self.media_key);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.media_key = reader.get_string(wt)?,
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl CreateAlbumField6 {
    pub fn encode(&self, _out: &mut Vec<u8>) {}

    pub fn encode_to_vec(&self) -> Vec<u8> {
        Vec::new()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = Reader::new(bytes);
        while let Some((_, wt)) = reader.read_tag()? {
            reader.skip_field(wt)?;
        }
        Ok(Self {})
    }
}

impl CreateAlbumField7 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if self.field1 != 0 {
            wire::put_i64(out, 1, self.field1);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.field1 = reader.get_i64(wt)?,
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CreateAlbumResponse {
    pub field1: Option<CreateAlbumResponseField1>, // 1
    pub field2: Vec<String>,                       // 2
    pub field3: Option<CreateAlbumResponseField3>, // 3
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CreateAlbumResponseField1 {
    pub album_media_key: String, // 1
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CreateAlbumResponseField3 {
    pub field1: Vec<CreateAlbumResponseField3Field1>, // 1
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CreateAlbumResponseField3Field1 {
    pub input_media_key: String, // 1
    pub result_key: String,      // 2
    pub status: i64,             // 3
}

impl CreateAlbumResponse {
    /// The album key core/api.go CreateAlbum returns; empty means failure
    /// at the call site.
    pub fn album_media_key(&self) -> Option<&str> {
        let key = self.field1.as_ref()?.album_media_key.as_str();
        if key.is_empty() {
            None
        } else {
            Some(key)
        }
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        if let Some(field1) = &self.field1 {
            wire::put_msg(out, 1, &field1.encode_to_vec());
        }
        for value in &self.field2 {
            wire::put_string(out, 2, value);
        }
        if let Some(field3) = &self.field3 {
            wire::put_msg(out, 3, &field3.encode_to_vec());
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => {
                    msg.field1 =
                        Some(CreateAlbumResponseField1::decode(reader.get_bytes(wt)?)?)
                }
                2 => msg.field2.push(reader.get_string(wt)?),
                3 => {
                    msg.field3 =
                        Some(CreateAlbumResponseField3::decode(reader.get_bytes(wt)?)?)
                }
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl CreateAlbumResponseField1 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if !self.album_media_key.is_empty() {
            wire::put_string(out, 1, &self.album_media_key);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.album_media_key = reader.get_string(wt)?,
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl CreateAlbumResponseField3 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        for item in &self.field1 {
            wire::put_msg(out, 1, &item.encode_to_vec());
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg
                    .field1
                    .push(CreateAlbumResponseField3Field1::decode(
                        reader.get_bytes(wt)?,
                    )?),
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl CreateAlbumResponseField3Field1 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if !self.input_media_key.is_empty() {
            wire::put_string(out, 1, &self.input_media_key);
        }
        if !self.result_key.is_empty() {
            wire::put_string(out, 2, &self.result_key);
        }
        if self.status != 0 {
            wire::put_i64(out, 3, self.status);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.input_media_key = reader.get_string(wt)?,
                2 => msg.result_key = reader.get_string(wt)?,
                3 => msg.status = reader.get_i64(wt)?,
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

// ---------------------------------------------------------------------------
// AddMediaToAlbum.proto (core/api.go AddMediaToAlbum; no response body used)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AddMediaToAlbum {
    pub media_keys: Vec<String>,                    // 1
    pub album_media_key: String,                    // 2
    pub field5: Option<AddMediaToAlbumField5>,      // 5 — Go sends { field1: 2 }
    pub device_info: Option<DeviceInfo>,            // 6
    pub timestamp: i64,                             // 7 — unix seconds
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AddMediaToAlbumField5 {
    pub field1: i64, // 1
}

impl AddMediaToAlbum {
    pub fn encode(&self, out: &mut Vec<u8>) {
        for key in &self.media_keys {
            wire::put_string(out, 1, key);
        }
        if !self.album_media_key.is_empty() {
            wire::put_string(out, 2, &self.album_media_key);
        }
        if let Some(field5) = &self.field5 {
            wire::put_msg(out, 5, &field5.encode_to_vec());
        }
        if let Some(device) = &self.device_info {
            wire::put_msg(out, 6, &device.encode_to_vec());
        }
        if self.timestamp != 0 {
            wire::put_i64(out, 7, self.timestamp);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.media_keys.push(reader.get_string(wt)?),
                2 => msg.album_media_key = reader.get_string(wt)?,
                5 => {
                    msg.field5 =
                        Some(AddMediaToAlbumField5::decode(reader.get_bytes(wt)?)?)
                }
                6 => msg.device_info = Some(DeviceInfo::decode(reader.get_bytes(wt)?)?),
                7 => msg.timestamp = reader.get_i64(wt)?,
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

impl AddMediaToAlbumField5 {
    pub fn encode(&self, out: &mut Vec<u8>) {
        if self.field1 != 0 {
            wire::put_i64(out, 1, self.field1);
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut reader = Reader::new(bytes);
        while let Some((field, wt)) = reader.read_tag()? {
            match field {
                1 => msg.field1 = reader.get_i64(wt)?,
                _ => reader.skip_field(wt)?,
            }
        }
        Ok(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_bytes(hex: &str) -> Vec<u8> {
        assert!(hex.len() % 2 == 0);
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn get_upload_token_fixture() {
        // Derivation (GetUploadToken.proto + core/api.go GetUploadToken,
        // which always sends f1=2, f2=2, f3=1, f4=3):
        //   field 1 varint 2     -> 08 02
        //   field 2 varint 2     -> 10 02
        //   field 3 varint 1     -> 18 01
        //   field 4 varint 3     -> 20 03
        //   field 7 varint 12345 -> 38 b9 60   (12345 = 0x3039 -> varint b9 60)
        let msg = GetUploadToken {
            f1: 2,
            f2: 2,
            f3: 1,
            f4: 3,
            file_size_bytes: 12345,
        };
        let expected = hex_bytes("080210021801200338b960");
        assert_eq!(msg.encode_to_vec(), expected);
        assert_eq!(GetUploadToken::decode(&expected).unwrap(), msg);
    }

    #[test]
    fn hash_check_fixture() {
        // Derivation (HashCheck.proto + core/api.go FindRemoteMediaByHash)
        // for sha1 = 00 01 02 ... 13 (20 bytes):
        //   innermost { field1 bytes sha1 }        -> 0a 14 <20 bytes>   (22 B)
        //   middle { field1 = innermost (22 B),
        //            field2 = {} }                 -> 0a 16 <22 B> 12 00 (26 B)
        //   top { field1 = middle }                -> 0a 1a <26 B>
        let sha1: Vec<u8> = (0u8..20).collect();
        let msg = HashCheck::new(&sha1);
        let expected =
            hex_bytes("0a1a0a160a14000102030405060708090a0b0c0d0e0f101112131200");
        assert_eq!(msg.encode_to_vec(), expected);
        assert_eq!(HashCheck::decode(&expected).unwrap(), msg);
    }

    #[test]
    fn commit_upload_fixture() {
        // Derivation (CommitUpload.proto + core/api.go CommitUpload) for:
        // token {1: 2, 2: [aa bb]}, file "a.jpg", sha1 = 20 x 0x11,
        // field4 {1: 1_700_000_000, 2: 46_000_000} (46_000_000 is Go's
        // `unknownInt` magic constant), quality 3, field10 1,
        // device {model "Pixel XL", make "Google", api 28}, field3 [1, 3]:
        //   token            -> 08 02 12 02 aa bb                       (6 B)
        //   item field1      -> 0a 06 <token>
        //   item file_name   -> 12 05 "a.jpg"
        //   item sha1        -> 1a 14 <20 x 11>
        //   item field4      -> 22 0b 08 80e2cfaa06 10 80cff715       (11 B)
        //   item quality     -> 38 03
        //   item field10     -> 50 01
        //   item (54 B)      -> 0a 36 <item>
        //   device (20 B)    -> 12 14 1a 08 "Pixel XL" 22 06 "Google" 28 1c
        //   field3           -> 1a 02 01 03
        let msg = CommitUpload {
            field1: Some(CommitUploadField1 {
                field1: Some(CommitToken {
                    field1: 2,
                    field2: vec![0xaa, 0xbb],
                }),
                file_name: "a.jpg".to_string(),
                sha1_hash: vec![0x11; 20],
                field4: Some(CommitUploadField4 {
                    file_last_modified_timestamp: 1_700_000_000,
                    field2: 46_000_000,
                }),
                quality: 3,
                field8: None,
                field10: 1,
                field17: 0,
            }),
            field2: Some(DeviceInfo {
                model: "Pixel XL".to_string(),
                make: "Google".to_string(),
                android_api_version: 28,
            }),
            field3: vec![1, 3],
        };
        let expected = hex_bytes(
            "0a360a0608021202aabb1205612e6a70671a141111111111111111111111111111111111111111220b0880e2cfaa061080cff7153803500112141a08506978656c20584c2206476f6f676c65281c1a020103",
        );
        assert_eq!(msg.encode_to_vec(), expected);
        assert_eq!(CommitUpload::decode(&expected).unwrap(), msg);
    }

    #[test]
    fn remote_matches_media_key_fixture() {
        // Hand-built per RemoteMatches.proto, media key "MK" at 1.2.2.1:
        //   {1: "MK"} -> 0a 02 4d 4b; wrapped in field 2 -> 12 04 <4 B>;
        //   wrapped in field 2 -> 12 06 <6 B>; wrapped in field 1 -> 0a 08 <8 B>
        let bytes = hex_bytes("0a08120612040a024d4b");
        let msg = RemoteMatches::decode(&bytes).unwrap();
        assert_eq!(msg.media_key(), Some("MK"));
        assert_eq!(msg.encode_to_vec(), bytes);
        // An empty response parses to "not present".
        assert_eq!(RemoteMatches::default().media_key(), None);
    }

    #[test]
    fn scotty_token_validation_and_legacy_decode() {
        // Envelope: field 1 varint 2 -> 08 02; field 2 bytes [1,2,3] -> 12 03 01 02 03.
        let raw = hex_bytes("08021203010203");
        let token = ScottyToken::parse(&raw).unwrap();
        assert_eq!(token.raw(), &raw[..]);
        // Legacy decode (core/scotty_token.go legacyCommitToken).
        assert_eq!(
            token.commit_token().unwrap(),
            CommitToken {
                field1: 2,
                field2: vec![1, 2, 3],
            }
        );

        // Wrong version.
        assert!(ScottyToken::parse(&hex_bytes("08031203010203")).is_err());
        // Duplicate field 1.
        assert!(ScottyToken::parse(&hex_bytes("080208021203010203")).is_err());
        // Empty field 2.
        assert!(ScottyToken::parse(&hex_bytes("08021200")).is_err());
        // Field 2 with wrong wire type (varint instead of bytes).
        assert!(ScottyToken::parse(&hex_bytes("08021007")).is_err());
        // Missing field 2 entirely.
        assert!(ScottyToken::parse(&hex_bytes("0802")).is_err());
    }

    #[test]
    fn create_media_items_live_photo_roundtrip() {
        let token_raw = hex_bytes("08021203010203");
        let photo_token = ScottyToken::parse(&token_raw).unwrap();
        let video_token = ScottyToken::parse(&token_raw).unwrap();
        let request = CreateMediaItemsRequest {
            blueprint_array: vec![MediaItemBlueprint {
                upload_token: photo_token.raw().to_vec(),
                file_name: "IMG_0001.HEIC".to_string(),
                source_sha1: vec![0x22; 20],
                filesystem_create_time: Some(UploadTimestamp {
                    seconds: 1_700_000_000,
                    nanoseconds: 0,
                }),
                filesystem_mod_time: Some(UploadTimestamp {
                    seconds: 1_700_000_100,
                    nanoseconds: 5,
                }),
                storage_policy: 3,
                reconcile_info: None,
                upload_quality: 1,
                live_photo_info: Some(LivePhotoInfo {
                    video_upload_token: video_token.raw().to_vec(),
                    video_source_sha1: vec![0x33; 20],
                }),
            }],
            upload_device_info: Some(DeviceInfo {
                model: "Pixel XL".to_string(),
                make: "Google".to_string(),
                android_api_version: 28,
            }),
            result_item_mask: vec![0x0a, 0x00],
        };
        let encoded = request.encode_to_vec();
        // Blueprint field 24 (live_photo_info, wire type 2) has the two-byte
        // tag c2 01: (24 << 3) | 2 = 194 -> varint c2 01.
        assert!(
            encoded.windows(2).any(|w| w == &[0xc2, 0x01][..]),
            "field-24 tag must appear in the blueprint encoding"
        );
        assert_eq!(CreateMediaItemsRequest::decode(&encoded).unwrap(), request);
    }

    #[test]
    fn create_media_items_reconcile_roundtrip() {
        let request = CreateMediaItemsRequest {
            blueprint_array: vec![MediaItemBlueprint {
                upload_token: hex_bytes("08021203010203"),
                file_name: "IMG_0002.MOV".to_string(),
                source_sha1: vec![0x44; 20],
                filesystem_create_time: None,
                filesystem_mod_time: None,
                storage_policy: 3,
                reconcile_info: Some(ReconcileInfo {
                    reconcile_type: ReconcileType::Phodeo,
                    source_sha1: vec![0x55; 20],
                    photo_upload_blueprint: None,
                }),
                upload_quality: 1,
                live_photo_info: None,
            }],
            upload_device_info: None,
            result_item_mask: Vec::new(),
        };
        let encoded = request.encode_to_vec();
        assert_eq!(CreateMediaItemsRequest::decode(&encoded).unwrap(), request);
    }

    #[test]
    fn create_media_items_response_first_media_key() {
        // { item: { result_item: { media_key: "K1" } } } built by encoding.
        let response = CreateMediaItemsResponse {
            item: vec![
                CreateMediaItemResponseItem { result_item: None },
                CreateMediaItemResponseItem {
                    result_item: Some(CreateMediaItemResult {
                        media_key: "K1".to_string(),
                    }),
                },
            ],
        };
        let encoded = response.encode_to_vec();
        let decoded = CreateMediaItemsResponse::decode(&encoded).unwrap();
        assert_eq!(decoded, response);
        assert_eq!(decoded.first_media_key(), Some("K1"));
    }

    #[test]
    fn album_messages_roundtrip() {
        let create = CreateAlbum {
            album_name: "Trip".to_string(),
            timestamp: 1_700_000_000,
            field3: 1,
            media_keys: vec![CreateAlbumField4 {
                field1: Some(CreateAlbumField4Field1 {
                    media_key: "MK1".to_string(),
                }),
            }],
            field6: Some(CreateAlbumField6 {}),
            field7: Some(CreateAlbumField7 { field1: 3 }),
            device_info: Some(DeviceInfo {
                model: "Pixel XL".to_string(),
                make: "Google".to_string(),
                android_api_version: 28,
            }),
        };
        let encoded = create.encode_to_vec();
        // The empty field-6 message must be present as tag + zero length.
        assert!(encoded.windows(2).any(|w| w == &[0x32, 0x00][..]));
        assert_eq!(CreateAlbum::decode(&encoded).unwrap(), create);

        let response = CreateAlbumResponse {
            field1: Some(CreateAlbumResponseField1 {
                album_media_key: "ALBUM1".to_string(),
            }),
            field2: vec!["extra".to_string()],
            field3: Some(CreateAlbumResponseField3 {
                field1: vec![CreateAlbumResponseField3Field1 {
                    input_media_key: "MK1".to_string(),
                    result_key: "RK1".to_string(),
                    status: 1,
                }],
            }),
        };
        let encoded = response.encode_to_vec();
        let decoded = CreateAlbumResponse::decode(&encoded).unwrap();
        assert_eq!(decoded, response);
        assert_eq!(decoded.album_media_key(), Some("ALBUM1"));

        let add = AddMediaToAlbum {
            media_keys: vec!["MK1".to_string(), "MK2".to_string()],
            album_media_key: "ALBUM1".to_string(),
            field5: Some(AddMediaToAlbumField5 { field1: 2 }),
            device_info: Some(DeviceInfo {
                model: "Pixel 2".to_string(),
                make: "Google".to_string(),
                android_api_version: 28,
            }),
            timestamp: 1_700_000_000,
        };
        assert_eq!(
            AddMediaToAlbum::decode(&add.encode_to_vec()).unwrap(),
            add
        );
    }

    #[test]
    fn commit_upload_response_media_key_path() {
        let response = CommitUploadResponse {
            field1: Some(CommitUploadResponseField1 {
                field1: Some(CommitToken {
                    field1: 2,
                    field2: vec![9, 9],
                }),
                field2: 7,
                field3: Some(CommitUploadResponseField3 {
                    media_key: "MK9".to_string(),
                }),
            }),
            field2: Some(CommitUploadResponseField2 {
                field1: 1,
                field2: 2,
                field4: 4,
                field6: 6,
                field7: 7,
                field11: 11,
            }),
        };
        let decoded =
            CommitUploadResponse::decode(&response.encode_to_vec()).unwrap();
        assert_eq!(decoded, response);
        assert_eq!(decoded.media_key(), Some("MK9"));
    }

    #[test]
    fn decode_skips_unknown_fields() {
        // GetUploadToken with an extra unknown field 99 (varint) appended.
        let mut bytes = hex_bytes("080210021801200338b960");
        bytes.extend_from_slice(&[0x98, 0x06, 0x2a]); // tag (99<<3|0)=792 -> 98 06, value 42
        let msg = GetUploadToken::decode(&bytes).unwrap();
        assert_eq!(msg.file_size_bytes, 12345);
    }
}
