//! EBML element writers — inverse of [`element`](super::element).
//!
//! Each `write_*` helper appends a complete element (id, size VINT and
//! payload) to a caller-provided `Vec<u8>`. Masters take a fully-
//! built body slice; the muxer composes nested masters by building
//! the inner-most layer first and bubbling outward.
//!
//! Streaming mux uses the unknown-size
//! Segment encoding; for that path use [`open_master_unknown_size`]
//! which writes only the id + reserved-VINT prefix and lets the
//! caller emit child elements directly.

/// Smallest VINT width (1..=8 bytes) that can encode `n` as a *size*
/// VINT — the reserved all-ones payload is left for `unknown size`,
/// so we pick the next width up when `n` hits that limit.
pub fn write_size_vint(n: u64, out: &mut Vec<u8>) {
    let w = size_vint_width(n);
    let mut buf = [0u8; 8];
    let start = 8 - w;
    let mut x = n;
    for i in (start..8).rev() {
        buf[i] = (x & 0xFF) as u8;
        x >>= 8;
    }
    buf[start] |= 1 << (8 - w as u32);
    out.extend_from_slice(&buf[start..]);
}

/// Number of bytes [`write_size_vint`] would emit for `n`.
pub fn size_vint_width(n: u64) -> usize {
    for w in 1..=8u8 {
        let bits = 7 * w as u32;
        let limit = if bits == 64 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        };
        if n < limit {
            return w as usize;
        }
    }
    panic!("EBML size {n} too large for any VINT width");
}

/// 1-byte all-ones VINT, the spec's "unknown size" marker. Used for
/// streaming Segment / Cluster encoding.
pub fn write_unknown_size_vint(out: &mut Vec<u8>) {
    out.push(0xFF);
}

/// Emit an element ID. IDs are stored on the wire with their width
/// marker, so the bytes come directly from the integer in big-endian
/// truncated to the natural width.
fn write_id(id: u64, out: &mut Vec<u8>) {
    let width = id_width(id);
    let be = id.to_be_bytes();
    out.extend_from_slice(&be[8 - width..]);
}

fn id_width(id: u64) -> usize {
    match id {
        0..=0xFF => 1,
        0x100..=0xFFFF => 2,
        0x1_0000..=0xFF_FFFF => 3,
        0x100_0000..=0xFFFF_FFFF => 4,
        _ => panic!("EBML ID 0x{id:X} exceeds 4-byte width"),
    }
}

/// Append `id + size_vint + payload`.
pub fn write_element(id: u64, payload: &[u8], out: &mut Vec<u8>) {
    write_id(id, out);
    write_size_vint(payload.len() as u64, out);
    out.extend_from_slice(payload);
}

/// Big-endian unsigned encoded to the smallest 1..=8 byte width.
fn uint_be(n: u64) -> Vec<u8> {
    if n == 0 {
        return vec![0];
    }
    let mut v = Vec::new();
    let mut started = false;
    for shift in (0..8).rev() {
        let b = ((n >> (shift * 8)) & 0xFF) as u8;
        if started || b != 0 {
            v.push(b);
            started = true;
        }
    }
    v
}

pub fn write_uint(id: u64, value: u64, out: &mut Vec<u8>) {
    write_element(id, &uint_be(value), out);
}

/// Emit `value` as a uint with an exact byte width (1..=8). Used by
/// the muxer to keep SeekHead entries a predictable size so segment
/// offsets can be computed before the body is written.
pub fn write_uint_fixed_width(id: u64, value: u64, width: usize, out: &mut Vec<u8>) {
    assert!(
        (1..=8).contains(&width),
        "fixed-width uint must be 1..=8 bytes"
    );
    let be = value.to_be_bytes();
    write_element(id, &be[8 - width..], out);
}

/// Number of bytes `write_id` would emit for `id`.
pub fn id_width_of(id: u64) -> usize {
    id_width(id)
}

/// Total bytes [`write_element`] would emit for an element with the
/// given id and payload length.
pub fn element_size(id: u64, payload_len: usize) -> usize {
    id_width(id) + size_vint_width(payload_len as u64) + payload_len
}

pub fn write_int(id: u64, value: i64, out: &mut Vec<u8>) {
    if value == 0 {
        write_element(id, &[0], out);
        return;
    }
    let bytes = value.to_be_bytes();
    // Drop leading sign-extension bytes (0x00 if positive, 0xFF if
    // negative) but always keep at least one byte that preserves the
    // sign bit.
    let sign_byte = if value < 0 { 0xFF } else { 0x00 };
    let mut start = 0;
    while start < 7 && bytes[start] == sign_byte && (bytes[start + 1] & 0x80) == (sign_byte & 0x80)
    {
        start += 1;
    }
    write_element(id, &bytes[start..], out);
}

pub fn write_float64(id: u64, value: f64, out: &mut Vec<u8>) {
    write_element(id, &value.to_be_bytes(), out);
}

pub fn write_float32(id: u64, value: f32, out: &mut Vec<u8>) {
    write_element(id, &value.to_be_bytes(), out);
}

pub fn write_ascii(id: u64, s: &str, out: &mut Vec<u8>) {
    debug_assert!(s.is_ascii(), "ASCII writer received non-ASCII payload");
    write_element(id, s.as_bytes(), out);
}

pub fn write_utf8(id: u64, s: &str, out: &mut Vec<u8>) {
    write_element(id, s.as_bytes(), out);
}

pub fn write_binary(id: u64, bytes: &[u8], out: &mut Vec<u8>) {
    write_element(id, bytes, out);
}

pub fn write_master(id: u64, body: &[u8], out: &mut Vec<u8>) {
    write_element(id, body, out);
}

/// Write `id + unknown-size VINT` and hand back to the caller —
/// children must follow directly in `out`. Used only for streaming
/// Segment / Cluster emission.
pub fn open_master_unknown_size(id: u64, out: &mut Vec<u8>) {
    write_id(id, out);
    write_unknown_size_vint(out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_vint_widths() {
        let mut v = vec![];
        write_size_vint(2, &mut v);
        assert_eq!(v, vec![0x82]);

        v.clear();
        write_size_vint(127, &mut v);
        // 127 is the limit for width 1 (reserved for "unknown"); spill to width 2.
        assert_eq!(v, vec![0x40, 0x7F]);

        v.clear();
        write_size_vint(128, &mut v);
        assert_eq!(v, vec![0x40, 0x80]);
    }

    #[test]
    fn uint_round_trip() {
        let mut v = vec![];
        write_uint(0x4286, 4, &mut v); // EBMLVersion
        // 0x42 0x86 | 0x81 | 0x04
        assert_eq!(v, vec![0x42, 0x86, 0x81, 0x04]);
    }

    #[test]
    fn int_drops_sign_extension() {
        let mut v = vec![];
        write_int(0x88, -1, &mut v);
        assert_eq!(v, vec![0x88, 0x81, 0xFF]);

        v.clear();
        write_int(0x88, 1, &mut v);
        assert_eq!(v, vec![0x88, 0x81, 0x01]);

        v.clear();
        write_int(0x88, -129, &mut v);
        // -129 needs 2 bytes: 0xFF 0x7F
        assert_eq!(v, vec![0x88, 0x82, 0xFF, 0x7F]);
    }

    #[test]
    fn round_trip_uint_through_reader() {
        use crate::ebml::element::Reader;
        let mut buf = vec![];
        write_uint(0x4286, 42, &mut buf);
        let mut r = Reader::new(&buf);
        let hdr = r.read_header().unwrap();
        assert_eq!(hdr.id.value, 0x4286);
        assert_eq!(r.read_uint(&hdr).unwrap(), 42);
    }

    #[test]
    fn round_trip_int_negative() {
        use crate::ebml::element::Reader;
        let mut buf = vec![];
        write_int(0x88, -1234, &mut buf);
        let mut r = Reader::new(&buf);
        let hdr = r.read_header().unwrap();
        assert_eq!(r.read_int(&hdr).unwrap(), -1234);
    }

    #[test]
    fn fixed_width_uint_writes_exact_bytes() {
        let mut v = vec![];
        write_uint_fixed_width(0x4286, 1, 8, &mut v);
        // id(2) + size_vint(1, value=8) + 8 bytes payload
        assert_eq!(v.len(), 2 + 1 + 8);
        assert_eq!(&v[..3], &[0x42, 0x86, 0x88]);
        assert_eq!(&v[3..], &[0, 0, 0, 0, 0, 0, 0, 1]);
    }

    #[test]
    fn element_size_matches_emit() {
        let mut v = vec![];
        write_uint(0x4286, 12345, &mut v);
        assert_eq!(v.len(), element_size(0x4286, uint_be(12345).len()));
    }

    #[test]
    fn round_trip_master_contains_uint() {
        use crate::ebml::element::Reader;
        let mut body = vec![];
        write_uint(0x4286, 1, &mut body);
        let mut buf = vec![];
        write_master(0x1A45DFA3, &body, &mut buf);
        let mut r = Reader::new(&buf);
        let hdr = r.read_header().unwrap();
        assert_eq!(hdr.id.value, 0x1A45DFA3);
        let mut inner = r.descend(&hdr).unwrap();
        let child = inner.read_header().unwrap();
        assert_eq!(child.id.value, 0x4286);
        assert_eq!(inner.read_uint(&child).unwrap(), 1);
    }
}
