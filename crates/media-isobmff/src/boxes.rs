//! Native ISOBMFF box encoders/decoders.
//!
//! The public struct shapes, [`Encode`] / [`Decode`] traits and type names
//! mirror `mp4-atom` 0.10 where supported. The implementation covers primitive,
//! leaf, codec sample-entry and container boxes; registry parsers are test oracles,
//! not production dependencies.

use crate::{IsobmffError, IsobmffResult};

// ---------------------------------------------------------------------
// Primitive types — shape-compatible with `mp4_atom::{FourCC, u24,
// FixedPoint}` so consumer code compiles unchanged.
// ---------------------------------------------------------------------

/// A four-character code used to identify atoms.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FourCC([u8; 4]);

impl FourCC {
    pub const fn new(value: &[u8; 4]) -> Self {
        FourCC(*value)
    }
}

impl AsRef<[u8; 4]> for FourCC {
    fn as_ref(&self) -> &[u8; 4] {
        &self.0
    }
}

impl From<[u8; 4]> for FourCC {
    fn from(v: [u8; 4]) -> Self {
        FourCC(v)
    }
}

impl From<&[u8; 4]> for FourCC {
    fn from(v: &[u8; 4]) -> Self {
        FourCC(*v)
    }
}

impl From<u32> for FourCC {
    fn from(v: u32) -> Self {
        FourCC(v.to_be_bytes())
    }
}

impl From<FourCC> for u32 {
    fn from(c: FourCC) -> Self {
        u32::from_be_bytes(c.0)
    }
}

impl core::fmt::Display for FourCC {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = String::from_utf8_lossy(&self.0);
        write!(f, "{s}")
    }
}

impl core::fmt::Debug for FourCC {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = String::from_utf8_lossy(&self.0);
        write!(f, "{s}")
    }
}

/// 24-bit unsigned integer, used for `FullBox` flags.
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub struct u24([u8; 3]);

impl u24 {
    pub const MAX: u32 = 0x00FF_FFFF;
}

impl From<[u8; 3]> for u24 {
    fn from(v: [u8; 3]) -> Self {
        u24(v)
    }
}

impl From<u24> for u32 {
    fn from(v: u24) -> Self {
        u32::from_be_bytes([0, v.0[0], v.0[1], v.0[2]])
    }
}

impl TryFrom<u32> for u24 {
    type Error = ();
    fn try_from(v: u32) -> Result<Self, ()> {
        if v > Self::MAX {
            return Err(());
        }
        let b = v.to_be_bytes();
        Ok(u24([b[1], b[2], b[3]]))
    }
}

/// Big-endian fixed-point integer/decimal pair. Common ISOBMFF flavors
/// are `FixedPoint<i16>` (8.8 — `tkhd.volume`), `FixedPoint<u16>`
/// (8.8 — `mvhd.rate`), and `FixedPoint<u32>` (16.16 — `tkhd.matrix`,
/// `mvhd.rate`).
#[derive(Copy, Clone, Default, PartialEq, Eq)]
pub struct FixedPoint<T> {
    int: T,
    dec: T,
}

impl<T: Copy> FixedPoint<T> {
    pub const fn new(int: T, dec: T) -> Self {
        Self { int, dec }
    }
    pub fn integer(&self) -> T {
        self.int
    }
    pub fn decimal(&self) -> T {
        self.dec
    }
}

impl<T: core::fmt::Debug> core::fmt::Debug for FixedPoint<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FixedPoint")
            .field("int", &self.int)
            .field("dec", &self.dec)
            .finish()
    }
}

// ---------------------------------------------------------------------
// Encode / Decode traits — mirror mp4-atom's surface so call sites
// like `ftyp.encode(&mut buf)?` keep working.
// ---------------------------------------------------------------------

pub trait Encode {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()>;
}

pub trait Decode: Sized {
    /// Decode self from the head of `buf`, advancing the read cursor
    /// past the consumed bytes. Implementations advance via the
    /// returned `(value, rest)` pair: callers reassign their slice to
    /// `rest`.
    fn decode(buf: &mut &[u8]) -> IsobmffResult<Self>;
}

/// Box trait. Anything that implements [`Atom`] also implements
/// [`Encode`] (writes `[size:u32, type:[u8;4], body]`).
pub trait Atom: Sized {
    const KIND: FourCC;
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()>;
    fn decode_body(buf: &[u8]) -> IsobmffResult<Self>;
}

impl<T: Atom> Encode for T {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        let start = buf.len();
        buf.extend_from_slice(&[0, 0, 0, 0]); // size placeholder
        buf.extend_from_slice(T::KIND.as_ref());
        T::encode_body(self, buf)?;
        let size: u32 = (buf.len() - start)
            .try_into()
            .map_err(|_| IsobmffError::Malformed("box body exceeds u32".into()))?;
        buf[start..start + 4].copy_from_slice(&size.to_be_bytes());
        Ok(())
    }
}

/// FullBox: 8-byte box header + 1-byte version + 3-byte flags.
/// Most ISOBMFF metadata boxes are FullBoxes; pure data containers
/// (`mdat`, `ftyp`, `moof`, `traf`) are not.
pub trait FullBox: Sized {
    const KIND: FourCC;
    fn version(&self) -> u8 {
        0
    }
    fn flags(&self) -> u32 {
        0
    }
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()>;
    fn decode_full_body(version: u8, flags: u32, buf: &[u8]) -> IsobmffResult<Self>;
}

/// Blanket Encode for FullBox: emits the size/type prefix plus the
/// version + 24-bit flags header.
pub fn encode_full<F: FullBox>(b: &F, buf: &mut Vec<u8>) -> IsobmffResult<()> {
    let start = buf.len();
    buf.extend_from_slice(&[0, 0, 0, 0]);
    buf.extend_from_slice(F::KIND.as_ref());
    buf.push(b.version());
    let flags = b.flags();
    if flags > u24::MAX {
        return Err(IsobmffError::Malformed(format!(
            "FullBox flags 0x{flags:06X} exceed 24-bit max"
        )));
    }
    buf.extend_from_slice(&flags.to_be_bytes()[1..]);
    b.encode_full_body(buf)?;
    let size: u32 = (buf.len() - start)
        .try_into()
        .map_err(|_| IsobmffError::Malformed("FullBox body exceeds u32".into()))?;
    buf[start..start + 4].copy_from_slice(&size.to_be_bytes());
    Ok(())
}

// ---------------------------------------------------------------------
// Primitive Decode impls for use in box bodies.
// ---------------------------------------------------------------------

#[inline]
fn split_at<'a>(buf: &mut &'a [u8], n: usize) -> IsobmffResult<&'a [u8]> {
    if buf.len() < n {
        return Err(IsobmffError::Malformed(format!(
            "short read: need {n} bytes, have {}",
            buf.len()
        )));
    }
    let (head, tail) = buf.split_at(n);
    *buf = tail;
    Ok(head)
}

impl Decode for u8 {
    fn decode(buf: &mut &[u8]) -> IsobmffResult<Self> {
        Ok(split_at(buf, 1)?[0])
    }
}
impl Decode for u16 {
    fn decode(buf: &mut &[u8]) -> IsobmffResult<Self> {
        let s = split_at(buf, 2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
}
impl Decode for u32 {
    fn decode(buf: &mut &[u8]) -> IsobmffResult<Self> {
        let s = split_at(buf, 4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
}
impl Decode for u64 {
    fn decode(buf: &mut &[u8]) -> IsobmffResult<Self> {
        let s = split_at(buf, 8)?;
        Ok(u64::from_be_bytes([
            s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
        ]))
    }
}
impl<const N: usize> Decode for [u8; N] {
    fn decode(buf: &mut &[u8]) -> IsobmffResult<Self> {
        let s = split_at(buf, N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }
}
impl Decode for FourCC {
    fn decode(buf: &mut &[u8]) -> IsobmffResult<Self> {
        Ok(FourCC(<[u8; 4]>::decode(buf)?))
    }
}

// ---------------------------------------------------------------------
// Leaf boxes.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ftyp {
    pub major_brand: FourCC,
    pub minor_version: u32,
    pub compatible_brands: Vec<FourCC>,
}

impl Atom for Ftyp {
    const KIND: FourCC = FourCC::new(b"ftyp");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(self.major_brand.as_ref());
        buf.extend_from_slice(&self.minor_version.to_be_bytes());
        for b in &self.compatible_brands {
            buf.extend_from_slice(b.as_ref());
        }
        Ok(())
    }
    fn decode_body(mut buf: &[u8]) -> IsobmffResult<Self> {
        let buf = &mut buf;
        let major_brand = FourCC::decode(buf)?;
        let minor_version = u32::decode(buf)?;
        let mut compatible_brands = Vec::new();
        while !buf.is_empty() {
            compatible_brands.push(FourCC::decode(buf)?);
        }
        Ok(Ftyp {
            major_brand,
            minor_version,
            compatible_brands,
        })
    }
}

/// Segment Type. Same wire shape as Ftyp; different 4cc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Styp {
    pub major_brand: FourCC,
    pub minor_version: u32,
    pub compatible_brands: Vec<FourCC>,
}

impl Atom for Styp {
    const KIND: FourCC = FourCC::new(b"styp");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(self.major_brand.as_ref());
        buf.extend_from_slice(&self.minor_version.to_be_bytes());
        for b in &self.compatible_brands {
            buf.extend_from_slice(b.as_ref());
        }
        Ok(())
    }
    fn decode_body(mut buf: &[u8]) -> IsobmffResult<Self> {
        let buf = &mut buf;
        let major_brand = FourCC::decode(buf)?;
        let minor_version = u32::decode(buf)?;
        let mut compatible_brands = Vec::new();
        while !buf.is_empty() {
            compatible_brands.push(FourCC::decode(buf)?);
        }
        Ok(Styp {
            major_brand,
            minor_version,
            compatible_brands,
        })
    }
}

/// Media Data Box. Body is the raw sample bytes; no further structure.
#[derive(Debug, Clone, PartialEq)]
pub struct Mdat {
    pub data: Vec<u8>,
}

impl Atom for Mdat {
    const KIND: FourCC = FourCC::new(b"mdat");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(&self.data);
        Ok(())
    }
    fn decode_body(buf: &[u8]) -> IsobmffResult<Self> {
        Ok(Mdat { data: buf.to_vec() })
    }
}

/// Movie Fragment Header. FullBox v0; body is just `sequence_number`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mfhd {
    pub sequence_number: u32,
}

impl Default for Mfhd {
    fn default() -> Self {
        Mfhd { sequence_number: 1 }
    }
}

impl FullBox for Mfhd {
    const KIND: FourCC = FourCC::new(b"mfhd");
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(&self.sequence_number.to_be_bytes());
        Ok(())
    }
    fn decode_full_body(_version: u8, _flags: u32, mut buf: &[u8]) -> IsobmffResult<Self> {
        let buf = &mut buf;
        Ok(Mfhd {
            sequence_number: u32::decode(buf)?,
        })
    }
}

impl Encode for Mfhd {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

// ---------------------------------------------------------------------
// Track-Fragment Header (Tfhd). FullBox v0. Flag bits per ISO/IEC
// 14496-12 §8.8.7:
//   0x000001 base_data_offset_present
//   0x000002 sample_description_index_present
//   0x000008 default_sample_duration_present
//   0x000010 default_sample_size_present
//   0x000020 default_sample_flags_present
//   0x010000 duration_is_empty
//   0x020000 default_base_is_moof  ← CMAF / HLS requirement
//
// The native writer ALWAYS sets `default_base_is_moof`, which
// absorbs `patches::patch_tfhd_default_base_is_moof` — consumers
// no longer need to post-patch the encoded bytes.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tfhd {
    pub track_id: u32,
    pub base_data_offset: Option<u64>,
    pub sample_description_index: Option<u32>,
    pub default_sample_duration: Option<u32>,
    pub default_sample_size: Option<u32>,
    pub default_sample_flags: Option<u32>,
}

impl FullBox for Tfhd {
    const KIND: FourCC = FourCC::new(b"tfhd");
    fn flags(&self) -> u32 {
        let mut f: u32 = 0;
        if self.base_data_offset.is_some() {
            f |= 0x000001;
        }
        if self.sample_description_index.is_some() {
            f |= 0x000002;
        }
        if self.default_sample_duration.is_some() {
            f |= 0x000008;
        }
        if self.default_sample_size.is_some() {
            f |= 0x000010;
        }
        if self.default_sample_flags.is_some() {
            f |= 0x000020;
        }
        // Always set default_base_is_moof for CMAF / HLS shape.
        f | 0x020000
    }
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(&self.track_id.to_be_bytes());
        if let Some(v) = self.base_data_offset {
            buf.extend_from_slice(&v.to_be_bytes());
        }
        if let Some(v) = self.sample_description_index {
            buf.extend_from_slice(&v.to_be_bytes());
        }
        if let Some(v) = self.default_sample_duration {
            buf.extend_from_slice(&v.to_be_bytes());
        }
        if let Some(v) = self.default_sample_size {
            buf.extend_from_slice(&v.to_be_bytes());
        }
        if let Some(v) = self.default_sample_flags {
            buf.extend_from_slice(&v.to_be_bytes());
        }
        Ok(())
    }
    fn decode_full_body(_version: u8, flags: u32, mut buf: &[u8]) -> IsobmffResult<Self> {
        let buf = &mut buf;
        let track_id = u32::decode(buf)?;
        let base_data_offset = if flags & 0x000001 != 0 {
            Some(u64::decode(buf)?)
        } else {
            None
        };
        let sample_description_index = if flags & 0x000002 != 0 {
            Some(u32::decode(buf)?)
        } else {
            None
        };
        let default_sample_duration = if flags & 0x000008 != 0 {
            Some(u32::decode(buf)?)
        } else {
            None
        };
        let default_sample_size = if flags & 0x000010 != 0 {
            Some(u32::decode(buf)?)
        } else {
            None
        };
        let default_sample_flags = if flags & 0x000020 != 0 {
            Some(u32::decode(buf)?)
        } else {
            None
        };
        Ok(Tfhd {
            track_id,
            base_data_offset,
            sample_description_index,
            default_sample_duration,
            default_sample_size,
            default_sample_flags,
        })
    }
}

impl Encode for Tfhd {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

// ---------------------------------------------------------------------
// Track Fragment Decode Time (Tfdt). FullBox v1; always writes 64-bit
// base_media_decode_time to match mp4-atom's encode-time choice.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tfdt {
    pub base_media_decode_time: u64,
}

impl FullBox for Tfdt {
    const KIND: FourCC = FourCC::new(b"tfdt");
    fn version(&self) -> u8 {
        1
    }
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(&self.base_media_decode_time.to_be_bytes());
        Ok(())
    }
    fn decode_full_body(version: u8, _flags: u32, mut buf: &[u8]) -> IsobmffResult<Self> {
        let buf = &mut buf;
        let base_media_decode_time = if version == 1 {
            u64::decode(buf)?
        } else {
            u32::decode(buf)? as u64
        };
        Ok(Tfdt {
            base_media_decode_time,
        })
    }
}

impl Encode for Tfdt {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

// ---------------------------------------------------------------------
// Track Run (Trun) + TrunEntry. FullBox v1. Flag bits per §8.8.8:
//   0x000001 data_offset_present
//   0x000004 first_sample_flags_present (we don't use)
//   0x000100 sample_duration_present
//   0x000200 sample_size_present
//   0x000400 sample_flags_present
//   0x000800 sample_cts_present
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrunEntry {
    pub duration: Option<u32>,
    pub size: Option<u32>,
    pub flags: Option<u32>,
    pub cts: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Trun {
    pub data_offset: Option<i32>,
    pub entries: Vec<TrunEntry>,
}

impl Trun {
    fn entry_flags(&self) -> (bool, bool, bool, bool) {
        // All-or-nothing per spec: a flag is set only when every
        // entry carries that field. Matches mp4-atom's encode choice.
        let dur = self.entries.iter().all(|e| e.duration.is_some());
        let sz = self.entries.iter().all(|e| e.size.is_some());
        let fl = self.entries.iter().all(|e| e.flags.is_some());
        let ct = self.entries.iter().all(|e| e.cts.is_some());
        (dur, sz, fl, ct)
    }
}

impl FullBox for Trun {
    const KIND: FourCC = FourCC::new(b"trun");
    /// v1 only when any sample carries a *negative* `cts` offset
    /// (B-frame reorder where the spec's `dts ≤ pts` invariant is
    /// violated, requiring signed composition offsets). v0 — the
    /// version every shipping MSE implementation has seen since 2010
    /// and what `ffmpeg -movflags +frag_keyframe` writes by default —
    /// otherwise. Holds for the MKV pass-through preroll path (every
    /// cts ≥ 0 by construction) and for MP4 pass-through / transcode
    /// (source / encoder DTS already satisfies the invariant).
    fn version(&self) -> u8 {
        let any_negative = self
            .entries
            .iter()
            .any(|e| e.cts.map(|c| c < 0).unwrap_or(false));
        if any_negative { 1 } else { 0 }
    }
    fn flags(&self) -> u32 {
        let (dur, sz, fl, ct) = self.entry_flags();
        let mut f: u32 = 0;
        if self.data_offset.is_some() {
            f |= 0x000001;
        }
        if dur {
            f |= 0x000100;
        }
        if sz {
            f |= 0x000200;
        }
        if fl {
            f |= 0x000400;
        }
        if ct {
            f |= 0x000800;
        }
        f
    }
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        let (dur, sz, fl, ct) = self.entry_flags();
        buf.extend_from_slice(&(self.entries.len() as u32).to_be_bytes());
        if let Some(off) = self.data_offset {
            buf.extend_from_slice(&off.to_be_bytes());
        }
        // We never emit first_sample_flags.
        for e in &self.entries {
            if dur {
                buf.extend_from_slice(&e.duration.unwrap_or(0).to_be_bytes());
            }
            if sz {
                buf.extend_from_slice(&e.size.unwrap_or(0).to_be_bytes());
            }
            if fl {
                buf.extend_from_slice(&e.flags.unwrap_or(0).to_be_bytes());
            }
            if ct {
                buf.extend_from_slice(&e.cts.unwrap_or(0).to_be_bytes());
            }
        }
        Ok(())
    }
    fn decode_full_body(_version: u8, flags: u32, mut buf: &[u8]) -> IsobmffResult<Self> {
        let buf = &mut buf;
        let sample_count = u32::decode(buf)?;
        let data_offset = if flags & 0x000001 != 0 {
            Some(i32::from_be_bytes(<[u8; 4]>::decode(buf)?))
        } else {
            None
        };
        let mut first_sample_flags = if flags & 0x000004 != 0 {
            Some(u32::decode(buf)?)
        } else {
            None
        };
        let mut entries = Vec::with_capacity(sample_count.min(4096) as usize);
        for _ in 0..sample_count {
            let duration = if flags & 0x000100 != 0 {
                Some(u32::decode(buf)?)
            } else {
                None
            };
            let size = if flags & 0x000200 != 0 {
                Some(u32::decode(buf)?)
            } else {
                None
            };
            let sample_flags = if let Some(fsf) = first_sample_flags.take() {
                Some(fsf)
            } else if flags & 0x000400 != 0 {
                Some(u32::decode(buf)?)
            } else {
                None
            };
            let cts = if flags & 0x000800 != 0 {
                Some(i32::from_be_bytes(<[u8; 4]>::decode(buf)?))
            } else {
                None
            };
            entries.push(TrunEntry {
                duration,
                size,
                flags: sample_flags,
                cts,
            });
        }
        Ok(Trun {
            data_offset,
            entries,
        })
    }
}

impl Encode for Trun {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

// ---------------------------------------------------------------------
// Containers: Traf + Moof. Plain (non-FullBox) child concatenation.
// ---------------------------------------------------------------------

/// Track Fragment box. For the audio fMP4 producer we only ever need
/// tfhd + tfdt + trun; the spec also allows sbgp/sgpd/subs/saiz/saio/
/// meta/senc/udta children, omitted here until a consumer needs them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Traf {
    pub tfhd: Tfhd,
    pub tfdt: Option<Tfdt>,
    pub trun: Vec<Trun>,
}

impl Atom for Traf {
    const KIND: FourCC = FourCC::new(b"traf");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        self.tfhd.encode(buf)?;
        if let Some(t) = &self.tfdt {
            t.encode(buf)?;
        }
        for trun in &self.trun {
            trun.encode(buf)?;
        }
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Traf::decode_body — not yet implemented (production path is encode-only)".into(),
        ))
    }
}

/// Movie Fragment box: mfhd + traf*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moof {
    pub mfhd: Mfhd,
    pub traf: Vec<Traf>,
}

impl Atom for Moof {
    const KIND: FourCC = FourCC::new(b"moof");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        self.mfhd.encode(buf)?;
        for traf in &self.traf {
            traf.encode(buf)?;
        }
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Moof::decode_body — not yet implemented (production path is encode-only)".into(),
        ))
    }
}

// ---------------------------------------------------------------------
// Init-segment leaf boxes (Mvhd / Tkhd / Trex / Mdhd / Hdlr / Smhd
// / Edts+Elst / Dinf+Dref+Url / Stts / Stsc / Stsz / Stco / Matrix).
// ---------------------------------------------------------------------

/// `tkhd` / `mvhd` orientation matrix. Default is the unity matrix
/// per ISO/IEC 14496-12.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Matrix {
    pub a: i32,
    pub b: i32,
    pub u: i32,
    pub c: i32,
    pub d: i32,
    pub v: i32,
    pub x: i32,
    pub y: i32,
    pub w: i32,
}

impl Default for Matrix {
    fn default() -> Self {
        // Unity matrix per ISO/IEC 14496-12 §6.2.2.
        Self {
            a: 0x00010000,
            b: 0,
            u: 0,
            c: 0,
            d: 0x00010000,
            v: 0,
            x: 0,
            y: 0,
            w: 0x40000000,
        }
    }
}

impl Matrix {
    fn encode(&self, buf: &mut Vec<u8>) {
        for v in [
            self.a, self.b, self.u, self.c, self.d, self.v, self.x, self.y, self.w,
        ] {
            buf.extend_from_slice(&v.to_be_bytes());
        }
    }
}

/// Movie Header (`mvhd`). FullBox v1; always writes 64-bit times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mvhd {
    pub creation_time: u64,
    pub modification_time: u64,
    pub timescale: u32,
    pub duration: u64,
    pub rate: FixedPoint<u16>,
    pub volume: FixedPoint<u8>,
    pub matrix: Matrix,
    pub next_track_id: u32,
}

impl Default for Mvhd {
    fn default() -> Self {
        Mvhd {
            creation_time: 0,
            modification_time: 0,
            timescale: 1000,
            duration: 0,
            rate: FixedPoint::default(),
            volume: FixedPoint::default(),
            matrix: Matrix::default(),
            next_track_id: 1,
        }
    }
}

impl FullBox for Mvhd {
    const KIND: FourCC = FourCC::new(b"mvhd");
    /// v0 (32-bit creation/modification/duration) whenever all three
    /// fields fit, v1 (64-bit) otherwise. v0 is what ffmpeg writes by
    /// default and what every shipping MSE implementation expects on
    /// the init segment.
    fn version(&self) -> u8 {
        if mvhd_fits_v0(self) { 0 } else { 1 }
    }
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        if mvhd_fits_v0(self) {
            buf.extend_from_slice(&(self.creation_time as u32).to_be_bytes());
            buf.extend_from_slice(&(self.modification_time as u32).to_be_bytes());
            buf.extend_from_slice(&self.timescale.to_be_bytes());
            buf.extend_from_slice(&(self.duration as u32).to_be_bytes());
        } else {
            buf.extend_from_slice(&self.creation_time.to_be_bytes());
            buf.extend_from_slice(&self.modification_time.to_be_bytes());
            buf.extend_from_slice(&self.timescale.to_be_bytes());
            buf.extend_from_slice(&self.duration.to_be_bytes());
        }
        buf.extend_from_slice(&self.rate.int.to_be_bytes());
        buf.extend_from_slice(&self.rate.dec.to_be_bytes());
        buf.extend_from_slice(&self.volume.int.to_be_bytes());
        buf.extend_from_slice(&self.volume.dec.to_be_bytes());
        buf.extend_from_slice(&[0u8; 2]); // reserved u16
        buf.extend_from_slice(&[0u8; 8]); // reserved u64
        self.matrix.encode(buf);
        buf.extend_from_slice(&[0u8; 24]); // pre_defined
        buf.extend_from_slice(&self.next_track_id.to_be_bytes());
        Ok(())
    }
    fn decode_full_body(_version: u8, _flags: u32, _buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Mvhd decode not implemented".into(),
        ))
    }
}

fn mvhd_fits_v0(m: &Mvhd) -> bool {
    m.creation_time <= u32::MAX as u64
        && m.modification_time <= u32::MAX as u64
        && m.duration <= u32::MAX as u64
}

impl Encode for Mvhd {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

/// Track Header (`tkhd`). FullBox v1. **Patch #2 (tkhd flags) is
/// baked in:** flags = `track_enabled | track_in_movie` = `0x000003`
/// always, matching Apple CoreMedia's HLS / iOS Safari requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tkhd {
    pub creation_time: u64,
    pub modification_time: u64,
    pub track_id: u32,
    pub duration: u64,
    pub layer: u16,
    pub alternate_group: u16,
    pub volume: FixedPoint<u8>,
    pub matrix: Matrix,
    pub width: FixedPoint<u16>,
    pub height: FixedPoint<u16>,
}

impl Default for Tkhd {
    fn default() -> Self {
        Tkhd {
            creation_time: 0,
            modification_time: 0,
            track_id: 1,
            duration: 0,
            layer: 0,
            alternate_group: 0,
            volume: FixedPoint::default(),
            matrix: Matrix::default(),
            width: FixedPoint::default(),
            height: FixedPoint::default(),
        }
    }
}

impl FullBox for Tkhd {
    const KIND: FourCC = FourCC::new(b"tkhd");
    /// v0 (32-bit) whenever all of creation/modification/duration fit
    /// in u32, v1 otherwise. ffmpeg writes v0 by default.
    fn version(&self) -> u8 {
        if tkhd_fits_v0(self) { 0 } else { 1 }
    }
    fn flags(&self) -> u32 {
        // track_enabled (bit 0) + track_in_movie (bit 1) — matches the
        // bytewise post-encoding patch (patches::patch_tkhd_flags).
        0x000003
    }
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        if tkhd_fits_v0(self) {
            buf.extend_from_slice(&(self.creation_time as u32).to_be_bytes());
            buf.extend_from_slice(&(self.modification_time as u32).to_be_bytes());
            buf.extend_from_slice(&self.track_id.to_be_bytes());
            buf.extend_from_slice(&[0u8; 4]); // reserved u32
            buf.extend_from_slice(&(self.duration as u32).to_be_bytes());
        } else {
            buf.extend_from_slice(&self.creation_time.to_be_bytes());
            buf.extend_from_slice(&self.modification_time.to_be_bytes());
            buf.extend_from_slice(&self.track_id.to_be_bytes());
            buf.extend_from_slice(&[0u8; 4]); // reserved u32
            buf.extend_from_slice(&self.duration.to_be_bytes());
        }
        buf.extend_from_slice(&[0u8; 8]); // reserved u64
        buf.extend_from_slice(&self.layer.to_be_bytes());
        buf.extend_from_slice(&self.alternate_group.to_be_bytes());
        buf.extend_from_slice(&self.volume.int.to_be_bytes());
        buf.extend_from_slice(&self.volume.dec.to_be_bytes());
        buf.extend_from_slice(&[0u8; 2]); // reserved u16
        self.matrix.encode(buf);
        buf.extend_from_slice(&self.width.int.to_be_bytes());
        buf.extend_from_slice(&self.width.dec.to_be_bytes());
        buf.extend_from_slice(&self.height.int.to_be_bytes());
        buf.extend_from_slice(&self.height.dec.to_be_bytes());
        Ok(())
    }
    fn decode_full_body(_version: u8, _flags: u32, _buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Tkhd decode not implemented".into(),
        ))
    }
}

fn tkhd_fits_v0(t: &Tkhd) -> bool {
    t.creation_time <= u32::MAX as u64
        && t.modification_time <= u32::MAX as u64
        && t.duration <= u32::MAX as u64
}

impl Encode for Tkhd {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

/// Track Extends (`trex`). FullBox v0.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trex {
    pub track_id: u32,
    pub default_sample_description_index: u32,
    pub default_sample_duration: u32,
    pub default_sample_size: u32,
    pub default_sample_flags: u32,
}

impl FullBox for Trex {
    const KIND: FourCC = FourCC::new(b"trex");
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(&self.track_id.to_be_bytes());
        buf.extend_from_slice(&self.default_sample_description_index.to_be_bytes());
        buf.extend_from_slice(&self.default_sample_duration.to_be_bytes());
        buf.extend_from_slice(&self.default_sample_size.to_be_bytes());
        buf.extend_from_slice(&self.default_sample_flags.to_be_bytes());
        Ok(())
    }
    fn decode_full_body(_version: u8, _flags: u32, _buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Trex decode not implemented".into(),
        ))
    }
}

impl Encode for Trex {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

/// Edit-List entry (`elst.entries[i]`). Always v1 — segment_duration
/// and media_time are 64-bit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ElstEntry {
    pub segment_duration: u64,
    pub media_time: u64,
    pub media_rate: u16,
    pub media_rate_fraction: u16,
}

/// Edit-List (`elst`). FullBox v1.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Elst {
    pub entries: Vec<ElstEntry>,
}

impl FullBox for Elst {
    const KIND: FourCC = FourCC::new(b"elst");
    fn version(&self) -> u8 {
        1
    }
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(&(self.entries.len() as u32).to_be_bytes());
        for e in &self.entries {
            buf.extend_from_slice(&e.segment_duration.to_be_bytes());
            buf.extend_from_slice(&e.media_time.to_be_bytes());
            buf.extend_from_slice(&e.media_rate.to_be_bytes());
            buf.extend_from_slice(&e.media_rate_fraction.to_be_bytes());
        }
        Ok(())
    }
    fn decode_full_body(_version: u8, _flags: u32, _buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Elst decode not implemented".into(),
        ))
    }
}

impl Encode for Elst {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

/// Edit-Box container (`edts`). Holds the elst.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Edts {
    pub elst: Option<Elst>,
}

impl Atom for Edts {
    const KIND: FourCC = FourCC::new(b"edts");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        if let Some(e) = &self.elst {
            e.encode(buf)?;
        }
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Edts decode not implemented".into(),
        ))
    }
}

/// Media Header (`mdhd`). FullBox v1. ISO 639-2 language packed as
/// three 5-bit chars + 0x60 offset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mdhd {
    pub creation_time: u64,
    pub modification_time: u64,
    pub timescale: u32,
    pub duration: u64,
    pub language: String,
}

fn language_code(lang: &str) -> u16 {
    let mut iter = lang.encode_utf16();
    let mut code = (iter.next().unwrap_or(0) & 0x1F) << 10;
    code += (iter.next().unwrap_or(0) & 0x1F) << 5;
    code += iter.next().unwrap_or(0) & 0x1F;
    code
}

impl FullBox for Mdhd {
    const KIND: FourCC = FourCC::new(b"mdhd");
    fn version(&self) -> u8 {
        if mdhd_fits_v0(self) { 0 } else { 1 }
    }
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        if mdhd_fits_v0(self) {
            buf.extend_from_slice(&(self.creation_time as u32).to_be_bytes());
            buf.extend_from_slice(&(self.modification_time as u32).to_be_bytes());
            buf.extend_from_slice(&self.timescale.to_be_bytes());
            buf.extend_from_slice(&(self.duration as u32).to_be_bytes());
        } else {
            buf.extend_from_slice(&self.creation_time.to_be_bytes());
            buf.extend_from_slice(&self.modification_time.to_be_bytes());
            buf.extend_from_slice(&self.timescale.to_be_bytes());
            buf.extend_from_slice(&self.duration.to_be_bytes());
        }
        buf.extend_from_slice(&language_code(&self.language).to_be_bytes());
        buf.extend_from_slice(&[0u8; 2]); // pre_defined
        Ok(())
    }
    fn decode_full_body(_version: u8, _flags: u32, _buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Mdhd decode not implemented".into(),
        ))
    }
}

fn mdhd_fits_v0(m: &Mdhd) -> bool {
    m.creation_time <= u32::MAX as u64
        && m.modification_time <= u32::MAX as u64
        && m.duration <= u32::MAX as u64
}

impl Encode for Mdhd {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

/// Handler Reference (`hdlr`). FullBox v0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hdlr {
    pub handler: FourCC,
    pub name: String,
}

impl Default for Hdlr {
    fn default() -> Self {
        Hdlr {
            handler: FourCC::new(b"none"),
            name: String::new(),
        }
    }
}

impl FullBox for Hdlr {
    const KIND: FourCC = FourCC::new(b"hdlr");
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(&[0u8; 4]); // pre_defined
        buf.extend_from_slice(self.handler.as_ref());
        buf.extend_from_slice(&[0u8; 12]); // reserved
        buf.extend_from_slice(self.name.as_bytes());
        buf.push(0); // null terminator
        Ok(())
    }
    fn decode_full_body(_version: u8, _flags: u32, _buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Hdlr decode not implemented".into(),
        ))
    }
}

impl Encode for Hdlr {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

/// Sound Media Header (`smhd`). FullBox v0.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Smhd {
    pub balance: FixedPoint<i8>,
}

impl FullBox for Smhd {
    const KIND: FourCC = FourCC::new(b"smhd");
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.push(self.balance.int as u8);
        buf.push(self.balance.dec as u8);
        buf.extend_from_slice(&[0u8; 2]); // reserved
        Ok(())
    }
    fn decode_full_body(_version: u8, _flags: u32, _buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Smhd decode not implemented".into(),
        ))
    }
}

impl Encode for Smhd {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

/// Video Media Header (`vmhd`). FullBox v0, flags=1 (no_lean_ahead).
/// Body is `graphicsmode:u16 + opcolor:[u16;3]` per ISO/IEC 14496-12 §8.4.5.2.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Vmhd {
    pub graphicsmode: u16,
    pub opcolor: [u16; 3],
}

impl FullBox for Vmhd {
    const KIND: FourCC = FourCC::new(b"vmhd");
    fn flags(&self) -> u32 {
        // Spec mandates flags = 1 for vmhd.
        0x000001
    }
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(&self.graphicsmode.to_be_bytes());
        for c in &self.opcolor {
            buf.extend_from_slice(&c.to_be_bytes());
        }
        Ok(())
    }
    fn decode_full_body(_version: u8, _flags: u32, _buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Vmhd decode not implemented".into(),
        ))
    }
}

impl Encode for Vmhd {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

/// Data Reference URL entry (`url `). FullBox v0. **Patch #3 (dref
/// url self_contained) is baked in:** flags = `0x000001` (bit 0,
/// the ISO spec position) instead of mp4-atom's incorrect bit 1.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Url {
    pub location: String,
}

impl FullBox for Url {
    const KIND: FourCC = FourCC::new(b"url ");
    fn flags(&self) -> u32 {
        // ISO/IEC 14496-12 §8.7.2.2: bit 0 = "data is in this box".
        // mp4-atom emitted 0x02; the bytewise patch flips it to 0x01.
        0x000001
    }
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        // Spec: when the self_contained flag is set, no location
        // string is written (the data is in the same file).
        if !self.location.is_empty() {
            buf.extend_from_slice(self.location.as_bytes());
            buf.push(0);
        }
        Ok(())
    }
    fn decode_full_body(_version: u8, _flags: u32, _buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed("Url decode not implemented".into()))
    }
}

impl Encode for Url {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

/// Data Reference (`dref`). FullBox v0 containing N urls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dref {
    pub urls: Vec<Url>,
}

impl FullBox for Dref {
    const KIND: FourCC = FourCC::new(b"dref");
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(&(self.urls.len() as u32).to_be_bytes());
        for u in &self.urls {
            u.encode(buf)?;
        }
        Ok(())
    }
    fn decode_full_body(_version: u8, _flags: u32, _buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Dref decode not implemented".into(),
        ))
    }
}

impl Encode for Dref {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

/// Data Information container (`dinf`). Atom container around `dref`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dinf {
    pub dref: Dref,
}

impl Atom for Dinf {
    const KIND: FourCC = FourCC::new(b"dinf");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        self.dref.encode(buf)
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Dinf decode not implemented".into(),
        ))
    }
}

// ---------------------------------------------------------------------
// stbl child boxes (Stts / Stsc / Stsz / Stco) — all FullBox v0 lists.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SttsEntry {
    pub sample_count: u32,
    pub sample_delta: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stts {
    pub entries: Vec<SttsEntry>,
}

impl FullBox for Stts {
    const KIND: FourCC = FourCC::new(b"stts");
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(&(self.entries.len() as u32).to_be_bytes());
        for e in &self.entries {
            buf.extend_from_slice(&e.sample_count.to_be_bytes());
            buf.extend_from_slice(&e.sample_delta.to_be_bytes());
        }
        Ok(())
    }
    fn decode_full_body(_version: u8, _flags: u32, _buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Stts decode not implemented".into(),
        ))
    }
}

impl Encode for Stts {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StscEntry {
    pub first_chunk: u32,
    pub samples_per_chunk: u32,
    pub sample_description_index: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stsc {
    pub entries: Vec<StscEntry>,
}

impl FullBox for Stsc {
    const KIND: FourCC = FourCC::new(b"stsc");
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(&(self.entries.len() as u32).to_be_bytes());
        for e in &self.entries {
            buf.extend_from_slice(&e.first_chunk.to_be_bytes());
            buf.extend_from_slice(&e.samples_per_chunk.to_be_bytes());
            buf.extend_from_slice(&e.sample_description_index.to_be_bytes());
        }
        Ok(())
    }
    fn decode_full_body(_version: u8, _flags: u32, _buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Stsc decode not implemented".into(),
        ))
    }
}

impl Encode for Stsc {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

/// `stsz` body shape. When every sample has the same size the box
/// encodes that constant once (`Identical`); otherwise it enumerates
/// per-sample sizes (`Different`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StszSamples {
    Identical { count: u32, size: u32 },
    Different { sizes: Vec<u32> },
}

impl Default for StszSamples {
    fn default() -> Self {
        StszSamples::Different { sizes: vec![] }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stsz {
    pub samples: StszSamples,
}

impl FullBox for Stsz {
    const KIND: FourCC = FourCC::new(b"stsz");
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        match &self.samples {
            StszSamples::Identical { count, size } => {
                buf.extend_from_slice(&size.to_be_bytes());
                buf.extend_from_slice(&count.to_be_bytes());
            }
            StszSamples::Different { sizes } => {
                buf.extend_from_slice(&0u32.to_be_bytes());
                buf.extend_from_slice(&(sizes.len() as u32).to_be_bytes());
                for s in sizes {
                    buf.extend_from_slice(&s.to_be_bytes());
                }
            }
        }
        Ok(())
    }
    fn decode_full_body(_version: u8, _flags: u32, _buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Stsz decode not implemented".into(),
        ))
    }
}

impl Encode for Stsz {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

/// Chunk Offset (`stco`, 32-bit). FullBox v0. `co64` (the 64-bit
/// variant) is not supported by this writer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stco {
    pub entries: Vec<u32>,
}

impl FullBox for Stco {
    const KIND: FourCC = FourCC::new(b"stco");
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(&(self.entries.len() as u32).to_be_bytes());
        for e in &self.entries {
            buf.extend_from_slice(&e.to_be_bytes());
        }
        Ok(())
    }
    fn decode_full_body(_version: u8, _flags: u32, _buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Stco decode not implemented".into(),
        ))
    }
}

impl Encode for Stco {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

// ---------------------------------------------------------------------
// Codec sample entries (Mp4a + Esds / Opus + Dops / Flac + Dfla)
// and the Codec enum + Stsd wrapper. Patch #3 (esds expandable
// descriptor lengths — required by Apple CoreMedia / iOS AVPlayer)
// is baked into the native Esds writer.
// ---------------------------------------------------------------------

/// Common audio sample-entry header (ISO/IEC 14496-12 §8.5.2). Shared
/// by Mp4a / Opus / Flac. Not a box itself — it's the base record
/// that lives inside each audio sample entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Audio {
    pub data_reference_index: u16,
    pub channel_count: u16,
    pub sample_size: u16,
    pub sample_rate: FixedPoint<u16>,
}

impl Audio {
    fn encode_into(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&[0u8; 6]); // reserved (u32 + u16)
        buf.extend_from_slice(&self.data_reference_index.to_be_bytes());
        buf.extend_from_slice(&[0u8; 8]); // version(u16)+reserved(u16+u32)
        buf.extend_from_slice(&self.channel_count.to_be_bytes());
        buf.extend_from_slice(&self.sample_size.to_be_bytes());
        buf.extend_from_slice(&[0u8; 4]); // pre_defined + reserved
        buf.extend_from_slice(&self.sample_rate.int.to_be_bytes());
        buf.extend_from_slice(&self.sample_rate.dec.to_be_bytes());
    }
}

/// Pixel Aspect Ratio (`pasp`). Atom (not FullBox). Carries the
/// horizontal/vertical pixel ratio so the renderer can correct for
/// non-square pixels (DVD/MPEG-2 sources, anamorphic encodes). 1:1
/// for the modern square-pixel case. ffmpeg writes this on every
/// video sample entry; some Safari MSE versions stall without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pasp {
    pub h_spacing: u32,
    pub v_spacing: u32,
}

impl Default for Pasp {
    fn default() -> Self {
        Pasp {
            h_spacing: 1,
            v_spacing: 1,
        }
    }
}

impl Atom for Pasp {
    const KIND: FourCC = FourCC::new(b"pasp");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(&self.h_spacing.to_be_bytes());
        buf.extend_from_slice(&self.v_spacing.to_be_bytes());
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Pasp decode not implemented".into(),
        ))
    }
}

/// Bitrate (`btrt`). Atom (not FullBox).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Btrt {
    pub buffer_size_db: u32,
    pub max_bitrate: u32,
    pub avg_bitrate: u32,
}

impl Atom for Btrt {
    const KIND: FourCC = FourCC::new(b"btrt");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(&self.buffer_size_db.to_be_bytes());
        buf.extend_from_slice(&self.max_bitrate.to_be_bytes());
        buf.extend_from_slice(&self.avg_bitrate.to_be_bytes());
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Btrt decode not implemented".into(),
        ))
    }
}

// ---------------------------------------------------------------------
// Opus.
// ---------------------------------------------------------------------

/// Opus specific data (`dOps`). Atom v0. We only support
/// channel_mapping_family = 0 (mono / stereo without a mapping table)
/// — other mapping families are unsupported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dops {
    pub output_channel_count: u8,
    pub pre_skip: u16,
    pub input_sample_rate: u32,
    pub output_gain: i16,
}

impl Atom for Dops {
    const KIND: FourCC = FourCC::new(b"dOps");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.push(0); // Version
        buf.push(self.output_channel_count);
        buf.extend_from_slice(&self.pre_skip.to_be_bytes());
        buf.extend_from_slice(&self.input_sample_rate.to_be_bytes());
        buf.extend_from_slice(&self.output_gain.to_be_bytes());
        buf.push(0); // ChannelMappingFamily = 0
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Dops decode not implemented".into(),
        ))
    }
}

/// Opus sample entry (`Opus`). Atom containing Audio + Dops + optional Btrt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opus {
    pub audio: Audio,
    pub dops: Dops,
    pub btrt: Option<Btrt>,
}

impl Atom for Opus {
    const KIND: FourCC = FourCC::new(b"Opus");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        self.audio.encode_into(buf);
        self.dops.encode(buf)?;
        if let Some(b) = &self.btrt {
            b.encode(buf)?;
        }
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Opus decode not implemented".into(),
        ))
    }
}

// ---------------------------------------------------------------------
// FLAC.
// ---------------------------------------------------------------------

/// FLAC metadata block (RFC 9639). Production paths emit StreamInfo
/// always, sometimes followed by VorbisComment. Other variants are
/// no-ops on write (matches mp4-atom's behavior for compat).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlacMetadataBlock {
    StreamInfo {
        minimum_block_size: u16,
        maximum_block_size: u16,
        minimum_frame_size: u32,
        maximum_frame_size: u32,
        sample_rate: u32,
        num_channels_minus_one: u8,
        bits_per_sample_minus_one: u8,
        number_of_interchannel_samples: u64,
        md5_checksum: Vec<u8>,
    },
    Padding,
    Application,
    SeekTable,
    VorbisComment {
        vendor_string: String,
        comments: Vec<String>,
    },
    CueSheet,
    Picture,
    Reserved,
    Forbidden,
}

impl FlacMetadataBlock {
    fn encode_into(&self, buf: &mut Vec<u8>, is_last: bool) -> IsobmffResult<()> {
        // Block-header layout: 1 byte (last-flag | block_type) + u24 length.
        // Reserve a 4-byte header, fill body, then patch the length.
        let block_type: u8 = match self {
            Self::StreamInfo { .. } => 0,
            Self::Padding => 1,
            Self::Application => 2,
            Self::SeekTable => 3,
            Self::VorbisComment { .. } => 4,
            Self::CueSheet => 5,
            Self::Picture => 6,
            Self::Reserved => return Ok(()),
            Self::Forbidden => return Ok(()),
        };
        let header_byte = if is_last {
            0x80 | block_type
        } else {
            block_type
        };
        let header_at = buf.len();
        buf.push(header_byte);
        buf.extend_from_slice(&[0u8; 3]); // u24 length placeholder
        let body_start = buf.len();
        match self {
            Self::StreamInfo {
                minimum_block_size,
                maximum_block_size,
                minimum_frame_size,
                maximum_frame_size,
                sample_rate,
                num_channels_minus_one,
                bits_per_sample_minus_one,
                number_of_interchannel_samples,
                md5_checksum,
            } => {
                buf.extend_from_slice(&minimum_block_size.to_be_bytes());
                buf.extend_from_slice(&maximum_block_size.to_be_bytes());
                let min_be = minimum_frame_size.to_be_bytes();
                buf.extend_from_slice(&min_be[1..]); // u24
                let max_be = maximum_frame_size.to_be_bytes();
                buf.extend_from_slice(&max_be[1..]); // u24
                let packed: u64 = ((*sample_rate as u64) << 44)
                    | ((*num_channels_minus_one as u64) << 41)
                    | ((*bits_per_sample_minus_one as u64) << 36)
                    | number_of_interchannel_samples;
                buf.extend_from_slice(&packed.to_be_bytes());
                if md5_checksum.len() != 16 {
                    return Err(IsobmffError::Malformed(format!(
                        "StreamInfo md5 must be 16 bytes, got {}",
                        md5_checksum.len()
                    )));
                }
                buf.extend_from_slice(md5_checksum);
            }
            Self::VorbisComment {
                vendor_string,
                comments,
            } => {
                let vb = vendor_string.as_bytes();
                buf.extend_from_slice(&(vb.len() as u32).to_le_bytes());
                buf.extend_from_slice(vb);
                buf.extend_from_slice(&(comments.len() as u32).to_le_bytes());
                for c in comments {
                    let cb = c.as_bytes();
                    buf.extend_from_slice(&(cb.len() as u32).to_le_bytes());
                    buf.extend_from_slice(cb);
                }
            }
            _ => {}
        }
        let body_len = (buf.len() - body_start) as u32;
        if body_len > u24::MAX {
            return Err(IsobmffError::Malformed(format!(
                "FLAC metadata block body {body_len} exceeds u24"
            )));
        }
        let len_be = body_len.to_be_bytes();
        buf[header_at + 1] = len_be[1];
        buf[header_at + 2] = len_be[2];
        buf[header_at + 3] = len_be[3];
        Ok(())
    }
}

/// FLAC specific data (`dfLa`). FullBox v0. Body is a back-to-back
/// run of [`FlacMetadataBlock`]s.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dfla {
    pub blocks: Vec<FlacMetadataBlock>,
}

impl FullBox for Dfla {
    const KIND: FourCC = FourCC::new(b"dfLa");
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        if self.blocks.is_empty() {
            return Err(IsobmffError::Malformed(
                "dfLa requires at least the StreamInfo block".into(),
            ));
        }
        let n = self.blocks.len();
        for (i, block) in self.blocks.iter().enumerate() {
            block.encode_into(buf, i + 1 == n)?;
        }
        Ok(())
    }
    fn decode_full_body(_version: u8, _flags: u32, _buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Dfla decode not implemented".into(),
        ))
    }
}

impl Encode for Dfla {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

/// FLAC sample entry (`fLaC`). Atom containing Audio + Dfla.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flac {
    pub audio: Audio,
    pub dfla: Dfla,
}

impl Atom for Flac {
    const KIND: FourCC = FourCC::new(b"fLaC");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        self.audio.encode_into(buf);
        self.dfla.encode(buf)?;
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Flac decode not implemented".into(),
        ))
    }
}

// ---------------------------------------------------------------------
// MP4 audio (`mp4a`) + ES descriptor tree (`esds`). Bakes in patch
// #3: descriptor lengths use the 4-byte expandable form `0x80 0x80
// 0x80 LEN` (Apple CoreMedia rejects the compact form).
// ---------------------------------------------------------------------

pub mod esds {
    use super::IsobmffResult;

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct SLConfig;

    impl SLConfig {
        pub const TAG: u8 = 0x06;
        pub fn encode_into(&self, buf: &mut Vec<u8>) {
            buf.push(2); // pre-defined
        }
    }

    /// `DecoderSpecific` (DecSpecificInfo). For AAC this is the
    /// 2-byte AudioSpecificConfig. The current consumer only emits
    /// the simple non-extended profile path
    /// (`profile, freq_index, chan_conf`).
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct DecoderSpecific {
        pub profile: u8,
        pub freq_index: u8,
        pub chan_conf: u8,
    }

    impl DecoderSpecific {
        pub const TAG: u8 = 0x05;
        pub fn encode_into(&self, buf: &mut Vec<u8>) {
            buf.push((self.profile << 3) + (self.freq_index >> 1));
            buf.push((self.freq_index << 7) + (self.chan_conf << 3));
        }
    }

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct DecoderConfig {
        pub object_type_indication: u8,
        pub stream_type: u8,
        pub up_stream: u8,
        pub buffer_size_db: u32,
        pub max_bitrate: u32,
        pub avg_bitrate: u32,
        pub dec_specific: DecoderSpecific,
    }

    impl DecoderConfig {
        pub const TAG: u8 = 0x04;
        pub fn encode_into(&self, buf: &mut Vec<u8>) {
            buf.push(self.object_type_indication);
            buf.push((self.stream_type << 2) + (self.up_stream & 0x02) + 1);
            // buffer_size_db is u24
            let be = self.buffer_size_db.to_be_bytes();
            buf.extend_from_slice(&be[1..]);
            buf.extend_from_slice(&self.max_bitrate.to_be_bytes());
            buf.extend_from_slice(&self.avg_bitrate.to_be_bytes());
            // Nested DecoderSpecific descriptor.
            write_descriptor(buf, DecoderSpecific::TAG, |b| {
                self.dec_specific.encode_into(b)
            });
        }
    }

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct EsDescriptor {
        pub es_id: u16,
        pub dec_config: DecoderConfig,
        pub sl_config: SLConfig,
    }

    impl EsDescriptor {
        pub const TAG: u8 = 0x03;
        pub fn encode_into(&self, buf: &mut Vec<u8>) {
            buf.extend_from_slice(&self.es_id.to_be_bytes());
            buf.push(0); // flags (priority + dependency + URL bits, all zero)
            write_descriptor(buf, DecoderConfig::TAG, |b| self.dec_config.encode_into(b));
            write_descriptor(buf, SLConfig::TAG, |b| self.sl_config.encode_into(b));
        }
    }

    /// Write `tag` + `0x80 0x80 0x80 LEN` + body. The 4-byte
    /// expandable length encoding is what Apple CoreMedia accepts
    /// (mp4-atom emits the compact 1-byte form, which CoreMedia
    /// rejects — patch #3 in `crate::patches`). The native path
    /// uses expandable always.
    fn write_descriptor(buf: &mut Vec<u8>, tag: u8, f: impl FnOnce(&mut Vec<u8>)) {
        buf.push(tag);
        let len_at = buf.len();
        buf.extend_from_slice(&[0x80, 0x80, 0x80, 0]);
        f(buf);
        let body_len = buf.len() - (len_at + 4);
        // Length is the bottom 7 bits of the 4th byte; the three
        // continuation prefix bytes stay 0x80.
        buf[len_at + 3] = (body_len & 0x7F) as u8;
    }

    pub(super) fn encode_es_descriptor(es: &EsDescriptor, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        write_descriptor(buf, EsDescriptor::TAG, |b| es.encode_into(b));
        Ok(())
    }
}

/// Elementary Stream Descriptor (`esds`). FullBox v0 wrapping a
/// single ES descriptor in the expandable-length form.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Esds {
    pub es_desc: esds::EsDescriptor,
}

impl FullBox for Esds {
    const KIND: FourCC = FourCC::new(b"esds");
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        esds::encode_es_descriptor(&self.es_desc, buf)
    }
    fn decode_full_body(_version: u8, _flags: u32, _buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Esds decode not implemented".into(),
        ))
    }
}

impl Encode for Esds {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

/// MP4 audio sample entry (`mp4a`). Atom containing Audio + Esds +
/// optional Btrt. The `taic` (TAI clock) child mp4-atom supports is
/// not yet wired here — the audio encoders don't write it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mp4a {
    pub audio: Audio,
    pub esds: Esds,
    pub btrt: Option<Btrt>,
}

impl Atom for Mp4a {
    const KIND: FourCC = FourCC::new(b"mp4a");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        self.audio.encode_into(buf);
        self.esds.encode(buf)?;
        if let Some(b) = &self.btrt {
            b.encode(buf)?;
        }
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Mp4a decode not implemented".into(),
        ))
    }
}

// ---------------------------------------------------------------------
// Visual sample-entry header — ISO/IEC 14496-12 §8.5.2.2. Counterpart
// to `Audio` above; shared shape for video sample entries (`avc1`/`avc3`/`hvc1`/`hev1`/`av01`). Not a box
// itself — the base record that lives inside each video sample entry.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visual {
    pub data_reference_index: u16,
    pub width: u16,
    pub height: u16,
}

impl Visual {
    fn encode_into(&self, buf: &mut Vec<u8>) {
        // SampleEntry base (ISO/IEC 14496-12 §8.5.2.1)
        buf.extend_from_slice(&[0u8; 6]); // reserved
        buf.extend_from_slice(&self.data_reference_index.to_be_bytes());
        // VisualSampleEntry (§8.5.2.2)
        buf.extend_from_slice(&[0u8; 2]); // pre_defined = 0
        buf.extend_from_slice(&[0u8; 2]); // reserved = 0
        buf.extend_from_slice(&[0u8; 12]); // pre_defined[3] = {0,0,0}
        buf.extend_from_slice(&self.width.to_be_bytes());
        buf.extend_from_slice(&self.height.to_be_bytes());
        buf.extend_from_slice(&0x00480000u32.to_be_bytes()); // horizresolution = 72 dpi (16.16 fp)
        buf.extend_from_slice(&0x00480000u32.to_be_bytes()); // vertresolution = 72 dpi
        buf.extend_from_slice(&0u32.to_be_bytes()); // reserved
        buf.extend_from_slice(&1u16.to_be_bytes()); // frame_count = 1
        // compressorname: 32-byte Pascal-style string. Zeroed for our
        // streamed output — players ignore this field for MSE input,
        // and it's only informational for offline tools.
        buf.extend_from_slice(&[0u8; 32]);
        buf.extend_from_slice(&0x0018u16.to_be_bytes()); // depth = 24
        buf.extend_from_slice(&(-1i16).to_be_bytes()); // pre_defined = -1
    }
}

// ---------------------------------------------------------------------
// AVC (`avc1` + `avcC`). The configuration record is the same blob
// `hw_codec::EncodedPacket::codec_config` emits — we wrap it as-is
// without re-parsing the SPS/PPS payload. Decoding back into a typed
// representation is unsupported.
// ---------------------------------------------------------------------

/// AVC decoder configuration record (`avcC`). Atom containing the raw
/// AVCDecoderConfigurationRecord bytes per ISO/IEC 14496-15 §5.2.4.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvcC {
    pub configuration_record: Vec<u8>,
}

impl Atom for AvcC {
    const KIND: FourCC = FourCC::new(b"avcC");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(&self.configuration_record);
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "AvcC decode not implemented".into(),
        ))
    }
}

/// AVC sample entry (`avc1`). Atom containing Visual + AvcC + optional
/// Btrt.
///
/// `avc1` carries SPS/PPS **out of band only** — in the avcC here, never
/// in the mdat samples (ISO/IEC 14496-15 §5.3.4). [`Avc3`] is the
/// in-band counterpart. An earlier version of this comment had the two
/// the wrong way round, which is worth naming: a muxer that writes
/// `avc1` over an encoder emitting per-IDR parameter sets produces a
/// non-conformant file that our own decoder still reads back fine,
/// because it is fed the samples concatenated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Avc1 {
    pub visual: Visual,
    pub avcc: AvcC,
    pub pasp: Option<Pasp>,
    pub btrt: Option<Btrt>,
}

impl Atom for Avc1 {
    const KIND: FourCC = FourCC::new(b"avc1");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        self.visual.encode_into(buf);
        self.avcc.encode(buf)?;
        if let Some(p) = &self.pasp {
            p.encode(buf)?;
        }
        if let Some(b) = &self.btrt {
            b.encode(buf)?;
        }
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Avc1 decode not implemented".into(),
        ))
    }
}

/// AVC sample entry (`avc3`). Identical body shape to [`Avc1`], only the
/// FourCC differs — the same relationship [`Hev1`] has to [`Hvc1`].
///
/// Per ISO/IEC 14496-15 the `avc3` brand PERMITS in-band parameter set
/// NALs (SPS/PPS) in mdat samples, in addition to the sample entry's
/// avcC; `avc1` forbids them. Hardware encoders routinely emit SPS/PPS
/// alongside every IDR, so an encoder-driven path that muxes as `avc1`
/// without stripping them is writing a file no strict parser should
/// accept. `avc3` permits but does not require in-band sets, so it is
/// also correct for an encoder that emits none — which makes it the safe
/// choice when the muxer cannot know which kind of encoder it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Avc3 {
    pub visual: Visual,
    pub avcc: AvcC,
    pub pasp: Option<Pasp>,
    pub btrt: Option<Btrt>,
}

impl Atom for Avc3 {
    const KIND: FourCC = FourCC::new(b"avc3");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        self.visual.encode_into(buf);
        self.avcc.encode(buf)?;
        if let Some(p) = &self.pasp {
            p.encode(buf)?;
        }
        if let Some(b) = &self.btrt {
            b.encode(buf)?;
        }
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Avc3 decode not implemented".into(),
        ))
    }
}

/// HEVC decoder configuration record (`hvcC`). Atom containing the raw
/// HEVCDecoderConfigurationRecord bytes per ISO/IEC 14496-15 §8.3.3.1.
/// Same opaque-blob posture as [`AvcC`]: callers supply the bytes;
/// this writer doesn't reparse them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvcC {
    pub configuration_record: Vec<u8>,
}

impl Atom for HvcC {
    const KIND: FourCC = FourCC::new(b"hvcC");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(&self.configuration_record);
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "HvcC decode not implemented".into(),
        ))
    }
}

/// HEVC sample entry (`hvc1`). Atom containing Visual + HvcC + optional
/// Btrt. Per ISO/IEC 14496-15 §8.4.3, the `hvc1` brand REQUIRES that
/// every VPS/SPS/PPS NAL appears in the sample entry's hvcC and that
/// in-band parameter set NALs (within mdat samples) are forbidden.
/// Use [`Hev1`] for container pass-through where the source may
/// embed in-band parameter sets that we can't safely strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hvc1 {
    pub visual: Visual,
    pub hvcc: HvcC,
    pub pasp: Option<Pasp>,
    pub btrt: Option<Btrt>,
}

impl Atom for Hvc1 {
    const KIND: FourCC = FourCC::new(b"hvc1");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        self.visual.encode_into(buf);
        self.hvcc.encode(buf)?;
        if let Some(p) = &self.pasp {
            p.encode(buf)?;
        }
        if let Some(b) = &self.btrt {
            b.encode(buf)?;
        }
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Hvc1 decode not implemented".into(),
        ))
    }
}

/// HEVC sample entry (`hev1`). Identical body shape to [`Hvc1`], only
/// the FourCC differs. Per ISO/IEC 14496-15 §8.4.3 the `hev1` brand
/// PERMITS in-band parameter set NALs in mdat samples (in addition to
/// the sample entry's hvcC). Required for MKV / WebM / TS pass-through
/// pipelines where the source bitstream typically embeds VPS/SPS/PPS
/// alongside each IDR — `hvc1` would silently mis-decode those streams
/// in strict MSE parsers (notably Firefox).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hev1 {
    pub visual: Visual,
    pub hvcc: HvcC,
    pub pasp: Option<Pasp>,
    pub btrt: Option<Btrt>,
}

impl Atom for Hev1 {
    const KIND: FourCC = FourCC::new(b"hev1");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        self.visual.encode_into(buf);
        self.hvcc.encode(buf)?;
        if let Some(p) = &self.pasp {
            p.encode(buf)?;
        }
        if let Some(b) = &self.btrt {
            b.encode(buf)?;
        }
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Hev1 decode not implemented".into(),
        ))
    }
}

/// AV1 codec configuration record (`av1C`). Atom containing the raw
/// AV1CodecConfigurationRecord bytes per ISO/IEC 14496-12 + the
/// AV1-in-ISOBMFF spec §2.3 — a 4-byte fixed header followed by the
/// sequence-header OBU. Same opaque-blob posture as [`AvcC`] / [`HvcC`]:
/// callers supply the configuration bytes and parsers can pass the source's
/// `av1C` body through verbatim;
/// this writer doesn't reparse them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Av1C {
    pub configuration_record: Vec<u8>,
}

impl Atom for Av1C {
    const KIND: FourCC = FourCC::new(b"av1C");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(&self.configuration_record);
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Av1C decode not implemented".into(),
        ))
    }
}

/// AV1 sample entry (`av01`). Atom containing Visual + Av1C + optional
/// Btrt. Unlike AVC/HEVC there's no in-band-vs-out-of-band brand split
/// (AV1 has a single `av01` sample entry); the sequence header lives in
/// the `av1C` and frame OBUs are carried verbatim in each mdat sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Av01 {
    pub visual: Visual,
    pub av1c: Av1C,
    pub pasp: Option<Pasp>,
    pub btrt: Option<Btrt>,
}

impl Atom for Av01 {
    const KIND: FourCC = FourCC::new(b"av01");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        self.visual.encode_into(buf);
        self.av1c.encode(buf)?;
        if let Some(p) = &self.pasp {
            p.encode(buf)?;
        }
        if let Some(b) = &self.btrt {
            b.encode(buf)?;
        }
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Av01 decode not implemented".into(),
        ))
    }
}

// ---------------------------------------------------------------------
// Codec enum + Stsd. Covers audio entries (Mp4a / Opus / Flac), the video variants
// the screen-rec muxer emits (Avc1 / Hvc1 / Hev1 / Av01), plus
// `Unknown` for the rare path where a consumer writes the sample
// entry's bytes itself (alac.rs's hand-built ALACSpecificConfig).
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Codec {
    Mp4a(Mp4a),
    Opus(Opus),
    Flac(Flac),
    Avc1(Avc1),
    Avc3(Avc3),
    Hvc1(Hvc1),
    Hev1(Hev1),
    Av01(Av01),
    /// 4cc-only placeholder. Encodes as `[size=8] [fourcc]` so a
    /// caller can post-replace
    /// the bytes with a hand-built sample entry.
    Unknown(FourCC),
    /// Hand-built sample entry with arbitrary body bytes. Used by the
    /// audio pass-through path in the muxer, where the source's
    /// AudioSpecificConfig / dfLa body is variable-length and bypassing
    /// the typed `Mp4a` / `Flac` builders is simpler than threading raw
    /// bytes through them.
    ///
    /// Encodes as `[size = body.len() + 8] [kind] [body]`.
    RawEntry {
        kind: FourCC,
        body: Vec<u8>,
    },
}

impl Codec {
    fn encode_into(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        match self {
            Codec::Mp4a(a) => a.encode(buf),
            Codec::Opus(a) => a.encode(buf),
            Codec::Flac(a) => a.encode(buf),
            Codec::Avc1(a) => a.encode(buf),
            Codec::Avc3(a) => a.encode(buf),
            Codec::Hvc1(a) => a.encode(buf),
            Codec::Hev1(a) => a.encode(buf),
            Codec::Av01(a) => a.encode(buf),
            Codec::Unknown(four) => {
                buf.extend_from_slice(&8u32.to_be_bytes());
                buf.extend_from_slice(four.as_ref());
                Ok(())
            }
            Codec::RawEntry { kind, body } => {
                let total = (body.len() + 8) as u32;
                buf.extend_from_slice(&total.to_be_bytes());
                buf.extend_from_slice(kind.as_ref());
                buf.extend_from_slice(body);
                Ok(())
            }
        }
    }
}

/// Sample Description (`stsd`). FullBox v0 wrapping N codec sample
/// entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stsd {
    pub codecs: Vec<Codec>,
}

impl FullBox for Stsd {
    const KIND: FourCC = FourCC::new(b"stsd");
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        buf.extend_from_slice(&(self.codecs.len() as u32).to_be_bytes());
        for c in &self.codecs {
            c.encode_into(buf)?;
        }
        Ok(())
    }
    fn decode_full_body(_version: u8, _flags: u32, _buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Stsd decode not implemented".into(),
        ))
    }
}

impl Encode for Stsd {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

// ---------------------------------------------------------------------
// Moov containers (Stbl / Minf / Mdia / Trak / Mvex / Moov).
// These are pure container Atoms — their bodies are concatenated
// child boxes in spec order.
// ---------------------------------------------------------------------

/// Sample Table (`stbl`). The leaf sample-table boxes the audio
/// fMP4 producer reaches for: stsd + stts + stsc + stsz + stco.
/// `co64` (64-bit chunk offsets) is not supported by this writer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stbl {
    pub stsd: Stsd,
    pub stts: Stts,
    pub stsc: Stsc,
    pub stsz: Stsz,
    pub stco: Option<Stco>,
}

impl Atom for Stbl {
    const KIND: FourCC = FourCC::new(b"stbl");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        self.stsd.encode(buf)?;
        self.stts.encode(buf)?;
        self.stsc.encode(buf)?;
        self.stsz.encode(buf)?;
        if let Some(stco) = &self.stco {
            stco.encode(buf)?;
        }
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Stbl decode not implemented".into(),
        ))
    }
}

/// Media Information (`minf`). Audio tracks set `smhd`, video tracks
/// set `vmhd`; the two are mutually exclusive in a real track but the
/// struct keeps them as independent options so callers don't reach for
/// an enum to pick one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Minf {
    pub smhd: Option<Smhd>,
    pub vmhd: Option<Vmhd>,
    pub dinf: Dinf,
    pub stbl: Stbl,
}

impl Atom for Minf {
    const KIND: FourCC = FourCC::new(b"minf");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        if let Some(s) = &self.smhd {
            s.encode(buf)?;
        }
        if let Some(v) = &self.vmhd {
            v.encode(buf)?;
        }
        self.dinf.encode(buf)?;
        self.stbl.encode(buf)?;
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Minf decode not implemented".into(),
        ))
    }
}

/// Media (`mdia`). Holds mdhd + hdlr + minf.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mdia {
    pub mdhd: Mdhd,
    pub hdlr: Hdlr,
    pub minf: Minf,
}

impl Atom for Mdia {
    const KIND: FourCC = FourCC::new(b"mdia");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        self.mdhd.encode(buf)?;
        self.hdlr.encode(buf)?;
        self.minf.encode(buf)?;
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Mdia decode not implemented".into(),
        ))
    }
}

/// Track (`trak`). Holds tkhd + optional edts + mdia.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trak {
    pub tkhd: Tkhd,
    pub edts: Option<Edts>,
    pub mdia: Mdia,
}

impl Atom for Trak {
    const KIND: FourCC = FourCC::new(b"trak");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        self.tkhd.encode(buf)?;
        if let Some(e) = &self.edts {
            e.encode(buf)?;
        }
        self.mdia.encode(buf)?;
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Trak decode not implemented".into(),
        ))
    }
}

/// Movie Extends Header (`mehd`). FullBox, v1 when the duration needs
/// 64 bits.
///
/// This is where a FRAGMENTED file declares its total length. The
/// durations in `mvhd` / `tkhd` / `mdhd` describe the samples listed in
/// `moov`, and a fragmented movie lists none there — so for a file whose
/// samples all live in `moof`s, `mehd.fragment_duration` is the only
/// declaration a player can build a timeline from. Optional by spec
/// (ISO/IEC 14496-12 §8.8.2) and genuinely absent from MSE init segments,
/// where the page supplies duration out of band; a standalone file that
/// omits it plays until the reader runs out of fragments and stops,
/// which looks exactly like a truncated encode.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mehd {
    /// Total duration of the fragmented movie, in the `mvhd` timescale.
    pub fragment_duration: u64,
}

impl FullBox for Mehd {
    const KIND: FourCC = FourCC::new(b"mehd");
    /// v0 (32-bit) while it fits, v1 (64-bit) otherwise — the same rule
    /// `Mvhd` uses, and for the same reason: v0 is the shape every
    /// shipping demuxer has read for twenty years.
    fn version(&self) -> u8 {
        u8::from(self.fragment_duration > u32::MAX as u64)
    }
    fn encode_full_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        if self.version() == 1 {
            buf.extend_from_slice(&self.fragment_duration.to_be_bytes());
        } else {
            buf.extend_from_slice(&(self.fragment_duration as u32).to_be_bytes());
        }
        Ok(())
    }
    fn decode_full_body(version: u8, _flags: u32, buf: &[u8]) -> IsobmffResult<Self> {
        let fragment_duration = match version {
            1 => u64::from_be_bytes(
                buf.get(..8)
                    .ok_or_else(|| IsobmffError::Malformed("mehd v1 body short".into()))?
                    .try_into()
                    .expect("8 bytes"),
            ),
            _ => u32::from_be_bytes(
                buf.get(..4)
                    .ok_or_else(|| IsobmffError::Malformed("mehd v0 body short".into()))?
                    .try_into()
                    .expect("4 bytes"),
            ) as u64,
        };
        Ok(Mehd { fragment_duration })
    }
}

impl Encode for Mehd {
    fn encode(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        encode_full(self, buf)
    }
}

/// Movie Extends (`mvex`). Container around trex entries — one per
/// track ID. The audio producer always emits exactly one trex.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mvex {
    /// Total fragmented duration, when the writer knows it. `None` is
    /// the streaming case (MSE, live capture): the length is not known
    /// when the init segment goes out, and the spec allows its absence.
    pub mehd: Option<Mehd>,
    pub trex: Vec<Trex>,
}

impl Atom for Mvex {
    const KIND: FourCC = FourCC::new(b"mvex");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        // mehd precedes every trex — ISO/IEC 14496-12 §8.8.1 orders the
        // container's children and demuxers that stop at the first trex
        // would miss a trailing one.
        if let Some(mehd) = &self.mehd {
            mehd.encode(buf)?;
        }
        for t in &self.trex {
            t.encode(buf)?;
        }
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Mvex decode not implemented".into(),
        ))
    }
}

/// Movie (`moov`). The top-level init segment box.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Moov {
    pub mvhd: Mvhd,
    pub trak: Vec<Trak>,
    pub mvex: Option<Mvex>,
}

impl Atom for Moov {
    const KIND: FourCC = FourCC::new(b"moov");
    fn encode_body(&self, buf: &mut Vec<u8>) -> IsobmffResult<()> {
        self.mvhd.encode(buf)?;
        for t in &self.trak {
            t.encode(buf)?;
        }
        if let Some(mvex) = &self.mvex {
            mvex.encode(buf)?;
        }
        Ok(())
    }
    fn decode_body(_buf: &[u8]) -> IsobmffResult<Self> {
        Err(IsobmffError::Malformed(
            "Moov decode not implemented".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bit-exact compatibility check against `mp4_atom` for each
    /// foundation box. The dev-only mp4-atom oracle pins equivalence
    /// against the same supported shapes.
    use mp4_atom as oracle;

    #[test]
    fn ftyp_matches_mp4_atom() {
        let native = Ftyp {
            major_brand: b"avif".into(),
            minor_version: 0,
            compatible_brands: vec![b"avif".into(), b"mif1".into(), b"miaf".into()],
        };
        let oracle_val = oracle::Ftyp {
            major_brand: b"avif".into(),
            minor_version: 0,
            compatible_brands: vec![b"avif".into(), b"mif1".into(), b"miaf".into()],
        };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        assert_eq!(native_buf, oracle_buf, "ftyp wire mismatch");
    }

    #[test]
    fn styp_matches_mp4_atom() {
        let native = Styp {
            major_brand: b"msdh".into(),
            minor_version: 0,
            compatible_brands: vec![b"msdh".into(), b"msix".into()],
        };
        let oracle_val = oracle::Styp {
            major_brand: b"msdh".into(),
            minor_version: 0,
            compatible_brands: vec![b"msdh".into(), b"msix".into()],
        };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        assert_eq!(native_buf, oracle_buf, "styp wire mismatch");
    }

    #[test]
    fn mdat_matches_mp4_atom() {
        let payload = b"abcdefghijklmnopqrstuvwxyz".to_vec();
        let native = Mdat {
            data: payload.clone(),
        };
        let oracle_val = oracle::Mdat { data: payload };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        assert_eq!(native_buf, oracle_buf, "mdat wire mismatch");
    }

    #[test]
    fn mfhd_matches_mp4_atom() {
        let native = Mfhd { sequence_number: 7 };
        let oracle_val = oracle::Mfhd { sequence_number: 7 };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        assert_eq!(native_buf, oracle_buf, "mfhd wire mismatch");
    }

    #[test]
    fn ftyp_round_trips() {
        let original = Ftyp {
            major_brand: b"isom".into(),
            minor_version: 0,
            compatible_brands: vec![b"isom".into(), b"avif".into()],
        };
        let mut buf = Vec::new();
        original.encode(&mut buf).unwrap();
        // Skip the 4-byte size + 4-byte type to feed decode_body the
        // body slice directly.
        let decoded = Ftyp::decode_body(&buf[8..]).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn mfhd_round_trips() {
        let original = Mfhd {
            sequence_number: 42,
        };
        let mut buf = Vec::new();
        original.encode(&mut buf).unwrap();
        // FullBox layout: size(4) + type(4) + version(1) + flags(3) +
        // body. decode_full_body wants the body only.
        let decoded = Mfhd::decode_full_body(0, 0, &buf[12..]).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn u24_round_trips() {
        let v = u24::try_from(0x123456u32).unwrap();
        let back: u32 = v.into();
        assert_eq!(back, 0x123456u32);
    }

    #[test]
    fn u24_rejects_overflow() {
        assert!(u24::try_from(0x0100_0000u32).is_err());
    }

    #[test]
    fn tfhd_matches_mp4_atom_with_default_base_is_moof_baked_in() {
        // The native writer always sets default_base_is_moof (0x020000).
        // To get the same bytes from mp4-atom we apply
        // `patch_tfhd_default_base_is_moof` to the oracle output only.
        let native = Tfhd {
            track_id: 1,
            base_data_offset: None,
            sample_description_index: Some(1),
            default_sample_duration: Some(1024),
            default_sample_size: None,
            default_sample_flags: None,
        };
        let oracle_val = oracle::Tfhd {
            track_id: 1,
            base_data_offset: None,
            sample_description_index: Some(1),
            default_sample_duration: Some(1024),
            default_sample_size: None,
            default_sample_flags: None,
        };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        crate::patches::patch_tfhd_default_base_is_moof(&mut oracle_buf);
        assert_eq!(
            native_buf, oracle_buf,
            "tfhd wire mismatch (with default_base_is_moof baked in)"
        );
    }

    #[test]
    fn tfdt_matches_mp4_atom() {
        let native = Tfdt {
            base_media_decode_time: 48_000,
        };
        let oracle_val = oracle::Tfdt {
            base_media_decode_time: 48_000,
        };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        assert_eq!(native_buf, oracle_buf, "tfdt wire mismatch");
    }

    /// Native v0 encoding (no negative cts) decodes back to the same
    /// struct. Used to be a byte-compare against mp4-atom's v1 output,
    /// but our writer now adapts to v0 when cts ≥ 0 — matches `ffmpeg
    /// -movflags +frag_keyframe`'s default and is more compatible
    /// with older MSE implementations. Round-tripping proves the
    /// encoder + decoder agree on the v0 wire shape.
    #[test]
    fn trun_v0_round_trip_single_sample() {
        let entries = vec![TrunEntry {
            duration: Some(1024),
            size: Some(256),
            flags: None,
            cts: None,
        }];
        let native = Trun {
            data_offset: Some(120),
            entries: entries.clone(),
        };
        let mut buf = Vec::new();
        native.encode(&mut buf).unwrap();
        // Version byte sits right after the 8-byte box header.
        assert_eq!(buf[8], 0, "expected trun v0 when cts absent");
        // Skip the box header + decode the FullBox body.
        let body = &buf[8 + 4..];
        let decoded = Trun::decode_full_body(0, native.flags(), body).unwrap();
        assert_eq!(decoded.data_offset, Some(120));
        assert_eq!(decoded.entries, entries);
    }

    #[test]
    fn trun_v0_round_trip_multiple_samples() {
        let entries = vec![
            TrunEntry {
                duration: Some(1024),
                size: Some(100),
                flags: None,
                cts: None,
            },
            TrunEntry {
                duration: Some(1024),
                size: Some(150),
                flags: None,
                cts: None,
            },
            TrunEntry {
                duration: Some(2048),
                size: Some(200),
                flags: None,
                cts: None,
            },
        ];
        let native = Trun {
            data_offset: Some(200),
            entries: entries.clone(),
        };
        let mut buf = Vec::new();
        native.encode(&mut buf).unwrap();
        assert_eq!(buf[8], 0, "expected trun v0 when cts absent");
        let body = &buf[8 + 4..];
        let decoded = Trun::decode_full_body(0, native.flags(), body).unwrap();
        assert_eq!(decoded.data_offset, Some(200));
        assert_eq!(decoded.entries, entries);
    }

    /// When *any* sample carries a negative cts, we MUST emit v1 (v0
    /// is unsigned). Locks in the version selector against future
    /// regressions.
    #[test]
    fn trun_v1_when_negative_cts_present() {
        let native = Trun {
            data_offset: Some(120),
            entries: vec![
                TrunEntry {
                    duration: Some(1024),
                    size: Some(256),
                    flags: None,
                    cts: Some(0),
                },
                TrunEntry {
                    duration: Some(1024),
                    size: Some(256),
                    flags: None,
                    cts: Some(-3750),
                },
            ],
        };
        let mut buf = Vec::new();
        native.encode(&mut buf).unwrap();
        assert_eq!(buf[8], 1, "expected trun v1 when any cts < 0");
        let body = &buf[8 + 4..];
        let decoded = Trun::decode_full_body(1, native.flags(), body).unwrap();
        assert_eq!(decoded.entries[1].cts, Some(-3750));
    }

    /// Native moof shape sanity check. Trun selects v0 / v1 based on
    /// cts presence, unlike the mp4-atom oracle's v1-always default;
    /// this contract checks box shape rather than byte equality.
    #[test]
    fn moof_emits_expected_box_shape() {
        let native = Moof {
            mfhd: Mfhd { sequence_number: 7 },
            traf: vec![Traf {
                tfhd: Tfhd {
                    track_id: 1,
                    base_data_offset: None,
                    sample_description_index: Some(1),
                    default_sample_duration: Some(1024),
                    default_sample_size: None,
                    default_sample_flags: None,
                },
                tfdt: Some(Tfdt {
                    base_media_decode_time: 0,
                }),
                trun: vec![Trun {
                    data_offset: Some(120),
                    entries: vec![TrunEntry {
                        duration: Some(1024),
                        size: Some(256),
                        flags: None,
                        cts: None,
                    }],
                }],
            }],
        };
        let mut buf = Vec::new();
        native.encode(&mut buf).unwrap();
        // Expected child fourccs, in order: mfhd, then a single traf
        // containing tfhd, tfdt, trun.
        assert_eq!(&buf[4..8], b"moof");
        assert_eq!(&buf[12..16], b"mfhd");
        // Find the traf and assert its children.
        let traf_pos = buf.windows(4).position(|w| w == b"traf").expect("traf");
        let traf_body_start = traf_pos + 4;
        let traf_body = &buf[traf_body_start..];
        let tfhd_off = traf_body.windows(4).position(|w| w == b"tfhd").unwrap();
        let tfdt_off = traf_body.windows(4).position(|w| w == b"tfdt").unwrap();
        let trun_off = traf_body.windows(4).position(|w| w == b"trun").unwrap();
        assert!(
            tfhd_off < tfdt_off && tfdt_off < trun_off,
            "traf child order"
        );
        // tfhd flags should carry default_base_is_moof (0x020000).
        let tfhd_flags_off = traf_body_start + tfhd_off + 4 /* fourcc */
            + 1 /* version */;
        let flags = u32::from_be_bytes([
            0,
            buf[tfhd_flags_off],
            buf[tfhd_flags_off + 1],
            buf[tfhd_flags_off + 2],
        ]);
        assert_eq!(
            flags & 0x020000,
            0x020000,
            "default_base_is_moof must be set"
        );
    }

    // ----- Init-segment box tests --------------------------
    // Used to be bit-exact-vs-mp4-atom oracle tests; replaced with
    // version-selector + flags + shape checks once we diverged from
    // mp4-atom's "always v1" default for headers (we now emit v0
    // when fields fit, matching `ffmpeg -c copy` output).

    #[test]
    fn mvhd_emits_v0_when_fields_fit() {
        let native = Mvhd {
            creation_time: 100,
            modification_time: 200,
            timescale: 48000,
            duration: 12345,
            rate: FixedPoint::new(1u16, 0),
            volume: FixedPoint::new(1u8, 0),
            matrix: Matrix::default(),
            next_track_id: 2,
        };
        let mut buf = Vec::new();
        native.encode(&mut buf).unwrap();
        assert_eq!(&buf[4..8], b"mvhd");
        // ver+flags is 4 bytes after fourcc.
        assert_eq!(buf[8], 0, "mvhd v0 expected when ts ≤ u32::MAX");
        // v0 body: ct(4) + mt(4) + ts(4) + dur(4) — 100/200/48000/12345.
        let body_start = 12; // 8B header + 4B ver/flags
        assert_eq!(
            u32::from_be_bytes(buf[body_start..body_start + 4].try_into().unwrap()),
            100
        );
        assert_eq!(
            u32::from_be_bytes(buf[body_start + 4..body_start + 8].try_into().unwrap()),
            200
        );
        assert_eq!(
            u32::from_be_bytes(buf[body_start + 8..body_start + 12].try_into().unwrap()),
            48000
        );
        assert_eq!(
            u32::from_be_bytes(buf[body_start + 12..body_start + 16].try_into().unwrap()),
            12345
        );
    }

    #[test]
    fn mvhd_emits_v1_when_duration_exceeds_u32() {
        let native = Mvhd {
            creation_time: 0,
            modification_time: 0,
            timescale: 48000,
            duration: u32::MAX as u64 + 1,
            rate: FixedPoint::new(1u16, 0),
            volume: FixedPoint::new(1u8, 0),
            matrix: Matrix::default(),
            next_track_id: 2,
        };
        let mut buf = Vec::new();
        native.encode(&mut buf).unwrap();
        assert_eq!(buf[8], 1, "mvhd v1 expected when duration > u32::MAX");
    }

    #[test]
    fn tkhd_emits_v0_when_fields_fit_with_flags_baked_in() {
        let native = Tkhd {
            creation_time: 0,
            modification_time: 0,
            track_id: 1,
            duration: 12345,
            layer: 0,
            alternate_group: 0,
            volume: FixedPoint::new(1u8, 0),
            matrix: Matrix::default(),
            width: FixedPoint::default(),
            height: FixedPoint::default(),
        };
        let mut buf = Vec::new();
        native.encode(&mut buf).unwrap();
        assert_eq!(&buf[4..8], b"tkhd");
        assert_eq!(buf[8], 0, "tkhd v0 expected when fields fit");
        // track_enabled (bit 0) + track_in_movie (bit 1) = 0x000003.
        let flags = u32::from_be_bytes([0, buf[9], buf[10], buf[11]]);
        assert_eq!(
            flags & 0x000003,
            0x000003,
            "tkhd flags must include enabled+in_movie"
        );
    }

    #[test]
    fn trex_matches_mp4_atom() {
        let native = Trex {
            track_id: 1,
            default_sample_description_index: 1,
            default_sample_duration: 1024,
            default_sample_size: 0,
            default_sample_flags: 0x010_000,
        };
        let oracle_val = oracle::Trex {
            track_id: 1,
            default_sample_description_index: 1,
            default_sample_duration: 1024,
            default_sample_size: 0,
            default_sample_flags: 0x010_000,
        };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        assert_eq!(native_buf, oracle_buf, "trex wire mismatch");
    }

    #[test]
    fn elst_matches_mp4_atom() {
        let native = Elst {
            entries: vec![ElstEntry {
                segment_duration: 12345,
                media_time: 0,
                media_rate: 1,
                media_rate_fraction: 0,
            }],
        };
        let oracle_val = oracle::Elst {
            entries: vec![oracle::ElstEntry {
                segment_duration: 12345,
                media_time: 0,
                media_rate: 1,
                media_rate_fraction: 0,
            }],
        };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        assert_eq!(native_buf, oracle_buf, "elst wire mismatch");
    }

    #[test]
    fn mdhd_emits_v0_when_fields_fit() {
        let native = Mdhd {
            creation_time: 100,
            modification_time: 200,
            timescale: 48000,
            duration: 30439936,
            language: "und".to_string(),
        };
        let mut buf = Vec::new();
        native.encode(&mut buf).unwrap();
        assert_eq!(&buf[4..8], b"mdhd");
        assert_eq!(buf[8], 0, "mdhd v0 expected when fields fit");
        let body_start = 12;
        assert_eq!(
            u32::from_be_bytes(buf[body_start..body_start + 4].try_into().unwrap()),
            100
        );
        assert_eq!(
            u32::from_be_bytes(buf[body_start + 4..body_start + 8].try_into().unwrap()),
            200
        );
        assert_eq!(
            u32::from_be_bytes(buf[body_start + 8..body_start + 12].try_into().unwrap()),
            48000
        );
        assert_eq!(
            u32::from_be_bytes(buf[body_start + 12..body_start + 16].try_into().unwrap()),
            30439936
        );
    }

    #[test]
    fn hdlr_matches_mp4_atom() {
        let native = Hdlr {
            handler: FourCC::new(b"soun"),
            name: "SoundHandler".to_string(),
        };
        let oracle_val = oracle::Hdlr {
            handler: b"soun".into(),
            name: "SoundHandler".to_string(),
        };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        assert_eq!(native_buf, oracle_buf, "hdlr wire mismatch");
    }

    #[test]
    fn smhd_matches_mp4_atom() {
        let native = Smhd {
            balance: FixedPoint::new(0i8, 0),
        };
        let oracle_val = oracle::Smhd {
            balance: oracle::FixedPoint::new(0i8, 0),
        };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        assert_eq!(native_buf, oracle_buf, "smhd wire mismatch");
    }

    #[test]
    fn dinf_matches_mp4_atom_with_url_self_contained_baked_in() {
        let native = Dinf {
            dref: Dref {
                urls: vec![Url {
                    location: String::new(),
                }],
            },
        };
        let oracle_val = oracle::Dinf {
            dref: oracle::Dref {
                urls: vec![oracle::Url {
                    location: String::new(),
                }],
            },
        };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        crate::patches::patch_dref_url_self_contained(&mut oracle_buf);
        assert_eq!(
            native_buf, oracle_buf,
            "dinf+dref+url wire mismatch (with self_contained baked in)"
        );
    }

    #[test]
    fn stts_matches_mp4_atom() {
        let native = Stts {
            entries: vec![SttsEntry {
                sample_count: 100,
                sample_delta: 1024,
            }],
        };
        let oracle_val = oracle::Stts {
            entries: vec![oracle::SttsEntry {
                sample_count: 100,
                sample_delta: 1024,
            }],
        };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        assert_eq!(native_buf, oracle_buf, "stts wire mismatch");
    }

    #[test]
    fn stsc_matches_mp4_atom() {
        let native = Stsc {
            entries: vec![StscEntry {
                first_chunk: 1,
                samples_per_chunk: 1,
                sample_description_index: 1,
            }],
        };
        let oracle_val = oracle::Stsc {
            entries: vec![oracle::StscEntry {
                first_chunk: 1,
                samples_per_chunk: 1,
                sample_description_index: 1,
            }],
        };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        assert_eq!(native_buf, oracle_buf, "stsc wire mismatch");
    }

    #[test]
    fn stsz_matches_mp4_atom_identical_and_different() {
        // Identical
        let native = Stsz {
            samples: StszSamples::Identical {
                count: 4,
                size: 1024,
            },
        };
        let oracle_val = oracle::Stsz {
            samples: oracle::StszSamples::Identical {
                count: 4,
                size: 1024,
            },
        };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        assert_eq!(native_buf, oracle_buf, "stsz identical wire mismatch");

        // Different
        let native = Stsz {
            samples: StszSamples::Different {
                sizes: vec![100, 200, 300],
            },
        };
        let oracle_val = oracle::Stsz {
            samples: oracle::StszSamples::Different {
                sizes: vec![100, 200, 300],
            },
        };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        assert_eq!(native_buf, oracle_buf, "stsz different wire mismatch");
    }

    #[test]
    fn stco_matches_mp4_atom() {
        let native = Stco {
            entries: vec![100, 200, 300],
        };
        let oracle_val = oracle::Stco {
            entries: vec![100, 200, 300],
        };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        assert_eq!(native_buf, oracle_buf, "stco wire mismatch");
    }

    // ----- Codec sample entry tests --------------------------

    #[test]
    fn btrt_matches_mp4_atom() {
        let native = Btrt {
            buffer_size_db: 1024,
            max_bitrate: 192_000,
            avg_bitrate: 128_000,
        };
        let oracle_val = oracle::Btrt {
            buffer_size_db: 1024,
            max_bitrate: 192_000,
            avg_bitrate: 128_000,
        };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        assert_eq!(native_buf, oracle_buf, "btrt wire mismatch");
    }

    #[test]
    fn opus_matches_mp4_atom() {
        let native = Opus {
            audio: Audio {
                data_reference_index: 1,
                channel_count: 2,
                sample_size: 16,
                sample_rate: FixedPoint::new(48000u16, 0),
            },
            dops: Dops {
                output_channel_count: 2,
                pre_skip: 312,
                input_sample_rate: 48000,
                output_gain: 0,
            },
            btrt: None,
        };
        let oracle_val = oracle::Opus {
            audio: oracle::Audio {
                data_reference_index: 1,
                channel_count: 2,
                sample_size: 16,
                sample_rate: oracle::FixedPoint::new(48000u16, 0),
            },
            dops: oracle::Dops {
                output_channel_count: 2,
                pre_skip: 312,
                input_sample_rate: 48000,
                output_gain: 0,
            },
            btrt: None,
        };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        assert_eq!(native_buf, oracle_buf, "Opus wire mismatch");
    }

    #[test]
    fn flac_matches_mp4_atom_streaminfo_only() {
        let md5: Vec<u8> = vec![0u8; 16];
        let native = Flac {
            audio: Audio {
                data_reference_index: 1,
                channel_count: 2,
                sample_size: 16,
                sample_rate: FixedPoint::new(44100u16, 0),
            },
            dfla: Dfla {
                blocks: vec![FlacMetadataBlock::StreamInfo {
                    minimum_block_size: 4608,
                    maximum_block_size: 4608,
                    minimum_frame_size: 16,
                    maximum_frame_size: 9102,
                    sample_rate: 44100,
                    num_channels_minus_one: 1,
                    bits_per_sample_minus_one: 15,
                    number_of_interchannel_samples: 120832,
                    md5_checksum: md5.clone(),
                }],
            },
        };
        let oracle_val = oracle::Flac {
            audio: oracle::Audio {
                data_reference_index: 1,
                channel_count: 2,
                sample_size: 16,
                sample_rate: oracle::FixedPoint::new(44100u16, 0),
            },
            dfla: oracle::Dfla {
                blocks: vec![oracle::FlacMetadataBlock::StreamInfo {
                    minimum_block_size: 4608,
                    maximum_block_size: 4608,
                    minimum_frame_size: 16u32.try_into().unwrap(),
                    maximum_frame_size: 9102u32.try_into().unwrap(),
                    sample_rate: 44100,
                    num_channels_minus_one: 1,
                    bits_per_sample_minus_one: 15,
                    number_of_interchannel_samples: 120832,
                    md5_checksum: md5,
                }],
            },
        };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        let mut oracle_buf = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_buf).unwrap();
        assert_eq!(native_buf, oracle_buf, "Flac wire mismatch");
    }

    #[test]
    fn esds_matches_mp4_atom_with_expandable_lengths_baked_in() {
        // 2-byte AudioSpecificConfig — the most common AAC case.
        let asc = &[0x12, 0x10];
        let native = Esds {
            es_desc: esds::EsDescriptor {
                es_id: 1,
                dec_config: esds::DecoderConfig {
                    object_type_indication: 0x40,
                    stream_type: 0x05,
                    up_stream: 0,
                    buffer_size_db: 0,
                    max_bitrate: 0,
                    avg_bitrate: 0,
                    dec_specific: esds::DecoderSpecific {
                        // Match what (profile<<3 | freq_index>>1, freq_index<<7 | chan_conf<<3)
                        // produces for asc = [0x12, 0x10]:
                        //   byte 0 = 0x12 = 0001_0010 → profile=2, freq_index_hi=2
                        //   byte 1 = 0x10 = 0001_0000 → freq_index_lo=0 → freq_index=4,
                        //                                chan_conf=2
                        profile: 2,
                        freq_index: 4,
                        chan_conf: 2,
                    },
                },
                sl_config: esds::SLConfig,
            },
        };
        let oracle_val = oracle::Esds {
            es_desc: oracle::esds::EsDescriptor {
                es_id: 1,
                dec_config: oracle::esds::DecoderConfig {
                    object_type_indication: 0x40,
                    stream_type: 0x05,
                    up_stream: 0,
                    buffer_size_db: Default::default(),
                    max_bitrate: 0,
                    avg_bitrate: 0,
                    dec_specific: oracle::esds::DecoderSpecific {
                        profile: 2,
                        freq_index: 4,
                        chan_conf: 2,
                    },
                },
                sl_config: oracle::esds::SLConfig::default(),
            },
        };
        let mut native_buf = Vec::new();
        native.encode(&mut native_buf).unwrap();
        // Build mp4-atom's compact-length esds, then have patches.rs
        // rewrite it to the expandable form. Compare against the
        // native (which emits expandable directly).
        //
        // patches::patch_esds_descriptor_lengths replaces the whole
        // esds box wholesale and walks ancestor box sizes; with no
        // ancestors here it's a pure replacement.
        let mut oracle_compact = Vec::new();
        oracle::Encode::encode(&oracle_val, &mut oracle_compact).unwrap();
        let mut oracle_expandable = oracle_compact;
        crate::patches::patch_esds_descriptor_lengths(&mut oracle_expandable, asc);
        assert_eq!(
            native_buf, oracle_expandable,
            "esds wire mismatch (with expandable lengths baked in)"
        );
    }

    #[test]
    fn stsd_with_unknown_alac_encodes_as_size_plus_fourcc() {
        let native = Stsd {
            codecs: vec![Codec::Unknown(FourCC::new(b"alac"))],
        };
        let mut buf = Vec::new();
        native.encode(&mut buf).unwrap();
        // Layout: stsd box(4 size + 4 type + 4 fullbox) + u32 count + child(8 size+4cc).
        // Box header: stsd starts at 0.
        assert_eq!(&buf[..4], &24u32.to_be_bytes()); // total stsd = 24
        assert_eq!(&buf[4..8], b"stsd");
        assert_eq!(&buf[8..12], &[0, 0, 0, 0]); // version+flags
        assert_eq!(&buf[12..16], &1u32.to_be_bytes()); // count
        assert_eq!(&buf[16..20], &8u32.to_be_bytes()); // child size
        assert_eq!(&buf[20..24], b"alac");
    }

    #[test]
    fn fourcc_round_trips_through_u32() {
        let cc = FourCC::new(b"hvc1");
        let n: u32 = cc.into();
        let back = FourCC::from(n);
        assert_eq!(cc, back);
    }

    #[test]
    fn avcc_wraps_configuration_record_as_avcc_box() {
        // Minimal but real avcC payload: version=1, profile=42 (Main),
        // compat=0, level=30, lengthSizeMinusOne=3 (4-byte NAL prefix),
        // numSPS=1, SPS length=0, numPPS=1, PPS length=0.
        let cfg = vec![1u8, 42, 0, 30, 0xFF, 0xE1, 0, 0, 0x01, 0, 0];
        let native = AvcC {
            configuration_record: cfg.clone(),
        };
        let mut buf = Vec::new();
        native.encode(&mut buf).unwrap();
        // 8-byte header + 11 bytes payload = 19 total.
        assert_eq!(&buf[..4], &19u32.to_be_bytes());
        assert_eq!(&buf[4..8], b"avcC");
        assert_eq!(&buf[8..], &cfg[..]);
    }

    #[test]
    fn avc1_carries_visual_header_then_avcc_then_optional_btrt() {
        let cfg = vec![1u8, 42, 0, 30, 0xFF, 0xE1, 0, 0, 0x01, 0, 0];
        let native = Codec::Avc1(Avc1 {
            visual: Visual {
                data_reference_index: 1,
                width: 1920,
                height: 1080,
            },
            avcc: AvcC {
                configuration_record: cfg.clone(),
            },
            pasp: None,
            btrt: None,
        });
        let stsd = Stsd {
            codecs: vec![native],
        };
        let mut buf = Vec::new();
        stsd.encode(&mut buf).unwrap();
        // stsd box header + version/flags + count + avc1 entry.
        assert_eq!(&buf[4..8], b"stsd");
        // The child sample entry's 4cc starts at offset 20:
        // [stsd size(4) + 'stsd'(4) + ver+flags(4) + count(4)] = 16,
        // then the child box header [size(4) + 4cc(4)] starts at 16,
        // so 'avc1' lands at offset 20.
        assert_eq!(&buf[20..24], b"avc1");
        // Visual header (78 bytes) immediately follows the 8-byte box
        // header, ending at offset 24 + 78 = 102. avcC starts there.
        assert_eq!(&buf[102..106], &19u32.to_be_bytes()); // avcC size
        assert_eq!(&buf[106..110], b"avcC");
        assert_eq!(&buf[110..121], &cfg[..]);
        // Verify width/height land at the right offsets inside Visual.
        // SampleEntry reserved(6) + data_reference_index(2) = 8 bytes
        // of base, then VisualSampleEntry pre_defined(2) + reserved(2)
        // + pre_defined[3](12) = 16, so width starts at 24 + 8 + 16 = 48.
        assert_eq!(&buf[48..50], &1920u16.to_be_bytes());
        assert_eq!(&buf[50..52], &1080u16.to_be_bytes());
    }
}
