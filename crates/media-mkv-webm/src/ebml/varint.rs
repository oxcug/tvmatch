//! EBML variable-length integers (VINT) per RFC 8794 §4.
//!
//! A VINT is 1..=8 bytes. The first byte's leading zero bits give the
//! total width minus one (the first 1-bit is the "width marker"). The
//! payload bits are the remaining bits after the marker.
//!
//! Two flavors are read off the wire:
//!
//! - **IDs** (`read_vint_id`): the width marker is *kept* as part of
//!   the value, so 1-byte `0x83` and 2-byte `0x4083` are distinct IDs
//!   even though they encode the same numeric payload (3).
//! - **Sizes** (`read_vint_size`): the marker is stripped. The all-ones
//!   payload (e.g. `0xFF`, `0x7FFF`, …) is the reserved "unknown
//!   length" value used by Segment and Cluster.

use crate::{Error, Result};

/// A decoded VINT with its on-wire width (1..=8 bytes).
#[derive(Debug, Clone, Copy)]
pub struct Vint {
    pub value: u64,
    pub width: u8,
}

/// Read a VINT and return both the marker-stripped value and the
/// indicator of whether the payload was all-ones (unknown size).
///
/// Returns `(value, width, is_unknown)`. `width` is 1..=8. `is_unknown`
/// is true iff every payload bit was 1 — callers reading element
/// *sizes* treat that as "unknown length", callers reading IDs ignore
/// it.
pub fn read_vint_raw(src: &[u8], pos: &mut usize) -> Result<(u64, u8, bool)> {
    let first = *src.get(*pos).ok_or(Error::UnexpectedEof("VINT"))?;
    if first == 0x00 {
        return Err(Error::InvalidVint);
    }

    // Width = position of first 1-bit, counted from MSB (1..=8).
    let width = first.leading_zeros() as u8 + 1;
    debug_assert!((1..=8).contains(&width));

    if *pos + width as usize > src.len() {
        return Err(Error::UnexpectedEof("VINT"));
    }

    // Strip the width marker from the first byte.
    let mask = !(1u8 << (8 - width));
    let mut value = (first & mask) as u64;

    for i in 1..width as usize {
        value = (value << 8) | src[*pos + i] as u64;
    }

    // Detect "unknown length" — payload is all-ones across `width * 7`
    // bits (width-1 bytes' worth plus the masked first byte).
    let payload_bits = (width as u32) * 7;
    let all_ones = if payload_bits >= 64 {
        value == u64::MAX
    } else {
        value == (1u64 << payload_bits) - 1
    };

    *pos += width as usize;
    Ok((value, width, all_ones))
}

/// Read an element ID. Returns the VINT with the width marker kept,
/// since IDs are distinguished by their on-wire encoding (the spec
/// calls this VINT representation, not VINT_DATA).
pub fn read_vint_id(src: &[u8], pos: &mut usize) -> Result<Vint> {
    let start = *pos;
    let (_value, width, _) = read_vint_raw(src, pos)?;

    // Re-read the bytes as an unmasked big-endian integer so the
    // width marker is preserved. IDs are short (1..=4 bytes in
    // matroska), so the unsigned value fits in u32 — but we keep u64
    // for generality.
    let mut id = 0u64;
    for i in 0..width as usize {
        id = (id << 8) | src[start + i] as u64;
    }
    Ok(Vint { value: id, width })
}

/// Read an element size. Returns `None` for the reserved "unknown
/// length" encoding (every payload bit set) — only Segment and Cluster
/// use it in practice.
pub fn read_vint_size(src: &[u8], pos: &mut usize) -> Result<Option<u64>> {
    let (value, _width, unknown) = read_vint_raw(src, pos)?;
    Ok(if unknown { None } else { Some(value) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc8794_examples_size() {
        // Single byte size: 0x82 → width 1, payload 0x02 = 2.
        let mut p = 0;
        assert_eq!(read_vint_size(&[0x82], &mut p).unwrap(), Some(2));
        assert_eq!(p, 1);

        // Two byte size: 0x40 0x02 → width 2, payload = 2.
        let mut p = 0;
        assert_eq!(read_vint_size(&[0x40, 0x02], &mut p).unwrap(), Some(2));
        assert_eq!(p, 2);

        // Unknown size: 0xFF → width 1, payload all-ones.
        let mut p = 0;
        assert_eq!(read_vint_size(&[0xFF], &mut p).unwrap(), None);
    }

    #[test]
    fn ids_preserve_width_marker() {
        // EBML element ID 0x1A45DFA3 — 4-byte VINT, marker = top bit
        // of first byte (0x10 sets the 4-byte width).
        let bytes = [0x1A, 0x45, 0xDF, 0xA3];
        let mut p = 0;
        let v = read_vint_id(&bytes, &mut p).unwrap();
        assert_eq!(v.value, 0x1A45DFA3);
        assert_eq!(v.width, 4);
        assert_eq!(p, 4);
    }

    #[test]
    fn invalid_vint_zero_byte() {
        let mut p = 0;
        assert!(matches!(
            read_vint_size(&[0x00, 0x01], &mut p),
            Err(Error::InvalidVint)
        ));
    }

    #[test]
    fn eof_partway() {
        let mut p = 0;
        assert!(matches!(
            read_vint_size(&[0x40], &mut p),
            Err(Error::UnexpectedEof(_))
        ));
    }
}
