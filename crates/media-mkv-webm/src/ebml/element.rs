//! EBML element headers and typed payload readers.
//!
//! `Reader` is a thin cursor over a `&[u8]`. Streaming I/O is not in
//! scope for the MVP — callers read or mmap the file first.

use super::varint::{Vint, read_vint_id, read_vint_size};
use crate::{Error, Result};

/// One element header: ID (with width marker) + payload size.
/// `None` size means "unknown length", which the spec only allows for
/// Segment and Cluster. Callers in those cases read children until
/// they hit a sibling-level ID or end of stream.
#[derive(Debug, Clone, Copy)]
pub struct ElementHeader {
    pub id: Vint,
    pub size: Option<u64>,
    /// Byte offset of the element's first byte (ID) within the
    /// Reader's slice. Used to translate child positions back into
    /// the parent's coordinate space (see Demuxer::clusters_offset).
    pub element_start: usize,
    /// Byte offset of the payload start within the Reader's slice.
    pub payload_start: usize,
}

impl ElementHeader {
    /// Returns the (exclusive) end offset of the payload, or `None` if
    /// the size is unknown.
    pub fn payload_end(&self) -> Option<usize> {
        self.size.map(|n| self.payload_start + n as usize)
    }
}

/// Slice cursor that hands out element headers and typed payloads.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Cursor over `buf` starting at `start`. Position reads return
    /// offsets into the underlying buffer, not the suffix — this is
    /// how `Frames` keeps a single coordinate system across the whole
    /// input.
    pub fn new_at(buf: &'a [u8], start: usize) -> Self {
        Self { buf, pos: start }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    pub fn eof(&self) -> bool {
        self.pos >= self.buf.len()
    }

    /// Read the next element header.
    pub fn read_header(&mut self) -> Result<ElementHeader> {
        let element_start = self.pos;
        let id = read_vint_id(self.buf, &mut self.pos)?;
        let size = read_vint_size(self.buf, &mut self.pos)?;
        Ok(ElementHeader {
            id,
            size,
            element_start,
            payload_start: self.pos,
        })
    }

    /// Skip past the current element's payload. For unknown-size
    /// elements this is a no-op — the caller must walk children.
    pub fn skip_payload(&mut self, hdr: &ElementHeader) -> Result<()> {
        let Some(end) = hdr.payload_end() else {
            return Ok(());
        };
        if end > self.buf.len() {
            return Err(Error::SizeOverflow);
        }
        self.pos = end;
        Ok(())
    }

    /// Borrow the raw payload bytes for the given header. Errors if
    /// the size is unknown or runs past the buffer.
    pub fn payload<'b>(&'b self, hdr: &ElementHeader) -> Result<&'b [u8]> {
        let end = hdr.payload_end().ok_or(Error::Malformed(
            "payload requested for unknown-size element",
        ))?;
        self.buf
            .get(hdr.payload_start..end)
            .ok_or(Error::SizeOverflow)
    }

    /// Carve a sub-reader limited to this element's payload. Useful
    /// for walking master elements without bleeding past their end.
    pub fn descend<'b>(&'b self, hdr: &ElementHeader) -> Result<Reader<'b>> {
        let end = hdr
            .payload_end()
            .ok_or(Error::Malformed("descend into unknown-size element"))?;
        let slice = self
            .buf
            .get(hdr.payload_start..end)
            .ok_or(Error::SizeOverflow)?;
        Ok(Reader::new(slice))
    }

    /// Sub-reader covering whatever bytes follow the header, even when
    /// size is unknown. Used for Segment/Cluster which legitimately
    /// declare unknown size — the child walk terminates on a
    /// sibling-level ID.
    pub fn descend_unbounded<'b>(&'b self, hdr: &ElementHeader) -> Reader<'b> {
        let slice = self.buf.get(hdr.payload_start..).unwrap_or(&[]);
        Reader::new(slice)
    }

    /// Big-endian unsigned, 0..=8 bytes. 0-length is defined as 0.
    pub fn read_uint(&self, hdr: &ElementHeader) -> Result<u64> {
        let bytes = self.payload(hdr)?;
        if bytes.len() > 8 {
            return Err(Error::Malformed("uint > 8 bytes"));
        }
        let mut v = 0u64;
        for &b in bytes {
            v = (v << 8) | b as u64;
        }
        Ok(v)
    }

    /// Big-endian signed, sign-extended from the high bit of the
    /// first byte. 0-length is defined as 0.
    pub fn read_int(&self, hdr: &ElementHeader) -> Result<i64> {
        let bytes = self.payload(hdr)?;
        if bytes.len() > 8 {
            return Err(Error::Malformed("int > 8 bytes"));
        }
        if bytes.is_empty() {
            return Ok(0);
        }
        // Sign-extend by shifting into the top of a u64.
        let shift = 64 - 8 * bytes.len();
        let mut v = 0u64;
        for &b in bytes {
            v = (v << 8) | b as u64;
        }
        Ok(((v << shift) as i64) >> shift)
    }

    /// IEEE-754 float, 0/4/8 bytes. 0-length is defined as 0.0.
    /// 80-bit (10-byte) floats from the original EBML spec are not
    /// emitted by Matroska or WebM — rejected here.
    pub fn read_float(&self, hdr: &ElementHeader) -> Result<f64> {
        let bytes = self.payload(hdr)?;
        match bytes.len() {
            0 => Ok(0.0),
            4 => Ok(f32::from_be_bytes(bytes.try_into().unwrap()) as f64),
            8 => Ok(f64::from_be_bytes(bytes.try_into().unwrap())),
            _ => Err(Error::Malformed("float length must be 0/4/8")),
        }
    }

    /// ASCII string with trailing-NUL trimming.
    pub fn read_ascii(&self, hdr: &ElementHeader) -> Result<&str> {
        let bytes = self.payload(hdr)?;
        let trimmed = trim_nul(bytes);
        std::str::from_utf8(trimmed).map_err(|_| Error::Malformed("ascii not UTF-8 safe"))
    }

    /// UTF-8 string with trailing-NUL trimming.
    pub fn read_utf8(&self, hdr: &ElementHeader) -> Result<&str> {
        let bytes = self.payload(hdr)?;
        std::str::from_utf8(trim_nul(bytes)).map_err(|_| Error::Malformed("utf8"))
    }
}

fn trim_nul(bytes: &[u8]) -> &[u8] {
    match bytes.iter().position(|&b| b == 0) {
        Some(end) => &bytes[..end],
        None => bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_simple_uint() {
        // ID 0x42 0x86 (DocTypeVersion) size 1 payload 0x04 = 4.
        let bytes = [0x42, 0x86, 0x81, 0x04];
        let mut r = Reader::new(&bytes);
        let hdr = r.read_header().unwrap();
        assert_eq!(hdr.id.value, 0x4286);
        assert_eq!(hdr.size, Some(1));
        assert_eq!(r.read_uint(&hdr).unwrap(), 4);
    }

    #[test]
    fn read_float_4_and_8() {
        let mut bytes = vec![0x44, 0x89, 0x88]; // Duration (0x4489) size 8
        bytes.extend_from_slice(&1234.5f64.to_be_bytes());
        let mut r = Reader::new(&bytes);
        let hdr = r.read_header().unwrap();
        assert_eq!(r.read_float(&hdr).unwrap(), 1234.5);
    }

    #[test]
    fn read_int_signed_extension() {
        // 2-byte signed: 0xFF 0xFE = -2.
        let bytes = [0x88, 0x82, 0xFF, 0xFE]; // arbitrary 1-byte ID 0x88, size 2
        let mut r = Reader::new(&bytes);
        let hdr = r.read_header().unwrap();
        assert_eq!(r.read_int(&hdr).unwrap(), -2);
    }

    #[test]
    fn descend_bounds() {
        // Outer master (id 0x80 size 4) wraps a child uint (id 0x81 size 1 payload 9)
        // plus 1 byte of trailing junk that descend must NOT expose.
        let bytes = [
            0x80, 0x84, 0x81, 0x81, 0x09, 0xAA, /* sibling */ 0x82, 0x80,
        ];
        let mut r = Reader::new(&bytes);
        let outer = r.read_header().unwrap();
        let mut inner = r.descend(&outer).unwrap();
        let child = inner.read_header().unwrap();
        assert_eq!(inner.read_uint(&child).unwrap(), 9);
        inner.skip_payload(&child).unwrap();
        // The 0xAA byte after the uint is inside the descended slice but
        // outside the child element — descend bounds the master, not
        // the children, so this trailing byte is reachable here.
        assert_eq!(inner.remaining(), 1);

        r.skip_payload(&outer).unwrap();
        let sibling = r.read_header().unwrap();
        assert_eq!(sibling.id.value, 0x82);
    }
}
