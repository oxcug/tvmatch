//! Bounded, codec-agnostic non-fragmented MP4 demuxing over `Read + Seek`.
//! Metadata and payload reads have separate budgets. Only a selected track's
//! samples are read; descriptions and payload bytes remain opaque to this layer.
//! See `isobmff/DEMUX.md` for the supported table/edit subset and validation boundary.
#[cfg(test)]
#[path = "demux/tests.rs"]
mod tests;
use crate::{IsobmffError, IsobmffResult as Result};
use std::io::{Read, Seek, SeekFrom};
/// Fixed structural limits, independent of codec.
pub const MAX_MOOV: usize = 8 * 1024 * 1024;
pub const MAX_BOXES: usize = 100_000;
pub const MAX_SAMPLES: usize = 100_000;
pub const MAX_TRACKS: usize = 64;
/// Caller-selected payload budgets, cumulative per reader. Text consumers should
/// lower `sample_bytes`; the defaults also allow opaque video/audio samples.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub read_bytes: usize,
    pub sample_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            read_bytes: 16 * 1024 * 1024,
            sample_bytes: 16 * 1024 * 1024,
        }
    }
}
fn bad(s: &str) -> IsobmffError {
    IsobmffError::Malformed(s.into())
}
fn limit(s: &str) -> IsobmffError {
    IsobmffError::Unsupported(format!("demux limit exceeded: {s}"))
}
fn time_error() -> IsobmffError {
    bad("invalid/overflowed sample time")
}
fn u16be(b: &[u8], i: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(
        b.get(i..i + 2)
            .ok_or_else(|| bad("short field"))?
            .try_into()
            .unwrap(),
    ))
}
fn u32be(b: &[u8], i: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(
        b.get(i..i + 4)
            .ok_or_else(|| bad("short field"))?
            .try_into()
            .unwrap(),
    ))
}
fn u64be(b: &[u8], i: usize) -> Result<u64> {
    Ok(u64::from_be_bytes(
        b.get(i..i + 8)
            .ok_or_else(|| bad("short field"))?
            .try_into()
            .unwrap(),
    ))
}
fn full(b: &[u8]) -> Result<()> {
    if u32be(b, 0)? != 0 {
        return Err(bad("unsupported full-box version/flags"));
    }
    Ok(())
}
/// Checked child box, excluding its header. Payload interpretation is caller-owned.
#[derive(Clone, Copy, Debug)]
pub struct BoxView<'a> {
    pub kind: [u8; 4],
    pub data: &'a [u8],
}
type Atom<'a> = BoxView<'a>;
/// Bounded child-box walker for consumer-owned codec configuration/modifier boxes.
#[derive(Default)]
pub struct BoxBudget {
    used: usize,
}
impl BoxBudget {
    pub fn read<'a>(&mut self, data: &'a [u8]) -> Result<Vec<BoxView<'a>>> {
        boxes(data, &mut self.used)
    }
}
fn boxes<'a>(mut b: &'a [u8], work: &mut usize) -> Result<Vec<Atom<'a>>> {
    let mut out = Vec::new();
    while !b.is_empty() {
        if *work >= MAX_BOXES {
            return Err(limit("boxes"));
        }
        *work += 1;
        let n = u32be(b, 0)?;
        let kind = b
            .get(4..8)
            .ok_or_else(|| bad("short box"))?
            .try_into()
            .unwrap();
        let (size, header) = match n {
            0 => (b.len() as u64, 8),
            1 => (u64be(b, 8)?, 16),
            _ => (n as u64, 8),
        };
        if size < header as u64 || size > b.len() as u64 {
            return Err(bad("invalid child box size"));
        }
        let size = size as usize;
        out.push(Atom {
            kind,
            data: &b[header..size],
        });
        b = &b[size..];
    }
    Ok(out)
}
fn optional<'a>(atoms: &[Atom<'a>], kind: &[u8; 4]) -> Result<Option<&'a [u8]>> {
    let mut found = atoms.iter().filter(|a| &a.kind == kind);
    let first = found.next().map(|a| a.data);
    if found.next().is_some() {
        return Err(bad("duplicate required box"));
    }
    Ok(first)
}
fn required<'a>(a: &[Atom<'a>], k: &[u8; 4]) -> Result<&'a [u8]> {
    optional(a, k)?.ok_or_else(|| bad("missing required box"))
}
struct Input<R> {
    reader: R,
    read: usize,
    len: u64,
    limits: Limits,
}
impl<R: Read + Seek> Input<R> {
    fn bytes(&mut self, offset: u64, n: usize) -> Result<Vec<u8>> {
        if n > self.limits.read_bytes - self.read {
            return Err(limit("bytes read"));
        }
        if offset.checked_add(n as u64).is_none_or(|e| e > self.len) {
            return Err(bad("read outside file"));
        }
        self.read += n;
        self.reader
            .seek(SeekFrom::Start(offset))
            .map_err(|_| bad("seek failed"))?;
        let mut b = vec![0; n];
        self.reader
            .read_exact(&mut b)
            .map_err(|_| bad("short read"))?;
        Ok(b)
    }
}
/// ISO sample-entry header plus uninterpreted codec configuration.
#[derive(Clone, Debug)]
pub struct SampleDescription {
    pub format: [u8; 4],
    pub data_reference_index: u16,
    pub configuration: Vec<u8>,
}
/// Container declarations, not independently verified content or language.
#[derive(Clone, Debug)]
pub struct Track {
    pub id: u32,
    pub handler: [u8; 4],
    pub enabled: bool,
    pub language: Option<String>,
    pub descriptions: Vec<SampleDescription>,
    pub unsupported_reason: Option<&'static str>,
    pub timescale: u32,
    pub duration: u64,
    table: Vec<u8>,
    edit: Option<(u64, u64, u64)>,
}
/// Metadata index. Opening skips media payloads and does not expand sample tables.
pub struct Demuxer<R> {
    input: Input<R>,
    tracks: Vec<Track>,
    mdat: Vec<(u64, u64)>,
    movie_scale: u32,
    work: usize,
}
fn header_time(b: &[u8], movie: bool) -> Result<(u32, u64, usize)> {
    let v = *b.first().ok_or_else(|| bad("short time header"))?;
    let (scale, duration, lang, min) = match v {
        0 => (
            u32be(b, 12)?,
            u32be(b, 16)? as u64,
            20,
            if movie { 100 } else { 24 },
        ),
        1 => (
            u32be(b, 20)?,
            u64be(b, 24)?,
            32,
            if movie { 112 } else { 36 },
        ),
        _ => return Err(bad("unsupported time header")),
    };
    if b.len() < min || b[1..4] != [0, 0, 0] || scale == 0 {
        return Err(bad("invalid timescale/header"));
    }
    Ok((scale, duration, lang))
}
fn edit_list(data: Option<&[u8]>, work: &mut usize) -> Result<Option<(u64, u64, u64)>> {
    let Some(data) = data else {
        return Ok(None);
    };
    let a = boxes(data, work)?;
    let b = required(&a, b"elst")?;
    let version = *b.first().ok_or_else(|| bad("short edit list"))?;
    let width = match version {
        0 => 12,
        1 => 20,
        _ => return Err(bad("unsupported edit version")),
    };
    let count = u32be(b, 4)? as usize;
    if b[1..4] != [0, 0, 0] || !(1..=2).contains(&count) || b.len() != 8 + count * width {
        return Err(bad("unsupported edit list"));
    }
    let mut delay = 0;
    let mut media = None;
    for i in 0..count {
        let p = 8 + i * width;
        let (duration, time, rate) = if version == 0 {
            (u32be(b, p)? as u64, u32be(b, p + 4)? as i32 as i64, p + 8)
        } else {
            (u64be(b, p)?, u64be(b, p + 8)? as i64, p + 16)
        };
        if u32be(b, rate)? != 0x00010000 || duration == 0 {
            return Err(bad("unsupported edit rate/duration"));
        }
        if time == -1 && i == 0 && count == 2 {
            delay = duration;
        } else if time >= 0 && i == count - 1 {
            media = Some((delay, time as u64, duration));
        } else {
            return Err(bad("unsupported edit sequence"));
        }
    }
    media.map(Some).ok_or_else(|| bad("missing media edit"))
}
fn open<R: Read + Seek>(mut reader: R, limits: Limits) -> Result<Demuxer<R>> {
    if limits.read_bytes == 0 || limits.sample_bytes == 0 {
        return Err(limit("zero read/sample budget"));
    }
    let len = reader
        .seek(SeekFrom::End(0))
        .map_err(|_| bad("length unavailable"))?;
    let mut input = Input {
        reader,
        read: 0,
        len,
        limits,
    };
    let mut pos = 0;
    let mut work = 0;
    let mut moov = None;
    let mut ftyp = false;
    let mut mdat = Vec::new();
    while pos < len {
        if work >= MAX_BOXES {
            return Err(limit("boxes"));
        }
        work += 1;
        let h = input.bytes(pos, 8)?;
        let n = u32be(&h, 0)?;
        let (size, header) = match n {
            0 => (len - pos, 8),
            1 => (u64be(&input.bytes(pos + 8, 8)?, 0)?, 16),
            _ => (n as u64, 8),
        };
        if size < header || size > len - pos {
            return Err(bad("invalid top-level box size"));
        }
        let end = pos + size;
        match &h[4..8] {
            b"ftyp" => {
                if ftyp || size - header < 8 || size - header > 4096 || (size - header) % 4 != 0 {
                    return Err(bad("invalid ftyp"));
                }
                let b = input.bytes(pos + header, (size - header) as usize)?;
                let allowed = [
                    *b"isom", *b"iso2", *b"iso3", *b"iso4", *b"iso5", *b"iso6", *b"mp41", *b"mp42",
                    *b"M4V ", *b"avc1", *b"dash",
                ];
                if !std::iter::once(&b[..4])
                    .chain(b[8..].chunks_exact(4))
                    .any(|v| allowed.iter().any(|a| v == a))
                {
                    return Err(bad("unsupported ISO BMFF brand"));
                }
                ftyp = true;
            }
            b"moov" => {
                if moov.is_some() || size - header > MAX_MOOV as u64 {
                    return Err(bad("duplicate/oversized moov"));
                }
                moov = Some(input.bytes(pos + header, (size - header) as usize)?);
            }
            b"mdat" => {
                if mdat.len() == 1024 {
                    return Err(limit("media extents"));
                }
                mdat.push((pos + header, end));
            }
            b"moof" => return Err(IsobmffError::Unsupported("fragmented MP4".into())),
            _ => {}
        }
        pos = end;
    }
    if !ftyp {
        return Err(bad("missing MP4 file type"));
    }
    let moov = moov.ok_or_else(|| bad("missing moov"))?;
    let root = boxes(&moov, &mut work)?;
    if optional(&root, b"mvex")?.is_some() {
        return Err(IsobmffError::Unsupported("fragmented MP4".into()));
    }
    let (movie_scale, _, _) = header_time(required(&root, b"mvhd")?, true)?;
    let mut tracks = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    let mut track_count = 0;
    for a in root.iter().filter(|a| &a.kind == b"trak") {
        track_count += 1;
        if track_count > MAX_TRACKS {
            return Err(limit("tracks"));
        }
        let t = boxes(a.data, &mut work)?;
        let tk = required(&t, b"tkhd")?;
        let (id, min) = match tk.first() {
            Some(0) => (u32be(tk, 12)?, 84),
            Some(1) => (u32be(tk, 20)?, 96),
            _ => return Err(bad("unsupported track header")),
        };
        if tk.len() < min || id == 0 || !ids.insert(id) {
            return Err(bad("invalid/duplicate track identity"));
        }
        let enabled = u32be(tk, 0)? & 1 != 0;
        let mdia = boxes(required(&t, b"mdia")?, &mut work)?;
        let handler = required(&mdia, b"hdlr")?;
        full(handler)?;
        if handler.len() < 24 {
            return Err(bad("short handler"));
        }
        let handler = handler[8..12].try_into().unwrap();
        let mdhd = required(&mdia, b"mdhd")?;
        let (scale, duration, lang_offset) = header_time(mdhd, false)?;
        let packed = u16be(mdhd, lang_offset)?;
        let language = if packed & 0x8000 != 0 {
            None
        } else {
            let b = [
                ((packed >> 10) & 31) as u8,
                ((packed >> 5) & 31) as u8,
                (packed & 31) as u8,
            ];
            b.iter()
                .all(|v| (1..=26).contains(v))
                .then(|| b.iter().map(|v| (v + 0x60) as char).collect::<String>())
        };
        let language = if let Some(elng) = optional(&mdia, b"elng")? {
            full(elng)?;
            let v = elng[4..].strip_suffix(&[0]).unwrap_or(&elng[4..]);
            let s = std::str::from_utf8(v).map_err(|_| bad("invalid extended language"))?;
            if s.is_empty()
                || s.len() > 63
                || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return Err(bad("invalid extended language"));
            }
            Some(s.to_owned())
        } else {
            language
        };
        let minf = boxes(required(&mdia, b"minf")?, &mut work)?;
        let stbl = required(&minf, b"stbl")?;
        let tables = boxes(stbl, &mut work)?;
        let stsd = required(&tables, b"stsd")?;
        full(stsd)?;
        let entries = boxes(
            stsd.get(8..)
                .ok_or_else(|| bad("short sample description"))?,
            &mut work,
        )?;
        if u32be(stsd, 4)? as usize != entries.len() || entries.is_empty() {
            return Err(bad("invalid sample descriptions"));
        }
        let mut descriptions = Vec::new();
        for entry in entries {
            if entry.data.len() < 8 || entry.data[..6] != [0; 6] {
                return Err(bad("invalid sample entry header"));
            }
            descriptions.push(SampleDescription {
                format: entry.kind,
                data_reference_index: u16be(entry.data, 6)?,
                configuration: entry.data[8..].to_vec(),
            });
        }
        let mut unsupported_reason = if descriptions.len() == 1 {
            None
        } else {
            Some("multiple sample descriptions")
        };
        let dinf = boxes(required(&minf, b"dinf")?, &mut work)?;
        let dref = required(&dinf, b"dref")?;
        full(dref)?;
        let refs = boxes(
            dref.get(8..).ok_or_else(|| bad("short data reference"))?,
            &mut work,
        )?;
        if u32be(dref, 4)? != 1
            || refs.len() != 1
            || refs[0].kind != *b"url "
            || refs[0].data != [0, 0, 0, 1]
            || descriptions.iter().any(|d| d.data_reference_index != 1)
        {
            unsupported_reason = Some("external/multiple data references");
        }
        if tables.iter().any(|a| {
            !matches!(
                &a.kind,
                b"stsd" | b"stts" | b"stsc" | b"stsz" | b"stco" | b"co64" | b"stss" | b"free"
            )
        }) {
            unsupported_reason = Some("unsupported sample tables");
        }
        let edit = edit_list(optional(&t, b"edts")?, &mut work);
        if edit.is_err() {
            unsupported_reason = Some("unsupported edit list");
        }
        tracks.push(Track {
            id,
            handler,
            enabled,
            language,
            descriptions,
            unsupported_reason,
            timescale: scale,
            duration,
            table: stbl.to_vec(),
            edit: edit.unwrap_or(None),
        });
    }
    Ok(Demuxer {
        input,
        tracks,
        mdat,
        movie_scale,
        work,
    })
}
#[derive(Clone, Copy)]
struct SampleLocation {
    offset: u64,
    size: u32,
    start: u64,
    duration: u32,
}
fn table<'a>(a: &[Atom<'a>], kind: &[u8; 4], width: usize) -> Result<(&'a [u8], usize)> {
    let b = required(a, kind)?;
    full(b)?;
    let n = u32be(b, 4)? as usize;
    if n > MAX_SAMPLES || b.len() != 8 + n * width {
        return Err(bad("invalid/oversized sample table"));
    }
    Ok((&b[8..], n))
}
fn samples(
    t: &Track,
    mdat: &[(u64, u64)],
    work: &mut usize,
    sample_limit: usize,
) -> Result<Vec<SampleLocation>> {
    let a = boxes(&t.table, work)?;
    let sz = required(&a, b"stsz")?;
    full(sz)?;
    let constant = u32be(sz, 4)?;
    let count = u32be(sz, 8)? as usize;
    if count > MAX_SAMPLES || sz.len() != 12 + if constant == 0 { count * 4 } else { 0 } {
        return Err(bad("invalid/oversized sample sizes"));
    }
    let (stts, n) = table(&a, b"stts", 8)?;
    let mut timings = Vec::new();
    let mut time = 0u64;
    for i in 0..n {
        let count = u32be(stts, i * 8)? as usize;
        let duration = u32be(stts, i * 8 + 4)?;
        if count == 0 || count > MAX_SAMPLES - timings.len() {
            return Err(bad("invalid timing run"));
        }
        for _ in 0..count {
            timings.push((time, duration));
            time = time.checked_add(duration as u64).ok_or_else(time_error)?;
        }
    }
    if time != t.duration {
        return Err(bad("sample timing/media duration mismatch"));
    }
    if timings.len() != count {
        return Err(bad("sample/timing count mismatch"));
    }
    let wide = optional(&a, b"co64")?.is_some();
    if wide && optional(&a, b"stco")?.is_some() {
        return Err(bad("duplicate chunk offset formats"));
    }
    let (offsets, chunks) = table(
        &a,
        if wide { b"co64" } else { b"stco" },
        if wide { 8 } else { 4 },
    )?;
    let (stsc, runs) = table(&a, b"stsc", 12)?;
    let mut mapping = Vec::new();
    for i in 0..runs {
        let first = u32be(stsc, i * 12)? as usize;
        let per = u32be(stsc, i * 12 + 4)? as usize;
        let desc = u32be(stsc, i * 12 + 8)?;
        if first == 0
            || first > chunks
            || per == 0
            || per > MAX_SAMPLES
            || desc != 1
            || mapping.last().is_some_and(|(p, _)| *p >= first)
        {
            return Err(bad("invalid chunk mapping"));
        }
        mapping.push((first, per));
    }
    if (chunks > 0 && mapping.first().map(|v| v.0) != Some(1)) || (chunks == 0 && count != 0) {
        return Err(bad("incomplete chunk mapping"));
    }
    let mut out = Vec::new();
    let mut run = 0;
    for chunk in 0..chunks {
        while run + 1 < mapping.len() && mapping[run + 1].0 <= chunk + 1 {
            run += 1;
        }
        let mut offset = if wide {
            u64be(offsets, chunk * 8)?
        } else {
            u32be(offsets, chunk * 4)? as u64
        };
        let per = mapping[run].1;
        if per > count - out.len() {
            return Err(bad("chunk sample count exceeds stsz"));
        }
        for _ in 0..per {
            let i = out.len();
            let size = if constant == 0 {
                u32be(sz, 12 + i * 4)?
            } else {
                constant
            };
            if size as u64 > sample_limit as u64 {
                return Err(limit("sample bytes"));
            }
            let end = offset
                .checked_add(size as u64)
                .ok_or_else(|| bad("sample range overflow"))?;
            let at = mdat.partition_point(|(start, _)| *start <= offset);
            if at == 0 || end > mdat[at - 1].1 {
                return Err(bad("sample outside media data"));
            }
            out.push(SampleLocation {
                offset,
                size,
                start: timings[i].0,
                duration: timings[i].1,
            });
            offset = end;
        }
    }
    if out.len() != count {
        return Err(bad("incomplete sample mapping"));
    }
    let mut ranges = out
        .iter()
        .map(|s| (s.offset, s.offset + s.size as u64))
        .collect::<Vec<_>>();
    ranges.sort_unstable();
    if ranges.windows(2).any(|p| p[0].1 > p[1].0) {
        return Err(bad("overlapping selected samples"));
    }
    Ok(out)
}
fn edited_times(
    start: u64,
    duration: u32,
    scale: u32,
    movie_scale: u32,
    (delay, media, length): (u64, u64, u64),
) -> Result<Option<(u64, u64)>> {
    let a = u128::from(start) * u128::from(movie_scale);
    let b = (u128::from(start) + u128::from(duration)) * u128::from(movie_scale);
    let begin = u128::from(media) * u128::from(movie_scale);
    let limit = begin + u128::from(length) * u128::from(scale);
    if b <= begin || a >= limit {
        return Ok(None);
    }
    let delay = u128::from(delay) * u128::from(scale);
    let denom = u128::from(scale) * u128::from(movie_scale);
    let convert = |n: u128| -> Result<u64> {
        u64::try_from(n.checked_mul(1_000_000_000).ok_or_else(time_error)? / denom)
            .map_err(|_| time_error())
    };
    Ok(Some((
        convert(delay + a.max(begin) - begin)?,
        convert(delay + b.min(limit) - begin)?,
    )))
}
fn ns(t: u64, scale: u32) -> Result<u64> {
    u64::try_from(u128::from(t) * 1_000_000_000 / u128::from(scale)).map_err(|_| time_error())
}
impl<R: Read + Seek> Demuxer<R> {
    pub fn open(reader: R) -> Result<Self> {
        open(reader, Limits::default())
    }
    pub fn with_limits(reader: R, limits: Limits) -> Result<Self> {
        open(reader, limits)
    }
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }
    pub fn bytes_read(&self) -> usize {
        self.input.read
    }
    /// Validate the selected table completely before returning any payload.
    /// Other tracks' sample tables/payloads are not validated or decoded.
    pub fn into_samples(mut self, id: u32) -> Result<SampleReader<R>> {
        let i = self
            .tracks
            .iter()
            .position(|t| t.id == id)
            .ok_or_else(|| bad("track not found"))?;
        let track = self.tracks.remove(i);
        if let Some(reason) = track.unsupported_reason {
            return Err(IsobmffError::Unsupported(reason.into()));
        }
        let locations = samples(
            &track,
            &self.mdat,
            &mut self.work,
            self.input.limits.sample_bytes,
        )?
        .into_iter();
        Ok(SampleReader {
            input: self.input,
            track,
            locations,
            movie_scale: self.movie_scale,
            failed: false,
        })
    }
}
/// Opaque sample data and container timing. `presentation_ns == None` means the
/// sample lies outside the selected media edit; bytes still reach the consumer
/// so it can validate them. No codec, language or caption policy is applied here.
#[derive(Debug)]
pub struct Sample {
    pub data: Vec<u8>,
    pub decode_time: u64,
    pub duration: u32,
    pub source_duration_ns: u64,
    pub presentation_ns: Option<(u64, u64)>,
}
/// Forward selected-track reader; a failed read is terminal. Stopping early leaves
/// the remaining sample payloads unvalidated, even though table ranges were checked.
pub struct SampleReader<R> {
    input: Input<R>,
    track: Track,
    locations: std::vec::IntoIter<SampleLocation>,
    movie_scale: u32,
    failed: bool,
}
impl<R: Read + Seek> SampleReader<R> {
    pub fn track(&self) -> &Track {
        &self.track
    }
    pub fn bytes_read(&self) -> usize {
        self.input.read
    }
    pub fn next_sample(&mut self) -> Result<Option<Sample>> {
        if self.failed {
            return Err(bad("sample reader is in failed state"));
        }
        let Some(sample) = self.locations.next() else {
            return Ok(None);
        };
        // Set failure first; clear it only after the complete operation succeeds.
        self.failed = true;
        let data = self.input.bytes(sample.offset, sample.size as usize)?;
        let presentation_ns = if let Some(edit) = self.track.edit {
            edited_times(
                sample.start,
                sample.duration,
                self.track.timescale,
                self.movie_scale,
                edit,
            )?
        } else {
            Some((
                ns(sample.start, self.track.timescale)?,
                ns(
                    sample
                        .start
                        .checked_add(sample.duration as u64)
                        .ok_or_else(time_error)?,
                    self.track.timescale,
                )?,
            ))
        };
        let source_duration_ns = ns(sample.duration as u64, self.track.timescale)?;
        self.failed = false;
        Ok(Some(Sample {
            data,
            decode_time: sample.start,
            duration: sample.duration,
            source_duration_ns,
            presentation_ns,
        }))
    }
}
