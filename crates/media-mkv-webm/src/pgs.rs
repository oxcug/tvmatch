//! Bounded headerless Matroska S_HDMV/PGS image reconstruction (not SUP or OCR).
//! See `pgs/README.md` for the deliberately strict supported subset and budgets.
use std::{error::Error, fmt};

#[cfg(test)]
#[path = "pgs/tests.rs"]
mod tests;

type Result<T> = std::result::Result<T, PgsError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PgsError {
    Malformed(&'static str),
    UnsupportedSegment(u8),
    UnsupportedCompositionState(u8),
    BudgetExceeded(&'static str),
    MissingState(&'static str),
    InvalidTimestamp,
    IncompleteDisplaySet,
    Poisoned,
}
impl fmt::Display for PgsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PGS: {self:?}")
    }
}
impl Error for PgsError {}

/// Independent of text subtitle and container I/O limits. Counts are live-state
/// bounds; packet/display/time limits bound work over a decoder's lifetime.
#[derive(Debug, Clone, Copy)]
pub struct PgsLimits {
    pub packet_bytes: usize,
    pub display_set_bytes: usize,
    pub object_bytes: usize,
    /// Pending declared RLE capacity + cached indices + temporary decoded indices.
    pub object_state_bytes: usize,
    /// Previous and newly rendered RGBA allocations together, before replacement.
    pub raster_bytes: usize,
    /// Cumulative cropped-raster pixels rendered, including repeated displays.
    pub total_render_pixels: usize,
    pub max_width: u16,
    pub max_height: u16,
    pub pixels: usize,
    pub objects: usize,
    pub palettes: usize,
    pub windows: usize,
    pub composition_objects: usize,
    pub segments_per_display: usize,
    pub packets: usize,
    pub displays: usize,
    pub total_packet_bytes: usize,
    pub max_timestamp_ns: u64,
}
impl Default for PgsLimits {
    fn default() -> Self {
        Self {
            packet_bytes: 4 * 1024 * 1024,
            display_set_bytes: 4 * 1024 * 1024,
            object_bytes: 4 * 1024 * 1024,
            object_state_bytes: 16 * 1024 * 1024,
            raster_bytes: 64 * 1024 * 1024,
            total_render_pixels: 256 * 1024 * 1024,
            max_width: 3840,
            max_height: 2160,
            pixels: 3840 * 2160,
            objects: 64,
            palettes: 8,
            windows: 8,
            composition_objects: 8,
            segments_per_display: 1024,
            packets: 100_000,
            displays: 20_000,
            total_packet_bytes: 64 * 1024 * 1024,
            max_timestamp_ns: 24 * 60 * 60 * 1_000_000_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositionObject {
    pub object_id: u16,
    pub window_id: u8,
    pub forced: bool,
    pub x: u16,
    pub y: u16,
    pub crop: Option<Rect>,
}
#[derive(Debug, PartialEq, Eq)]
pub struct PgsImage {
    /// Cropped union, positioned in the original canvas. Straight-alpha RGBA8.
    pub rect: Rect,
    pub rgba: Vec<u8>,
}
/// A committed replacement/clear, NOT a duration-bearing text cue. Its timestamp
/// closes the preceding committed display, even if equal. END is not an erase.
/// At clean EOF the last display's end remains unknown. No +5s duration is made up.
#[derive(Debug)]
pub struct PgsDisplay {
    pub timestamp_ns: u64,
    pub previous_timestamp_ns: Option<u64>,
    pub canvas_width: u16,
    pub canvas_height: u16,
    pub frame_rate_code: u8,
    pub composition_number: u16,
    pub composition_state: u8,
    pub palette_id: u8,
    pub palette_update: bool,
    pub objects: Vec<CompositionObject>,
    /// None is an effective clear (including a wholly transparent composition).
    pub image: Option<PgsImage>,
    /// Pixel/geometry/canvas equality, independent of composition number/time.
    /// Every commit is still emitted, retaining provenance for downstream dedup.
    pub unchanged: bool,
}
struct Composition {
    time: u64,
    width: u16,
    height: u16,
    rate: u8,
    number: u16,
    state: u8,
    palette: u8,
    update: bool,
    objects: Vec<CompositionObject>,
}
struct Object {
    id: u16,
    width: u16,
    height: u16,
    pixels: Vec<u8>,
}
struct Fragment {
    id: u16,
    version: u8,
    width: u16,
    height: u16,
    expected: usize,
    data: Vec<u8>,
}
struct Palette {
    id: u8,
    // Original Y/Cr/Cb/alpha, retained without destructive color conversion.
    entries: [Option<[u8; 4]>; 256],
}

/// Persistent single-track decoder. Any error poisons the instance: previously
/// emitted images are partial evidence, never a successful whole-file extraction.
/// Segments must be complete within packets; display sets/ODS may span packets.
pub struct PgsDecoder {
    limits: PgsLimits,
    objects: Vec<Object>,
    fragments: Vec<Fragment>,
    palettes: Vec<Palette>,
    windows: Vec<(u8, Rect)>,
    pending: Option<Composition>,
    last: Option<PgsDisplay>,
    last_packet_time: Option<u64>,
    set_bytes: usize,
    set_segments: usize,
    packets: usize,
    displays: usize,
    total_bytes: usize,
    rendered_pixels: usize,
    poisoned: bool,
}
impl PgsDecoder {
    pub fn new(limits: PgsLimits) -> Self {
        Self {
            limits,
            objects: Vec::new(),
            fragments: Vec::new(),
            palettes: Vec::new(),
            windows: Vec::new(),
            pending: None,
            last: None,
            last_packet_time: None,
            set_bytes: 0,
            set_segments: 0,
            packets: 0,
            displays: 0,
            total_bytes: 0,
            rendered_pixels: 0,
            poisoned: false,
        }
    }
    /// Callback borrows only the current image. Do not retain every raster for an
    /// episode. A later error invalidates whole-stream success, not earlier bytes.
    pub fn push_packet(
        &mut self,
        timestamp_ns: u64,
        packet: &[u8],
        mut emit: impl FnMut(&PgsDisplay),
    ) -> Result<()> {
        if self.poisoned {
            return Err(PgsError::Poisoned);
        }
        let result = self.push(timestamp_ns, packet, &mut emit);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    pub fn finish(&mut self) -> Result<()> {
        if self.poisoned {
            return Err(PgsError::Poisoned);
        }
        if self.pending.is_some() || !self.fragments.is_empty() {
            self.poisoned = true;
            return Err(PgsError::IncompleteDisplaySet);
        }
        Ok(())
    }
    fn push(&mut self, time: u64, packet: &[u8], emit: &mut impl FnMut(&PgsDisplay)) -> Result<()> {
        if time > self.limits.max_timestamp_ns || self.last_packet_time.is_some_and(|t| time < t) {
            return Err(PgsError::InvalidTimestamp);
        }
        self.last_packet_time = Some(time);
        bound(packet.len(), self.limits.packet_bytes, "packet bytes")?;
        charge(&mut self.packets, 1, self.limits.packets, "packets")?;
        charge(
            &mut self.total_bytes,
            packet.len(),
            self.limits.total_packet_bytes,
            "total packet bytes",
        )?;
        if packet.is_empty() {
            return Err(PgsError::Malformed("empty packet"));
        }
        let mut r = Bytes(packet);
        while !r.0.is_empty() {
            let kind = r.u8()?;
            let len = usize::from(r.u16()?);
            let body = r.take(len)?;
            charge(
                &mut self.set_bytes,
                len + 3,
                self.limits.display_set_bytes,
                "display set bytes",
            )?;
            charge(
                &mut self.set_segments,
                1,
                self.limits.segments_per_display,
                "display set segments",
            )?;
            if kind != 0x16 && self.pending.is_none() {
                return Err(PgsError::MissingState("segment before PCS"));
            }
            match kind {
                0x16 => self.pcs(time, body)?,
                0x14 => self.pds(body)?,
                0x15 => self.ods(body)?,
                0x17 => self.wds(body)?,
                0x80 => {
                    if !body.is_empty() {
                        return Err(PgsError::Malformed("nonempty END"));
                    }
                    self.commit()?;
                    emit(self.last.as_ref().unwrap());
                    self.set_bytes = 0;
                    self.set_segments = 0;
                }
                _ => return Err(PgsError::UnsupportedSegment(kind)),
            }
        }
        Ok(())
    }
    fn dimensions(&self, width: u16, height: u16) -> Result<usize> {
        if width == 0 || height == 0 {
            return Err(PgsError::Malformed("zero dimensions"));
        }
        bound(
            usize::from(width),
            usize::from(self.limits.max_width),
            "width",
        )?;
        bound(
            usize::from(height),
            usize::from(self.limits.max_height),
            "height",
        )?;
        let pixels = usize::from(width)
            .checked_mul(usize::from(height))
            .ok_or(PgsError::BudgetExceeded("pixels"))?;
        bound(pixels, self.limits.pixels, "pixels")?;
        Ok(pixels)
    }
    fn pcs(&mut self, time: u64, body: &[u8]) -> Result<()> {
        if self.pending.is_some() {
            return Err(PgsError::IncompleteDisplaySet);
        }
        let mut r = Bytes(body);
        let width = r.u16()?;
        let height = r.u16()?;
        self.dimensions(width, height)?;
        let rate = r.u8()?;
        let number = r.u16()?;
        let state = r.u8()?;
        if !matches!(state, 0 | 0x40 | 0x80) {
            return Err(PgsError::UnsupportedCompositionState(state));
        }
        let update = match r.u8()? {
            0 => false,
            0x80 => true,
            _ => return Err(PgsError::Malformed("palette update flag")),
        };
        let palette = r.u8()?;
        let count = usize::from(r.u8()?);
        bound(
            count,
            self.limits.composition_objects,
            "composition objects",
        )?;
        if state != 0x80
            && self
                .last
                .as_ref()
                .is_some_and(|last| last.canvas_width != width || last.canvas_height != height)
        {
            return Err(PgsError::Malformed("canvas changed without epoch"));
        }
        if state != 0 {
            // Acquisition (0x40) is a self-contained refresh within the epoch,
            // not normal cached-object reuse and not a new canvas/epoch. Both
            // refresh states release decoding resources; incomplete prior sets
            // were rejected above. Keep the last raster only for interval/dedup
            // and old+new allocation accounting; cumulative budgets never reset.
            self.objects.clear();
            self.fragments.clear();
            self.palettes.clear();
            self.windows.clear();
        }
        let mut objects = Vec::new();
        for _ in 0..count {
            let object_id = r.u16()?;
            let window_id = r.u8()?;
            let flags = r.u8()?;
            if flags & 0x3f != 0 {
                return Err(PgsError::Malformed("composition object flags"));
            }
            let x = r.u16()?;
            let y = r.u16()?;
            let crop = if flags & 0x80 != 0 {
                Some(r.rect()?)
            } else {
                None
            };
            objects.push(CompositionObject {
                object_id,
                window_id,
                forced: flags & 0x40 != 0,
                x,
                y,
                crop,
            });
        }
        r.end()?;
        if update && count == 0 {
            if state != 0 {
                return Err(PgsError::MissingState(
                    "palette update without fresh composition at epoch/acquisition",
                ));
            }
            objects = self
                .last
                .as_ref()
                .ok_or(PgsError::MissingState(
                    "palette update without committed composition",
                ))?
                .objects
                .clone();
        }
        self.pending = Some(Composition {
            time,
            width,
            height,
            rate,
            number,
            state,
            palette,
            update,
            objects,
        });
        Ok(())
    }
    fn pds(&mut self, body: &[u8]) -> Result<()> {
        let mut r = Bytes(body);
        let id = r.u8()?;
        let _version = r.u8()?;
        if !r.0.len().is_multiple_of(5) {
            return Err(PgsError::Malformed("palette length"));
        }
        let index = if let Some(i) = self.palettes.iter().position(|p| p.id == id) {
            i
        } else {
            bound(self.palettes.len() + 1, self.limits.palettes, "palettes")?;
            self.palettes.push(Palette {
                id,
                entries: [None; 256],
            });
            self.palettes.len() - 1
        };
        let mut seen = [false; 256];
        while !r.0.is_empty() {
            let entry = usize::from(r.u8()?);
            if seen[entry] {
                return Err(PgsError::Malformed("duplicate palette entry"));
            }
            seen[entry] = true;
            let y = r.u8()?;
            let cr = r.u8()?;
            let cb = r.u8()?;
            let alpha = r.u8()?;
            self.palettes[index].entries[entry] = Some([y, cr, cb, alpha]);
        }
        Ok(())
    }
    fn wds(&mut self, body: &[u8]) -> Result<()> {
        let mut r = Bytes(body);
        let count = usize::from(r.u8()?);
        bound(count, self.limits.windows, "windows")?;
        let c = self.pending.as_ref().unwrap();
        let mut windows = Vec::new();
        for _ in 0..count {
            let id = r.u8()?;
            let rect = r.rect()?;
            if rect.width == 0
                || rect.height == 0
                || right(rect) > u32::from(c.width)
                || bottom(rect) > u32::from(c.height)
            {
                return Err(PgsError::Malformed("window outside canvas"));
            }
            if windows.iter().any(|(old, _)| *old == id) {
                return Err(PgsError::Malformed("duplicate window"));
            }
            windows.push((id, rect));
        }
        r.end()?;
        self.windows = windows;
        Ok(())
    }
    fn state_bytes(&self) -> Result<usize> {
        self.objects
            .iter()
            .map(|o| o.pixels.len())
            .chain(self.fragments.iter().map(|f| f.expected))
            .try_fold(0usize, |n, size| {
                n.checked_add(size)
                    .ok_or(PgsError::BudgetExceeded("object state bytes"))
            })
    }
    fn ods(&mut self, body: &[u8]) -> Result<()> {
        let mut r = Bytes(body);
        let id = r.u16()?;
        let version = r.u8()?;
        let flags = r.u8()?;
        if flags & 0x3f != 0 {
            return Err(PgsError::Malformed("object sequence flags"));
        }
        let first = flags & 0x80 != 0;
        let last = flags & 0x40 != 0;
        let index = if first {
            if self.fragments.iter().any(|f| f.id == id) {
                return Err(PgsError::Malformed("overlapping first fragment"));
            }
            let expected = r
                .u24()?
                .checked_sub(4)
                .ok_or(PgsError::Malformed("object data length"))?;
            bound(
                expected,
                self.limits.object_bytes,
                "compressed object bytes",
            )?;
            let width = r.u16()?;
            let height = r.u16()?;
            self.dimensions(width, height)?;
            let live = self.objects.len()
                + self
                    .fragments
                    .iter()
                    .filter(|f| !self.objects.iter().any(|o| o.id == f.id))
                    .count();
            bound(
                live + usize::from(!self.objects.iter().any(|o| o.id == id)),
                self.limits.objects,
                "objects",
            )?;
            let mut state = self.state_bytes()?;
            charge(
                &mut state,
                expected,
                self.limits.object_state_bytes,
                "object state bytes",
            )?;
            let mut data = Vec::new();
            data.try_reserve_exact(expected)
                .map_err(|_| PgsError::BudgetExceeded("object allocation"))?;
            self.fragments.push(Fragment {
                id,
                version,
                width,
                height,
                expected,
                data,
            });
            self.fragments.len() - 1
        } else {
            self.fragments
                .iter()
                .position(|f| f.id == id && f.version == version)
                .ok_or(PgsError::MissingState("matching object fragment"))?
        };
        let f = &mut self.fragments[index];
        if r.0.len() > f.expected - f.data.len() {
            return Err(PgsError::Malformed("object length overflow"));
        }
        f.data.extend_from_slice(r.0);
        if last {
            if f.data.len() != f.expected {
                return Err(PgsError::Malformed("short last fragment"));
            }
            let pixels =
                self.dimensions(self.fragments[index].width, self.fragments[index].height)?;
            let mut state = self.state_bytes()?;
            charge(
                &mut state,
                pixels,
                self.limits.object_state_bytes,
                "object state bytes",
            )?;
            let f = &self.fragments[index];
            let decoded = decode_rle(&f.data, f.width, f.height, pixels)?;
            let object = Object {
                id,
                width: f.width,
                height: f.height,
                pixels: decoded,
            };
            self.fragments.remove(index);
            if let Some(i) = self.objects.iter().position(|o| o.id == id) {
                self.objects[i] = object;
            } else {
                self.objects.push(object);
            }
        } else if f.data.len() == f.expected {
            return Err(PgsError::Malformed("missing last fragment flag"));
        }
        Ok(())
    }
    fn commit(&mut self) -> Result<()> {
        if !self.fragments.is_empty() {
            return Err(PgsError::IncompleteDisplaySet);
        }
        charge(&mut self.displays, 1, self.limits.displays, "displays")?;
        let c = self.pending.as_ref().unwrap();
        let mut rendered_pixels = self.rendered_pixels;
        let image = self.render(c, &mut rendered_pixels)?;
        self.rendered_pixels = rendered_pixels;
        let unchanged = self.last.as_ref().is_some_and(|old| {
            old.canvas_width == c.width && old.canvas_height == c.height && old.image == image
        });
        let previous_timestamp_ns = self.last.as_ref().map(|old| old.timestamp_ns);
        let c = self.pending.take().unwrap();
        self.last = Some(PgsDisplay {
            timestamp_ns: c.time,
            previous_timestamp_ns,
            canvas_width: c.width,
            canvas_height: c.height,
            frame_rate_code: c.rate,
            composition_number: c.number,
            composition_state: c.state,
            palette_id: c.palette,
            palette_update: c.update,
            objects: c.objects,
            image,
            unchanged,
        });
        Ok(())
    }
    fn render(&self, c: &Composition, rendered_pixels: &mut usize) -> Result<Option<PgsImage>> {
        if c.objects.is_empty() {
            return Ok(None);
        }
        let palette = self
            .palettes
            .iter()
            .find(|p| p.id == c.palette)
            .ok_or(PgsError::MissingState("palette"))?;
        let mut placements = Vec::new();
        let mut union: Option<Rect> = None;
        for reference in &c.objects {
            let object = self
                .objects
                .iter()
                .find(|o| o.id == reference.object_id)
                .ok_or(PgsError::MissingState("object"))?;
            let window = self
                .windows
                .iter()
                .find(|(id, _)| *id == reference.window_id)
                .ok_or(PgsError::MissingState("window"))?
                .1;
            let crop = reference.crop.unwrap_or(Rect {
                x: 0,
                y: 0,
                width: object.width,
                height: object.height,
            });
            if crop.width == 0
                || crop.height == 0
                || right(crop) > u32::from(object.width)
                || bottom(crop) > u32::from(object.height)
            {
                return Err(PgsError::Malformed("object crop"));
            }
            // Crop selects source pixels; PCS x/y locates that crop in canvas space.
            let dest = Rect {
                x: reference.x,
                y: reference.y,
                width: crop.width,
                height: crop.height,
            };
            let clip = intersect(dest, window);
            if let Some(clip) = clip {
                union = Some(union.map_or(clip, |u| unite(u, clip)));
                placements.push((object, crop, dest, clip));
            }
        }
        let Some(rect) = union else {
            return Ok(None);
        };
        let area = self.dimensions(rect.width, rect.height)?;
        charge(
            rendered_pixels,
            area,
            self.limits.total_render_pixels,
            "total render pixels",
        )?;
        let bytes = area
            .checked_mul(4)
            .ok_or(PgsError::BudgetExceeded("raster bytes"))?;
        let mut live = self
            .last
            .as_ref()
            .and_then(|d| d.image.as_ref())
            .map_or(0, |i| i.rgba.len());
        charge(&mut live, bytes, self.limits.raster_bytes, "raster bytes")?;
        let mut pixels = zeroes(bytes)?;
        for (object, crop, dest, clip) in placements {
            for y in u32::from(clip.y)..bottom(clip) {
                for x in u32::from(clip.x)..right(clip) {
                    let sx = u32::from(crop.x) + x - u32::from(dest.x);
                    let sy = u32::from(crop.y) + y - u32::from(dest.y);
                    let index =
                        object.pixels[sy as usize * usize::from(object.width) + sx as usize];
                    // Undefined slots and reserved index255 are transparent;
                    // the palette ID itself must still have been declared.
                    let [luma, cr, cb, alpha] =
                        palette.entries[usize::from(index)].unwrap_or([16, 128, 128, 0]);
                    let color = rgba(luma, cr, cb, if index == 255 { 0 } else { alpha });
                    let offset = ((y - u32::from(rect.y)) as usize * usize::from(rect.width)
                        + (x - u32::from(rect.x)) as usize)
                        * 4;
                    over(&mut pixels[offset..offset + 4], color);
                }
            }
        }
        if pixels.chunks_exact(4).all(|p| p[3] == 0) {
            return Ok(None);
        }
        Ok(Some(PgsImage { rect, rgba: pixels }))
    }
}

fn bound(value: usize, max: usize, what: &'static str) -> Result<()> {
    if value > max {
        Err(PgsError::BudgetExceeded(what))
    } else {
        Ok(())
    }
}
fn charge(value: &mut usize, add: usize, max: usize, what: &'static str) -> Result<()> {
    let next = value
        .checked_add(add)
        .ok_or(PgsError::BudgetExceeded(what))?;
    bound(next, max, what)?;
    *value = next;
    Ok(())
}
fn zeroes(len: usize) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    v.try_reserve_exact(len)
        .map_err(|_| PgsError::BudgetExceeded("raster/index allocation"))?;
    v.resize(len, 0);
    Ok(v)
}
struct Bytes<'a>(&'a [u8]);
impl<'a> Bytes<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.0.len() {
            return Err(PgsError::Malformed("truncated segment"));
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u24(&mut self) -> Result<usize> {
        let b = self.take(3)?;
        Ok((usize::from(b[0]) << 16) | (usize::from(b[1]) << 8) | usize::from(b[2]))
    }
    fn rect(&mut self) -> Result<Rect> {
        Ok(Rect {
            x: self.u16()?,
            y: self.u16()?,
            width: self.u16()?,
            height: self.u16()?,
        })
    }
    fn end(&self) -> Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(PgsError::Malformed("segment trailing bytes"))
        }
    }
}
fn decode_rle(data: &[u8], width: u16, height: u16, pixels: usize) -> Result<Vec<u8>> {
    let mut out = zeroes(pixels)?;
    let mut r = Bytes(data);
    let (mut x, mut y) = (0usize, 0usize);
    let width = usize::from(width);
    let height = usize::from(height);
    while !r.0.is_empty() {
        if y == height {
            return Err(PgsError::Malformed("RLE trailing bytes"));
        }
        let first = r.u8()?;
        let (run, color) = if first != 0 {
            (1, first)
        } else {
            let flags = r.u8()?;
            if flags == 0 {
                if x != width {
                    return Err(PgsError::Malformed(
                        "RLE short row (implicit padding unsupported)",
                    ));
                }
                x = 0;
                y += 1;
                continue;
            }
            let run = if flags & 0x40 != 0 {
                (usize::from(flags & 0x3f) << 8) | usize::from(r.u8()?)
            } else {
                usize::from(flags & 0x3f)
            };
            let color = if flags & 0x80 != 0 { r.u8()? } else { 0 };
            (run, color)
        };
        if run == 0 || run > width - x {
            return Err(PgsError::Malformed("RLE row overflow/zero run"));
        }
        out[y * width + x..y * width + x + run].fill(color);
        x += run;
    }
    if y != height || x != 0 {
        return Err(PgsError::Malformed("RLE incomplete raster"));
    }
    Ok(out)
}
fn right(r: Rect) -> u32 {
    u32::from(r.x) + u32::from(r.width)
}
fn bottom(r: Rect) -> u32 {
    u32::from(r.y) + u32::from(r.height)
}
fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let r = right(a).min(right(b));
    let d = bottom(a).min(bottom(b));
    (r > u32::from(x) && d > u32::from(y)).then(|| Rect {
        x,
        y,
        width: (r - u32::from(x)) as u16,
        height: (d - u32::from(y)) as u16,
    })
}
fn unite(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Rect {
        x,
        y,
        width: (right(a).max(right(b)) - u32::from(x)) as u16,
        height: (bottom(a).max(bottom(b)) - u32::from(y)) as u16,
    }
}
// Studio-range BT.709 Y'CbCr (PGS HD subtitle palette), rounded and clamped.
fn rgba(y: u8, cr: u8, cb: u8, alpha: u8) -> [u8; 4] {
    let y = 298 * (i32::from(y) - 16);
    let cr = i32::from(cr) - 128;
    let cb = i32::from(cb) - 128;
    let channel = |v: i32| ((v + 128) >> 8).clamp(0, 255) as u8;
    [
        channel(y + 459 * cr),
        channel(y - 137 * cr - 55 * cb),
        channel(y + 541 * cb),
        alpha,
    ]
}
fn over(dst: &mut [u8], src: [u8; 4]) {
    let sa = u32::from(src[3]);
    let da = u32::from(dst[3]);
    let alpha = sa * 255 + da * (255 - sa);
    if alpha == 0 {
        dst.fill(0);
        return;
    }
    for c in 0..3 {
        dst[c] = ((u32::from(src[c]) * sa * 255 + u32::from(dst[c]) * da * (255 - sa) + alpha / 2)
            / alpha) as u8;
    }
    dst[3] = ((alpha + 127) / 255) as u8;
}
