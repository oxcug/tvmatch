//! HEIF box subset (ISO/IEC 23008-12) — native walker, no mp4-atom.
//!
//! HEIF reuses the ISOBMFF container family but with a different box
//! hierarchy than MP4: image data lives under `meta`, addressed by item
//! IDs in `iloc` and `iinf`, with properties attached via `iprp`/`ipco`/`ipma`
//! and derived images (grid/iovl/iden) chained through `iref`.
//!
//! [`read_heif`] returns a structured [`HeifFile`] view. Callers feed
//! it a borrowed `&[u8]` of the whole file and pull out the bits they
//! need (primary item bitstream via `iloc` + property table via `ipma`).
//!
//! Box reference (Nokia HEIF tech summary, ISO/IEC 23008-12):
//!
//! | Box | Purpose |
//! |---|---|
//! | `ftyp` | File brand: `mif1` (image), `msf1` (sequence), `heic`/`heix` (HEVC), `avif` (AV1) |
//! | `meta` | Container for image items |
//! | `pitm` | Primary item ID |
//! | `iloc` | Item byte locations in `mdat` |
//! | `iinf` | Item types (4CC: `hvc1`, `av01`, `grid`, `iovl`, `iden`) |
//! | `iref` | Inter-item refs: `dimg` (derived inputs), `auxl` (auxiliary), `thmb` (thumbnail) |
//! | `iprp` → `ipco` / `ipma` | Property table + per-item associations |
//! | `mdat` | Raw coded bitstream payload |

use crate::{IsobmffError, IsobmffResult};

/// Result of parsing a HEIF/AVIF file's top-level structure.
#[derive(Debug, Clone)]
pub struct HeifFile<'a> {
    pub major_brand: [u8; 4],
    pub compatible_brands: Vec<[u8; 4]>,
    pub meta: HeifMeta,
    /// Byte slice into the source for the primary `mdat`. iloc extents
    /// (construction_method = 0) are file-relative; we translate via
    /// `mdat_offset`.
    pub mdat: &'a [u8],
    pub mdat_offset: u64,
    /// `idat` box body bytes — small inline-data area used by
    /// construction_method = 1 items (HEIC grid descriptors, etc.).
    /// `None` if the file doesn't carry one.
    pub idat: Option<&'a [u8]>,
}

#[derive(Debug, Clone, Default)]
pub struct HeifMeta {
    pub primary_item: u32,
    pub items: Vec<ItemInfo>,
    pub locations: Vec<ItemLocation>,
    pub references: Vec<ItemReference>,
    pub properties: PropertyTable,
}

#[derive(Debug, Clone)]
pub struct ItemInfo {
    pub id: u32,
    pub item_type: [u8; 4],
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ItemLocation {
    pub id: u32,
    /// 0 = file-relative (mdat), 1 = idat-relative, 2 = item-relative.
    pub construction_method: u8,
    pub base_offset: u64,
    pub extents: Vec<ItemExtent>,
}

#[derive(Debug, Clone, Copy)]
pub struct ItemExtent {
    pub offset: u64,
    pub length: u64,
}

#[derive(Debug, Clone)]
pub struct ItemReference {
    pub from: u32,
    pub ref_type: [u8; 4],
    pub to: Vec<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct PropertyTable {
    /// `ipco` children in order. 1-based `property_index` in `ipma`
    /// refers into this vec (index 0 → "none", so callers subtract 1).
    pub properties: Vec<Property>,
    /// `ipma` rows: `(item_id, [(property_index, essential), ...])`.
    pub associations: Vec<(u32, Vec<(u16, bool)>)>,
}

#[derive(Debug, Clone)]
pub enum Property {
    Ispe {
        width: u32,
        height: u32,
    },
    Pixi {
        channel_depths: Vec<u8>,
    },
    Colr(ColrPayload),
    Pasp {
        h: u32,
        v: u32,
    },
    AuxC {
        aux_type: String,
    },
    Irot {
        /// 0..=3 quarter-turns counter-clockwise.
        angle_quarter_turns: u8,
    },
    Imir {
        axis: MirrorAxis,
    },
    Clap,
    /// Raw box body for variants we don't model — includes the
    /// codec-config records (`hvcC`, `av1C`) that consumers re-parse
    /// in their own crates.
    Other {
        four_cc: [u8; 4],
        data: Vec<u8>,
    },
}

#[derive(Debug, Clone)]
pub enum ColrPayload {
    /// ICC v4 profile blob (`colour_type` = `prof` or `rICC`).
    Icc(Vec<u8>),
    Nclx {
        primaries: u16,
        transfer: u16,
        matrix: u16,
        full_range: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorAxis {
    /// `imir.axis = 0` — flip about the horizontal axis (vertical mirror).
    Vertical,
    /// `imir.axis = 1` — flip about the vertical axis (horizontal mirror).
    Horizontal,
}

impl HeifMeta {
    pub fn item(&self, id: u32) -> Option<&ItemInfo> {
        self.items.iter().find(|i| i.id == id)
    }
    pub fn location(&self, id: u32) -> Option<&ItemLocation> {
        self.locations.iter().find(|l| l.id == id)
    }
    /// Resolve `ipma` associations for `id` into a list of property
    /// refs. Order matches the on-wire association list, which the
    /// spec says is significant for transformative props (e.g. clap
    /// before irot).
    pub fn properties_for(&self, id: u32) -> Vec<&Property> {
        let Some((_, assoc)) = self.properties.associations.iter().find(|(i, _)| *i == id) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(assoc.len());
        for (idx, _essential) in assoc {
            let i = *idx as usize;
            if i == 0 || i > self.properties.properties.len() {
                continue;
            }
            out.push(&self.properties.properties[i - 1]);
        }
        out
    }
}

impl<'a> HeifFile<'a> {
    /// Slice the bytes for a specific [`ItemLocation`] extent list.
    /// Supports `construction_method = 0` (mdat-relative, file offset
    /// space — the common case) and `1` (idat-relative — used by
    /// HEIC `grid`/`iden` descriptors). Method `2` (item-relative)
    /// is rare and not yet implemented.
    pub fn read_item_bytes(&self, loc: &ItemLocation) -> IsobmffResult<Vec<u8>> {
        match loc.construction_method {
            0 => self.read_extents_from_mdat(loc),
            1 => self.read_extents_from_idat(loc),
            other => Err(IsobmffError::Heif(format!(
                "iloc construction_method = {other} (item-relative not supported)"
            ))),
        }
    }

    fn read_extents_from_mdat(&self, loc: &ItemLocation) -> IsobmffResult<Vec<u8>> {
        let mut out = Vec::new();
        for ext in &loc.extents {
            let abs_offset = loc
                .base_offset
                .checked_add(ext.offset)
                .ok_or_else(|| IsobmffError::Heif("iloc extent offset overflow".into()))?;
            let abs_end = abs_offset
                .checked_add(ext.length)
                .ok_or_else(|| IsobmffError::Heif("iloc extent length overflow".into()))?;
            let mdat_end = self.mdat_offset + self.mdat.len() as u64;
            if abs_offset < self.mdat_offset || abs_end > mdat_end {
                return Err(IsobmffError::Heif(format!(
                    "iloc extent [{abs_offset}..{abs_end}) outside mdat \
                     [{}..{mdat_end})",
                    self.mdat_offset
                )));
            }
            let rel_start = (abs_offset - self.mdat_offset) as usize;
            let rel_end = (abs_end - self.mdat_offset) as usize;
            out.extend_from_slice(&self.mdat[rel_start..rel_end]);
        }
        Ok(out)
    }

    fn read_extents_from_idat(&self, loc: &ItemLocation) -> IsobmffResult<Vec<u8>> {
        let idat = self
            .idat
            .ok_or_else(|| IsobmffError::Heif("iloc method=1 but no idat box".into()))?;
        let mut out = Vec::new();
        for ext in &loc.extents {
            // base_offset is idat-relative for method=1 (not file-relative).
            let start = loc
                .base_offset
                .checked_add(ext.offset)
                .ok_or_else(|| IsobmffError::Heif("idat extent offset overflow".into()))?
                as usize;
            let end = start + ext.length as usize;
            if end > idat.len() {
                return Err(IsobmffError::Heif(format!(
                    "idat extent [{start}..{end}) outside idat (len {})",
                    idat.len()
                )));
            }
            out.extend_from_slice(&idat[start..end]);
        }
        Ok(out)
    }
}

/// Parsed HEIF `grid` item descriptor (ISO/IEC 23008-12 §6.6.2.3).
///
/// The grid's `dimg` reference list (look it up in
/// [`HeifMeta::references`] with `ref_type = b"dimg"`) gives the tile
/// item IDs in row-major order. Tile dimensions come from each
/// tile's `ispe` property — the `Grid::output_width/height` are the
/// final composed canvas size after any conformance-window crop.
#[derive(Debug, Clone)]
pub struct Grid {
    pub rows: u32,
    pub columns: u32,
    pub output_width: u32,
    pub output_height: u32,
}

/// Parse a `grid` item's descriptor bytes (typically pulled via
/// [`HeifFile::read_item_bytes`]).
pub fn parse_grid_descriptor(bytes: &[u8]) -> IsobmffResult<Grid> {
    if bytes.len() < 8 {
        return Err(IsobmffError::Heif("grid descriptor < 8 bytes".into()));
    }
    let _version = bytes[0];
    let flags = bytes[1];
    let rows = bytes[2] as u32 + 1;
    let columns = bytes[3] as u32 + 1;
    let large_dim = (flags & 1) != 0;
    let (w, h) = if large_dim {
        if bytes.len() < 12 {
            return Err(IsobmffError::Heif(
                "grid descriptor < 12 bytes with large flag".into(),
            ));
        }
        (
            u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
        )
    } else {
        (
            u16::from_be_bytes([bytes[4], bytes[5]]) as u32,
            u16::from_be_bytes([bytes[6], bytes[7]]) as u32,
        )
    };
    Ok(Grid {
        rows,
        columns,
        output_width: w,
        output_height: h,
    })
}

// ---------------------------------------------------------------------
// Walker
// ---------------------------------------------------------------------

/// Top-level walk of a HEIF/AVIF byte slice. Reads `ftyp`, `meta`, and
/// records `mdat` as a borrowed slice. Other top-level boxes (`free`,
/// `mdat` placeholders before the real one, vendor proprietary) are
/// skipped.
pub fn read_heif(bytes: &[u8]) -> IsobmffResult<HeifFile<'_>> {
    let mut cur = Cursor::new(bytes);
    let mut major_brand = [0u8; 4];
    let mut compatible_brands = Vec::new();
    let mut meta_with_idat: Option<(HeifMeta, Option<&[u8]>)> = None;
    let mut mdat: Option<(u64, &[u8])> = None;
    let mut ftyp_seen = false;

    while cur.has_remaining() {
        let header = cur.read_box_header_lenient()?;
        match &header.kind {
            b"ftyp" => {
                let body = cur.consume(header.body_len())?;
                let mut b = Cursor::new(body);
                major_brand = b.read_array::<4>()?;
                let _minor = b.read_u32_be()?;
                while b.has_remaining() {
                    compatible_brands.push(b.read_array::<4>()?);
                }
                ftyp_seen = true;
            }
            b"meta" => {
                let body = cur.consume(header.body_len())?;
                meta_with_idat = Some(parse_meta(body)?);
            }
            b"mdat" => {
                // mdat is typically the largest box in a HEIC (multi-MiB).
                // Callers that only need metadata often pass a truncated
                // buffer — accept whatever fraction of mdat is present
                // and let `read_item_bytes` bounds-check at access time.
                let body_offset = cur.pos() as u64;
                let body_end = (cur.pos() + header.body_len()).min(bytes.len());
                let body = &bytes[cur.pos()..body_end];
                cur.advance_to(body_end);
                if mdat.is_none() {
                    mdat = Some((body_offset, body));
                }
            }
            _ => {
                // Skip unknown / uninteresting boxes. Tolerate truncation
                // (e.g. a `free`/`skip` box at the tail that runs off the
                // end of a trimmed buffer).
                let body_end = (cur.pos() + header.body_len()).min(bytes.len());
                cur.advance_to(body_end);
            }
        }
    }
    if !ftyp_seen {
        return Err(IsobmffError::Heif("no ftyp box".into()));
    }
    let (meta, idat) = meta_with_idat.ok_or_else(|| IsobmffError::Heif("no meta box".into()))?;
    let (mdat_offset, mdat) = mdat.ok_or_else(|| IsobmffError::Heif("no mdat box".into()))?;
    Ok(HeifFile {
        major_brand,
        compatible_brands,
        meta,
        mdat,
        mdat_offset,
        idat,
    })
}

fn parse_meta<'a>(body: &'a [u8]) -> IsobmffResult<(HeifMeta, Option<&'a [u8]>)> {
    let mut cur = Cursor::new(body);
    // FullBox header: version(1) + flags(3) — discard.
    cur.consume(4)?;

    let mut meta = HeifMeta::default();
    let mut idat: Option<&'a [u8]> = None;
    while cur.has_remaining() {
        let header = cur.read_box_header()?;
        let child = cur.consume(header.body_len())?;
        match &header.kind {
            b"pitm" => {
                let mut c = Cursor::new(child);
                let version = c.read_u8()?;
                c.consume(3)?; // flags
                meta.primary_item = if version == 0 {
                    c.read_u16_be()? as u32
                } else {
                    c.read_u32_be()?
                };
            }
            b"iinf" => meta.items = parse_iinf(child)?,
            b"iloc" => meta.locations = parse_iloc(child)?,
            b"iref" => meta.references = parse_iref(child)?,
            b"iprp" => meta.properties = parse_iprp(child)?,
            b"idat" => idat = Some(child),
            _ => {} // hdlr, dinf — ignored
        }
    }
    Ok((meta, idat))
}

fn parse_iinf(body: &[u8]) -> IsobmffResult<Vec<ItemInfo>> {
    let mut cur = Cursor::new(body);
    let version = cur.read_u8()?;
    cur.consume(3)?; // flags
    let entry_count = if version < 1 {
        cur.read_u16_be()? as u32
    } else {
        cur.read_u32_be()?
    };
    let mut out = Vec::with_capacity(entry_count as usize);
    for _ in 0..entry_count {
        let header = cur.read_box_header()?;
        let infe = cur.consume(header.body_len())?;
        if &header.kind != b"infe" {
            continue;
        }
        out.push(parse_infe(infe)?);
    }
    Ok(out)
}

fn parse_infe(body: &[u8]) -> IsobmffResult<ItemInfo> {
    let mut cur = Cursor::new(body);
    let version = cur.read_u8()?;
    cur.consume(3)?; // flags
    let (id, item_type) = match version {
        0 | 1 => {
            let id = cur.read_u16_be()? as u32;
            cur.consume(2)?; // item_protection_index
            // item_name (null-terminated), then content_type/content_encoding.
            // Item type 4cc is not present in v0/v1 — historically these were
            // MIME-typed only. We map to a sentinel so the consumer can still
            // dispatch via 4cc; iPhone HEICs always use v2+ infe.
            (id, *b"\0\0\0\0")
        }
        _ => {
            let id = if version == 2 {
                cur.read_u16_be()? as u32
            } else {
                cur.read_u32_be()?
            };
            cur.consume(2)?; // item_protection_index
            let item_type = cur.read_array::<4>()?;
            (id, item_type)
        }
    };
    // item_name (null-terminated UTF-8) — read up to the first 0 byte.
    let name = cur
        .read_null_terminated()
        .ok()
        .map(|s| String::from_utf8_lossy(s).into_owned());
    Ok(ItemInfo {
        id,
        item_type,
        name,
    })
}

fn parse_iloc(body: &[u8]) -> IsobmffResult<Vec<ItemLocation>> {
    let mut cur = Cursor::new(body);
    let version = cur.read_u8()?;
    cur.consume(3)?; // flags
    let packed = cur.read_u8()?;
    let offset_size = ((packed >> 4) & 0xF) as usize;
    let length_size = (packed & 0xF) as usize;
    let packed2 = cur.read_u8()?;
    let base_offset_size = ((packed2 >> 4) & 0xF) as usize;
    let index_size = if version == 1 || version == 2 {
        (packed2 & 0xF) as usize
    } else {
        0
    };
    let item_count = if version < 2 {
        cur.read_u16_be()? as u32
    } else {
        cur.read_u32_be()?
    };
    let mut out = Vec::with_capacity(item_count as usize);
    for _ in 0..item_count {
        let id = if version < 2 {
            cur.read_u16_be()? as u32
        } else {
            cur.read_u32_be()?
        };
        let construction_method = if version == 1 || version == 2 {
            (cur.read_u16_be()? & 0x0F) as u8
        } else {
            0
        };
        cur.consume(2)?; // data_reference_index
        let base_offset = cur.read_uint_be(base_offset_size)?;
        let extent_count = cur.read_u16_be()? as usize;
        let mut extents = Vec::with_capacity(extent_count);
        for _ in 0..extent_count {
            if version == 1 || version == 2 {
                cur.consume(index_size)?; // extent_index
            }
            let offset = cur.read_uint_be(offset_size)?;
            let length = cur.read_uint_be(length_size)?;
            extents.push(ItemExtent { offset, length });
        }
        out.push(ItemLocation {
            id,
            construction_method,
            base_offset,
            extents,
        });
    }
    Ok(out)
}

fn parse_iref(body: &[u8]) -> IsobmffResult<Vec<ItemReference>> {
    let mut cur = Cursor::new(body);
    let version = cur.read_u8()?;
    cur.consume(3)?; // flags
    let mut out = Vec::new();
    while cur.has_remaining() {
        let header = cur.read_box_header()?;
        let body = cur.consume(header.body_len())?;
        let mut c = Cursor::new(body);
        let from = if version == 0 {
            c.read_u16_be()? as u32
        } else {
            c.read_u32_be()?
        };
        let ref_count = c.read_u16_be()? as usize;
        let mut to = Vec::with_capacity(ref_count);
        for _ in 0..ref_count {
            let id = if version == 0 {
                c.read_u16_be()? as u32
            } else {
                c.read_u32_be()?
            };
            to.push(id);
        }
        out.push(ItemReference {
            from,
            ref_type: header.kind,
            to,
        });
    }
    Ok(out)
}

fn parse_iprp(body: &[u8]) -> IsobmffResult<PropertyTable> {
    let mut cur = Cursor::new(body);
    let mut table = PropertyTable::default();
    while cur.has_remaining() {
        let header = cur.read_box_header()?;
        let child = cur.consume(header.body_len())?;
        match &header.kind {
            b"ipco" => table.properties = parse_ipco(child)?,
            b"ipma" => parse_ipma_into(child, &mut table.associations)?,
            _ => {}
        }
    }
    Ok(table)
}

fn parse_ipco(body: &[u8]) -> IsobmffResult<Vec<Property>> {
    let mut cur = Cursor::new(body);
    let mut out = Vec::new();
    while cur.has_remaining() {
        let header = cur.read_box_header()?;
        let prop_body = cur.consume(header.body_len())?;
        out.push(parse_property(header.kind, prop_body)?);
    }
    Ok(out)
}

fn parse_property(four_cc: [u8; 4], body: &[u8]) -> IsobmffResult<Property> {
    Ok(match &four_cc {
        b"ispe" => {
            let mut c = Cursor::new(body);
            c.consume(4)?; // version+flags
            let width = c.read_u32_be()?;
            let height = c.read_u32_be()?;
            Property::Ispe { width, height }
        }
        b"pixi" => {
            let mut c = Cursor::new(body);
            c.consume(4)?; // version+flags
            let n = c.read_u8()? as usize;
            let mut depths = Vec::with_capacity(n);
            for _ in 0..n {
                depths.push(c.read_u8()?);
            }
            Property::Pixi {
                channel_depths: depths,
            }
        }
        b"colr" => {
            let mut c = Cursor::new(body);
            let colour_type = c.read_array::<4>()?;
            match &colour_type {
                b"nclx" => {
                    let primaries = c.read_u16_be()?;
                    let transfer = c.read_u16_be()?;
                    let matrix = c.read_u16_be()?;
                    let full_range = (c.read_u8()? & 0x80) != 0;
                    Property::Colr(ColrPayload::Nclx {
                        primaries,
                        transfer,
                        matrix,
                        full_range,
                    })
                }
                b"prof" | b"rICC" => {
                    let icc = c.remaining_bytes().to_vec();
                    Property::Colr(ColrPayload::Icc(icc))
                }
                _ => Property::Other {
                    four_cc,
                    data: body.to_vec(),
                },
            }
        }
        b"pasp" => {
            let mut c = Cursor::new(body);
            let h = c.read_u32_be()?;
            let v = c.read_u32_be()?;
            Property::Pasp { h, v }
        }
        b"auxC" => {
            let mut c = Cursor::new(body);
            c.consume(4)?; // version+flags
            let s = c.read_null_terminated().unwrap_or(&[]);
            Property::AuxC {
                aux_type: String::from_utf8_lossy(s).into_owned(),
            }
        }
        b"irot" => {
            let mut c = Cursor::new(body);
            let angle = c.read_u8()? & 0x03;
            Property::Irot {
                angle_quarter_turns: angle,
            }
        }
        b"imir" => {
            let mut c = Cursor::new(body);
            let axis_bit = c.read_u8()? & 0x01;
            Property::Imir {
                axis: if axis_bit == 0 {
                    MirrorAxis::Vertical
                } else {
                    MirrorAxis::Horizontal
                },
            }
        }
        b"clap" => Property::Clap,
        _ => Property::Other {
            four_cc,
            data: body.to_vec(),
        },
    })
}

fn parse_ipma_into(body: &[u8], out: &mut Vec<(u32, Vec<(u16, bool)>)>) -> IsobmffResult<()> {
    let mut cur = Cursor::new(body);
    let version = cur.read_u8()?;
    let flags = cur.read_uint_be(3)? as u32;
    let entry_count = cur.read_u32_be()?;
    let large_property_index = flags & 1 != 0;
    for _ in 0..entry_count {
        let id = if version == 0 {
            cur.read_u16_be()? as u32
        } else {
            cur.read_u32_be()?
        };
        let assoc_count = cur.read_u8()? as usize;
        let mut assocs = Vec::with_capacity(assoc_count);
        for _ in 0..assoc_count {
            let (idx, essential) = if large_property_index {
                let w = cur.read_u16_be()?;
                let essential = (w & 0x8000) != 0;
                (w & 0x7FFF, essential)
            } else {
                let b = cur.read_u8()?;
                let essential = (b & 0x80) != 0;
                ((b & 0x7F) as u16, essential)
            };
            assocs.push((idx, essential));
        }
        out.push((id, assocs));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Tiny zero-alloc Cursor used by the walker. Public box-walking via
// `Cursor` is intentionally not exported — consumers should reach for
// `read_heif`.
// ---------------------------------------------------------------------

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

struct BoxHeader {
    kind: [u8; 4],
    /// Total box length (header + body). Always set; large boxes
    /// (size==1 + u64 length) are resolved here.
    total: usize,
    /// Bytes already consumed for the header itself.
    header_size: usize,
}

impl BoxHeader {
    fn body_len(&self) -> usize {
        self.total - self.header_size
    }
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn pos(&self) -> usize {
        self.pos
    }
    fn has_remaining(&self) -> bool {
        self.pos < self.buf.len()
    }
    fn remaining_bytes(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }
    fn consume(&mut self, n: usize) -> IsobmffResult<&'a [u8]> {
        if self.pos + n > self.buf.len() {
            return Err(IsobmffError::Heif(format!(
                "short read: need {n} bytes at pos {}, have {}",
                self.pos,
                self.buf.len() - self.pos
            )));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn read_u8(&mut self) -> IsobmffResult<u8> {
        Ok(self.consume(1)?[0])
    }
    fn read_u16_be(&mut self) -> IsobmffResult<u16> {
        let s = self.consume(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn read_u32_be(&mut self) -> IsobmffResult<u32> {
        let s = self.consume(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn read_u64_be(&mut self) -> IsobmffResult<u64> {
        let s = self.consume(8)?;
        Ok(u64::from_be_bytes([
            s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
        ]))
    }
    fn read_array<const N: usize>(&mut self) -> IsobmffResult<[u8; N]> {
        let s = self.consume(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(s);
        Ok(out)
    }
    /// Variable-width unsigned big-endian. `n` in 0..=8; `n == 0` is 0.
    fn read_uint_be(&mut self, n: usize) -> IsobmffResult<u64> {
        if n == 0 {
            return Ok(0);
        }
        if n > 8 {
            return Err(IsobmffError::Heif(format!("read_uint_be: n={n} > 8")));
        }
        let s = self.consume(n)?;
        let mut v = 0u64;
        for &b in s {
            v = (v << 8) | b as u64;
        }
        Ok(v)
    }
    /// Read up to (but not including) the next 0 byte. Advances past
    /// the terminator. Returns the body slice.
    fn read_null_terminated(&mut self) -> IsobmffResult<&'a [u8]> {
        let start = self.pos;
        while self.pos < self.buf.len() {
            if self.buf[self.pos] == 0 {
                let s = &self.buf[start..self.pos];
                self.pos += 1;
                return Ok(s);
            }
            self.pos += 1;
        }
        Err(IsobmffError::Heif("unterminated string in box body".into()))
    }
    fn advance_to(&mut self, end: usize) {
        self.pos = end.min(self.buf.len());
    }
    /// Like [`read_box_header`] but allows the declared body length to
    /// exceed the buffer. The caller decides whether to bail (typical
    /// for meta) or accept partial content (typical for mdat) on
    /// truncated buffers.
    fn read_box_header_lenient(&mut self) -> IsobmffResult<BoxHeader> {
        let size32 = self.read_u32_be()?;
        let kind = self.read_array::<4>()?;
        let (total, header_size) = match size32 {
            0 => {
                let body_end = self.buf.len();
                (body_end + 8 - (self.pos - 8), 8)
            }
            1 => {
                let large = self.read_u64_be()? as usize;
                (large, 16)
            }
            n => (n as usize, 8),
        };
        if total < header_size {
            return Err(IsobmffError::Heif(format!(
                "box total {total} < header {header_size}"
            )));
        }
        Ok(BoxHeader {
            kind,
            total,
            header_size,
        })
    }
    fn read_box_header(&mut self) -> IsobmffResult<BoxHeader> {
        let size32 = self.read_u32_be()?;
        let kind = self.read_array::<4>()?;
        let (total, header_size) = match size32 {
            0 => {
                // 0 means "extends to EOF of the parent / file".
                let total = self.buf.len() + 8 - self.pos + 8;
                let _ = total;
                let body_end = self.buf.len();
                (body_end + 8 - (self.pos - 8), 8)
            }
            1 => {
                let large = self.read_u64_be()? as usize;
                (large, 16)
            }
            n => (n as usize, 8),
        };
        if total < header_size {
            return Err(IsobmffError::Heif(format!(
                "box total {total} < header {header_size}"
            )));
        }
        let pos_at_body = self.pos;
        if pos_at_body + total - header_size > self.buf.len() {
            return Err(IsobmffError::Heif(format!(
                "{:?}: body extends past EOF (total {}, header {})",
                kind, total, header_size
            )));
        }
        Ok(BoxHeader {
            kind,
            total,
            header_size,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-build a minimal HEIF file: ftyp + meta(pitm+iinf+iloc+iprp)
    /// + mdat. Single hvc1 item, one extent, one ispe property.
    fn synth_heic_minimal() -> Vec<u8> {
        fn box_with_body(four_cc: &[u8; 4], body: &[u8]) -> Vec<u8> {
            let total = 8 + body.len();
            let mut v = Vec::with_capacity(total);
            v.extend_from_slice(&(total as u32).to_be_bytes());
            v.extend_from_slice(four_cc);
            v.extend_from_slice(body);
            v
        }
        // ftyp: major=heic, minor=0, compatible=[heic, mif1]
        let mut ftyp_body = Vec::new();
        ftyp_body.extend_from_slice(b"heic");
        ftyp_body.extend_from_slice(&0u32.to_be_bytes());
        ftyp_body.extend_from_slice(b"heic");
        ftyp_body.extend_from_slice(b"mif1");
        let ftyp = box_with_body(b"ftyp", &ftyp_body);

        // pitm v0: primary_item = 1
        let mut pitm_body = Vec::new();
        pitm_body.extend_from_slice(&[0, 0, 0, 0]); // version + flags
        pitm_body.extend_from_slice(&1u16.to_be_bytes());
        let pitm = box_with_body(b"pitm", &pitm_body);

        // iinf v0: 1 entry, infe v2 (id=1, type=hvc1)
        let mut infe_body = Vec::new();
        infe_body.extend_from_slice(&[2, 0, 0, 0]); // version=2, flags
        infe_body.extend_from_slice(&1u16.to_be_bytes()); // id
        infe_body.extend_from_slice(&0u16.to_be_bytes()); // prot
        infe_body.extend_from_slice(b"hvc1");
        infe_body.push(0); // name (empty)
        let infe = box_with_body(b"infe", &infe_body);
        let mut iinf_body = Vec::new();
        iinf_body.extend_from_slice(&[0, 0, 0, 0]); // version + flags
        iinf_body.extend_from_slice(&1u16.to_be_bytes()); // entry_count
        iinf_body.extend_from_slice(&infe);
        let iinf = box_with_body(b"iinf", &iinf_body);

        // mdat body (raw payload — 10 bytes of "hello12345")
        let mdat_payload: &[u8] = b"hello12345";
        let mdat = box_with_body(b"mdat", mdat_payload);

        // We need to know where the mdat payload starts in the file so
        // iloc can point at it. Lay out and compute.
        // Pre-build meta with iloc pointing at a placeholder, then fix
        // up offsets once the file size is known.
        //
        // We'll structure: [ftyp][meta][mdat]
        // meta size depends on its content; iloc.base_offset is an
        // absolute file offset. Build meta first, then position mdat.

        // ispe property (width=2, height=2)
        let mut ispe_body = Vec::new();
        ispe_body.extend_from_slice(&[0, 0, 0, 0]); // version + flags
        ispe_body.extend_from_slice(&2u32.to_be_bytes());
        ispe_body.extend_from_slice(&2u32.to_be_bytes());
        let ispe = box_with_body(b"ispe", &ispe_body);
        let ipco = box_with_body(b"ipco", &ispe);

        // ipma v0: 1 entry, id=1, [(property_index=1, essential=true)]
        let mut ipma_body = Vec::new();
        ipma_body.extend_from_slice(&[0, 0, 0, 0]); // version + flags
        ipma_body.extend_from_slice(&1u32.to_be_bytes()); // entry_count
        ipma_body.extend_from_slice(&1u16.to_be_bytes()); // id
        ipma_body.push(1); // assoc_count
        ipma_body.push(0x81); // essential | property_index = 1
        let ipma = box_with_body(b"ipma", &ipma_body);
        let mut iprp_body = Vec::new();
        iprp_body.extend_from_slice(&ipco);
        iprp_body.extend_from_slice(&ipma);
        let iprp = box_with_body(b"iprp", &iprp_body);

        // iloc v1: offset_size=4, length_size=4, base_offset_size=4,
        // index_size=0. item_count=1. id=1, construction_method=0,
        // data_ref_index=0, base_offset=0 (file-relative),
        // extent_count=1, offset=<mdat_payload_offset>, length=10.
        // We don't know mdat_payload_offset yet — patch it after layout.
        let mut iloc_body = Vec::new();
        iloc_body.extend_from_slice(&[1, 0, 0, 0]); // version=1 + flags
        iloc_body.push(0x44); // offset_size=4 | length_size=4
        iloc_body.push(0x40); // base_offset_size=4 | index_size=0
        iloc_body.extend_from_slice(&1u16.to_be_bytes()); // item_count
        iloc_body.extend_from_slice(&1u16.to_be_bytes()); // id
        iloc_body.extend_from_slice(&0u16.to_be_bytes()); // construction
        iloc_body.extend_from_slice(&0u16.to_be_bytes()); // data_ref
        iloc_body.extend_from_slice(&0u32.to_be_bytes()); // base_offset
        iloc_body.extend_from_slice(&1u16.to_be_bytes()); // extent_count
        // Placeholder offset+length — patch after layout.
        let extent_offset_in_iloc_body = iloc_body.len();
        iloc_body.extend_from_slice(&0u32.to_be_bytes()); // offset placeholder
        iloc_body.extend_from_slice(&(mdat_payload.len() as u32).to_be_bytes());
        let iloc = box_with_body(b"iloc", &iloc_body);

        let mut meta_body = Vec::new();
        meta_body.extend_from_slice(&[0, 0, 0, 0]); // version + flags
        meta_body.extend_from_slice(&pitm);
        meta_body.extend_from_slice(&iinf);
        meta_body.extend_from_slice(&iprp);
        meta_body.extend_from_slice(&iloc);
        let meta_box = box_with_body(b"meta", &meta_body);

        let mdat_payload_file_offset = (ftyp.len() + meta_box.len() + 8) as u32;
        let mut file = Vec::new();
        file.extend_from_slice(&ftyp);
        file.extend_from_slice(&meta_box);
        file.extend_from_slice(&mdat);

        // Patch the iloc extent offset. The iloc box sits inside meta;
        // we know its index from layout:
        //   ftyp(N) + [meta header(8) + version+flags(4) + pitm + iinf + iprp + iloc...]
        let meta_start = ftyp.len();
        let iloc_in_meta_body_offset =
            4 + pitm.len() + iinf.len() + iprp.len() + 8 + extent_offset_in_iloc_body;
        let patch_at = meta_start + 8 + iloc_in_meta_body_offset;
        file[patch_at..patch_at + 4].copy_from_slice(&mdat_payload_file_offset.to_be_bytes());
        file
    }

    #[test]
    fn reads_synthetic_heic() {
        let bytes = synth_heic_minimal();
        let parsed = read_heif(&bytes).expect("parse ok");
        assert_eq!(&parsed.major_brand, b"heic");
        assert_eq!(parsed.compatible_brands.len(), 2);
        assert_eq!(parsed.meta.primary_item, 1);
        assert_eq!(parsed.meta.items.len(), 1);
        assert_eq!(&parsed.meta.items[0].item_type, b"hvc1");
        assert_eq!(parsed.meta.locations.len(), 1);
        let bitstream = parsed
            .read_item_bytes(&parsed.meta.locations[0])
            .expect("extents ok");
        assert_eq!(&bitstream, b"hello12345");
        // Property table: ispe at index 0, associated with item 1.
        let props = parsed.meta.properties_for(1);
        assert_eq!(props.len(), 1);
        match props[0] {
            Property::Ispe { width, height } => {
                assert_eq!((*width, *height), (2, 2));
            }
            other => panic!("expected Ispe, got {other:?}"),
        }
    }

    #[test]
    fn rejects_no_ftyp() {
        let r = read_heif(&[0u8; 12]);
        assert!(matches!(r, Err(IsobmffError::Heif(_))));
    }
}
