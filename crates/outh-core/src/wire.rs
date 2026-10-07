//! Minimal protobuf wire-format helpers (CONTRACT.md decision 3: hand-rolled,
//! no prost/protobuf crates).
//!
//! Only the wire types gotohp's messages use are supported: varint (0),
//! 64-bit (1), length-delimited (2) and 32-bit (5). Groups (3/4) never occur
//! in the `.proto` contracts and are rejected. Signed values are handled by
//! callers casting through `u64`/`i64`, exactly as protobuf sign-extends
//! int32/int64 on the wire; no message in the contract uses sint (zigzag),
//! fixed32/fixed64 appear only in fields we skip (CommitUploadResponse).

use crate::{Error, Result};

pub const WIRE_VARINT: u32 = 0;
pub const WIRE_64BIT: u32 = 1;
pub const WIRE_LEN: u32 = 2;
pub const WIRE_32BIT: u32 = 5;

/// Appends `value` as a base-128 varint.
pub fn encode_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Appends the tag for `field` with the given wire type.
pub fn encode_tag(out: &mut Vec<u8>, field: u32, wire_type: u32) {
    encode_varint(out, ((field as u64) << 3) | (wire_type as u64));
}

/// Appends a varint field unconditionally. Proto3 zero-omission is the
/// caller's job (protocol.rs guards scalar fields with `!= 0` checks,
/// matching Go's generated marshaller).
pub fn put_u64(out: &mut Vec<u8>, field: u32, value: u64) {
    encode_tag(out, field, WIRE_VARINT);
    encode_varint(out, value);
}

/// Appends a signed varint field. Negative values sign-extend to 64 bits and
/// therefore take 10 bytes, exactly as in Go's protobuf encoding.
pub fn put_i64(out: &mut Vec<u8>, field: u32, value: i64) {
    put_u64(out, field, value as u64);
}

pub fn put_i32(out: &mut Vec<u8>, field: u32, value: i32) {
    put_i64(out, field, value as i64);
}

/// Appends a length-delimited field (bytes / string / packed repeated).
pub fn put_bytes(out: &mut Vec<u8>, field: u32, value: &[u8]) {
    encode_tag(out, field, WIRE_LEN);
    encode_varint(out, value.len() as u64);
    out.extend_from_slice(value);
}

pub fn put_string(out: &mut Vec<u8>, field: u32, value: &str) {
    put_bytes(out, field, value.as_bytes());
}

/// Appends an embedded message field; `encoded` is the already-encoded
/// sub-message. An empty slice still writes tag + zero length, preserving
/// message presence (e.g. HashCheck field 2, CreateAlbum field 6).
pub fn put_msg(out: &mut Vec<u8>, field: u32, encoded: &[u8]) {
    put_bytes(out, field, encoded);
}

/// Sequential cursor over an encoded message.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// Reads one varint. Rejects encodings longer than 10 bytes and values
    /// overflowing u64, like Go's `protowire.ConsumeVarint`.
    pub fn read_varint(&mut self) -> Result<u64> {
        let mut value: u64 = 0;
        let mut shift: u32 = 0;
        loop {
            if self.pos >= self.buf.len() {
                return Err(Error::Protocol("truncated varint".to_string()));
            }
            let byte = self.buf[self.pos];
            self.pos += 1;
            if shift == 63 {
                // 10th byte: only bit 0 may be set (Go protowire semantics:
                // overflow bytes are an error unless they encode the
                // canonical 10-byte maximum form).
                if byte > 1 {
                    return Err(Error::Protocol("varint overflows u64".to_string()));
                }
                value |= (byte as u64) << shift;
                if byte & 0x80 != 0 {
                    return Err(Error::Protocol("varint too long".to_string()));
                }
                return Ok(value);
            }
            value |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
        }
    }

    /// Reads a field tag. Returns `None` at a clean end of buffer.
    pub fn read_tag(&mut self) -> Result<Option<(u32, u32)>> {
        if self.is_empty() {
            return Ok(None);
        }
        let key = self.read_varint()?;
        let field = (key >> 3) as u32;
        let wire_type = (key & 0x07) as u32;
        if field == 0 {
            return Err(Error::Protocol("invalid field number 0".to_string()));
        }
        Ok(Some((field, wire_type)))
    }

    /// Reads a length-delimited value, returning the raw slice.
    pub fn read_bytes(&mut self) -> Result<&'a [u8]> {
        let len = self.read_varint()? as usize;
        if len > self.remaining() {
            return Err(Error::Protocol("truncated length-delimited field".to_string()));
        }
        let start = self.pos;
        self.pos += len;
        Ok(&self.buf[start..start + len])
    }

    pub fn read_fixed32(&mut self) -> Result<u32> {
        if self.remaining() < 4 {
            return Err(Error::Protocol("truncated fixed32".to_string()));
        }
        let start = self.pos;
        self.pos += 4;
        Ok(u32::from_le_bytes([
            self.buf[start],
            self.buf[start + 1],
            self.buf[start + 2],
            self.buf[start + 3],
        ]))
    }

    pub fn read_fixed64(&mut self) -> Result<u64> {
        if self.remaining() < 8 {
            return Err(Error::Protocol("truncated fixed64".to_string()));
        }
        let start = self.pos;
        self.pos += 8;
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&self.buf[start..start + 8]);
        Ok(u64::from_le_bytes(bytes))
    }

    /// Reads a field value that must be a varint (after [`Reader::read_tag`]).
    pub fn get_u64(&mut self, wire_type: u32) -> Result<u64> {
        if wire_type != WIRE_VARINT {
            return Err(Error::Protocol(format!(
                "expected varint wire type, got {wire_type}"
            )));
        }
        self.read_varint()
    }

    pub fn get_i64(&mut self, wire_type: u32) -> Result<i64> {
        Ok(self.get_u64(wire_type)? as i64)
    }

    pub fn get_i32(&mut self, wire_type: u32) -> Result<i32> {
        Ok(self.get_u64(wire_type)? as i32)
    }

    /// Reads a field value that must be length-delimited.
    pub fn get_bytes(&mut self, wire_type: u32) -> Result<&'a [u8]> {
        if wire_type != WIRE_LEN {
            return Err(Error::Protocol(format!(
                "expected length-delimited wire type, got {wire_type}"
            )));
        }
        self.read_bytes()
    }

    pub fn get_string(&mut self, wire_type: u32) -> Result<String> {
        let bytes = self.get_bytes(wire_type)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|_| Error::Protocol("invalid UTF-8 in string field".to_string()))
    }

    /// Reads an embedded message as a sub-reader over its bytes.
    pub fn get_msg(&mut self, wire_type: u32) -> Result<Reader<'a>> {
        let bytes = self.get_bytes(wire_type)?;
        Ok(Reader::new(bytes))
    }

    /// Consumes and discards one field value of the given wire type
    /// (after [`Reader::read_tag`]). Group wire types are rejected.
    pub fn skip_field(&mut self, wire_type: u32) -> Result<()> {
        match wire_type {
            WIRE_VARINT => {
                self.read_varint()?;
            }
            WIRE_64BIT => {
                self.read_fixed64()?;
            }
            WIRE_LEN => {
                self.read_bytes()?;
            }
            WIRE_32BIT => {
                self.read_fixed32()?;
            }
            other => {
                return Err(Error::Protocol(format!(
                    "unsupported wire type {other}"
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded_varint(value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        encode_varint(&mut out, value);
        out
    }

    #[test]
    fn varint_roundtrip_incl_large_values() {
        let cases: [(u64, &[u8]); 8] = [
            (0, &[0x00]),
            (1, &[0x01]),
            (127, &[0x7f]),
            (128, &[0x80, 0x01]),
            (300, &[0xac, 0x02]),
            (12345, &[0xb9, 0x60]),
            (1 << 63, &[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01]),
            (u64::MAX, &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]),
        ];
        for (value, expected) in cases {
            assert_eq!(encoded_varint(value), expected, "encode {value}");
            let mut reader = Reader::new(expected);
            assert_eq!(reader.read_varint().unwrap(), value, "decode {value}");
            assert!(reader.is_empty());
        }
    }

    #[test]
    fn negative_i64_encodes_as_ten_byte_varint() {
        let mut out = Vec::new();
        put_i64(&mut out, 1, -1);
        // tag 0x08, then the 10-byte sign-extended varint of u64::MAX.
        assert_eq!(
            out,
            vec![0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]
        );
        let mut reader = Reader::new(&out);
        let (field, wt) = reader.read_tag().unwrap().unwrap();
        assert_eq!((field, wt), (1, WIRE_VARINT));
        assert_eq!(reader.get_i64(wt).unwrap(), -1);
    }

    #[test]
    fn nested_message_bytes_match_hand_computed_hex() {
        // inner message: field 1 (varint) = 150  ->  08 96 01
        let mut inner = Vec::new();
        put_u64(&mut inner, 1, 150);
        assert_eq!(inner, vec![0x08, 0x96, 0x01]);

        // outer message: field 1 (message) = inner, field 2 (string) = "hi"
        // -> 0a 03 08 96 01 | 12 02 68 69
        let mut outer = Vec::new();
        put_msg(&mut outer, 1, &inner);
        put_string(&mut outer, 2, "hi");
        assert_eq!(
            outer,
            vec![0x0a, 0x03, 0x08, 0x96, 0x01, 0x12, 0x02, 0x68, 0x69]
        );

        // Read it back through the cursor API.
        let mut reader = Reader::new(&outer);
        let (field, wt) = reader.read_tag().unwrap().unwrap();
        assert_eq!((field, wt), (1, WIRE_LEN));
        let mut sub = reader.get_msg(wt).unwrap();
        let (sub_field, sub_wt) = sub.read_tag().unwrap().unwrap();
        assert_eq!((sub_field, sub_wt), (1, WIRE_VARINT));
        assert_eq!(sub.get_u64(sub_wt).unwrap(), 150);
        let (field, wt) = reader.read_tag().unwrap().unwrap();
        assert_eq!((field, wt), (2, WIRE_LEN));
        assert_eq!(reader.get_string(wt).unwrap(), "hi");
        assert!(reader.read_tag().unwrap().is_none());
    }

    #[test]
    fn empty_message_field_keeps_presence() {
        let mut out = Vec::new();
        put_msg(&mut out, 2, &[]);
        assert_eq!(out, vec![0x12, 0x00]);
    }

    #[test]
    fn skip_field_handles_all_supported_wire_types() {
        let mut out = Vec::new();
        put_u64(&mut out, 1, 7); // varint
        out.extend_from_slice(&[0x19]); // field 3, wire type 1 (64-bit)
        out.extend_from_slice(&[0x2a; 8]);
        put_bytes(&mut out, 4, &[1, 2, 3]); // length-delimited
        out.extend_from_slice(&[0x2d]); // field 5, wire type 5 (32-bit)
        out.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]);

        let mut reader = Reader::new(&out);
        let mut seen = Vec::new();
        while let Some((field, wt)) = reader.read_tag().unwrap() {
            if field == 1 {
                assert_eq!(reader.get_u64(wt).unwrap(), 7);
            } else {
                reader.skip_field(wt).unwrap();
            }
            seen.push(field);
        }
        assert_eq!(seen, vec![1, 3, 4, 5]);
        assert!(reader.is_empty());
    }

    #[test]
    fn malformed_inputs_are_errors_not_panics() {
        // Truncated varint.
        assert!(Reader::new(&[0x80]).read_varint().is_err());
        // Field number 0.
        assert!(Reader::new(&[0x00]).read_tag().is_err());
        // Length past end of buffer.
        let mut reader = Reader::new(&[0x0a, 0x05, 0x01]);
        let (_, wt) = reader.read_tag().unwrap().unwrap();
        assert!(reader.get_bytes(wt).is_err());
        // Group wire type cannot be skipped.
        let mut reader = Reader::new(&[0x0b]);
        let (_, wt) = reader.read_tag().unwrap().unwrap();
        assert!(reader.skip_field(wt).is_err());
        // Overlong varint.
        assert!(Reader::new(&[0xff; 11]).read_varint().is_err());
    }
}
