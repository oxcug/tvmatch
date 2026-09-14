//! Read-side ISOBMFF parsing for source MP4 files.
//!
//! Companion to [`crate::boxes`] (write-side) and [`crate::heif`]
//! (HEIF parsing). [`parse_video_track`] walks `ftyp / moov / trak`,
//! finds the first video track, and materializes its sample table
//! into a dense [`Vec<SampleRef>`] keyed by absolute file offset.
//!
//! Consumers (e.g. the app's video segment match-copy path) can then
//! slice samples covering `[offset, offset + duration)` directly out
//! of the source bytes — no decode needed when the source codec is
//! already in the client's accepted list.

use crate::{IsobmffError, IsobmffResult};

/// Video codec families recognized by [`parse_video_track`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    /// AVC / H.264 — sample entry `avc1`/`avc3`, config in `avcC`.
    Avc,
    /// HEVC / H.265 — sample entry `hvc1`/`hev1`, config in `hvcC`.
    Hevc,
    /// AV1 — sample entry `av01`, config in `av1C`.
    Av1,
}

/// Audio codec families recognized by [`parse_audio_track`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    /// AAC — sample entry `mp4a` (OTI 0x40), config in the inner
    /// `esds` AudioSpecificConfig.
    Aac,
    /// FLAC — sample entry `fLaC`, config in `dfLa`.
    Flac,
    /// MP3 — sample entry `mp4a` with OTI 0x69/0x6B, or `.mp3`. No
    /// separate config blob (the bitstream is self-describing).
    Mp3,
}

/// One sample (frame) in a video track. `offset` is an absolute byte
/// position in the source file the buffer was parsed from.
#[derive(Debug, Clone, Copy)]
pub struct SampleRef {
    pub offset: u64,
    pub size: u32,
    /// Decode timestamp in track timescale.
    pub dts: u64,
    /// Sample duration in track timescale.
    pub duration: u32,
    /// `PTS - DTS`. Zero when the track has no `ctts` (B-frame-free).
    pub cts_offset: i32,
    /// `true` for sync samples. Defaults to `true` for every sample if
    /// the track omits `stss` (per ISO/IEC 14496-12, all samples are
    /// sync samples in that case).
    pub is_keyframe: bool,
}

impl SampleRef {
    /// Presentation timestamp in track timescale. Saturates at 0 if the
    /// `ctts` offset is negative and exceeds `dts` — pathological input.
    pub fn pts(&self) -> u64 {
        if self.cts_offset >= 0 {
            self.dts.saturating_add(self.cts_offset as u64)
        } else {
            self.dts.saturating_sub((-self.cts_offset) as u64)
        }
    }
}

/// Parsed video track. The samples vec is dense — one entry per
/// frame, in decode order.
#[derive(Debug, Clone)]
pub struct VideoTrack {
    pub codec: VideoCodec,
    /// Codec configuration record — `avcC` body for AVC, `hvcC` body
    /// for HEVC, `av1C` body for AV1. Suitable for direct use in
    /// `MediaSource` init segments and `VideoDecoder.configure`.
    pub codec_config: Vec<u8>,
    pub width: u16,
    pub height: u16,
    /// Track media timescale (samples per second for the sample-table
    /// timing fields). Sourced from `mdhd`.
    pub timescale: u32,
    /// Track duration in `timescale` units. Sourced from `mdhd`.
    pub duration: u64,
    pub samples: Vec<SampleRef>,
}

/// Parsed audio track. Mirrors [`VideoTrack`] for the audio side —
/// produced by [`parse_audio_track_from_moov`] when a movie's `moov`
/// carries a trak with handler type `soun`.
#[derive(Debug, Clone)]
pub struct AudioTrack {
    pub codec: AudioCodec,
    /// Codec-specific configuration record:
    /// - AAC → AudioSpecificConfig bytes (typically 2 for AAC-LC).
    /// - FLAC → `dfLa` body (FullBox header + STREAMINFO + ...).
    /// - MP3 → empty.
    pub codec_config: Vec<u8>,
    pub channel_count: u16,
    pub sample_rate: u32,
    /// Track media timescale. For audio this is almost always the
    /// codec's actual sample rate (e.g. 48000 for AAC at 48 kHz), but
    /// the spec allows divergence — callers should trust `timescale`
    /// for sample-timing math and `sample_rate` for codec mime/config.
    pub timescale: u32,
    pub duration: u64,
    pub samples: Vec<SampleRef>,
}

impl VideoTrack {
    /// Index of the first sync sample whose presentation time is `>=
    /// time_in_timescale`. Falls back to the largest sync sample with
    /// `pts <= time_in_timescale` if no later sync sample exists. The
    /// MSE match-copy path needs to start fragments on a keyframe so
    /// the receiver can decode without referencing earlier samples.
    pub fn keyframe_index_for_time(&self, time_in_timescale: u64) -> Option<usize> {
        let mut last_before = None;
        let mut first_after = None;
        for (i, s) in self.samples.iter().enumerate() {
            if !s.is_keyframe {
                continue;
            }
            if s.pts() <= time_in_timescale {
                last_before = Some(i);
            } else if first_after.is_none() {
                first_after = Some(i);
                break;
            }
        }
        last_before.or(first_after)
    }
}

// ─────────────────────────── public API ───────────────────────────

/// Walk an ISOBMFF buffer and materialize its first video track.
///
/// `bytes` must contain at least the `moov` box. Sample byte offsets
/// in [`SampleRef::offset`] are absolute file positions sourced from
/// `stco`/`co64`, so the buffer **doesn't** need to contain `mdat` —
/// callers can pass just the moov region and read sample payloads
/// from the file separately via [`SampleRef::offset`] + `size`.
///
/// Returns `Err(IsobmffError::Parse(..))` if the buffer doesn't start
/// with a valid box stream, has no `moov`, has no `trak` with handler
/// type `vide`, or the sample table is internally inconsistent.
pub fn parse_video_track(bytes: &[u8]) -> IsobmffResult<VideoTrack> {
    let moov = find_top_level_box(bytes, b"moov")?.ok_or_else(|| parse_err("no moov box"))?;
    parse_video_track_from_moov(moov)
}

/// Parse a video track given just the `moov` box body. Callers that
/// already located moov (e.g. via [`read_top_level_box`]) skip the
/// re-scan that [`parse_video_track`] does.
pub fn parse_video_track_from_moov(moov_body: &[u8]) -> IsobmffResult<VideoTrack> {
    let mut cur = Cursor::new(moov_body);
    while cur.has_remaining() {
        let (hdr, body) = cur.read_box()?;
        if &hdr.kind == b"trak"
            && let Some(track) = try_parse_video_trak(body)?
        {
            return Ok(track);
        }
    }
    Err(parse_err("no video track in moov"))
}

/// Walk an ISOBMFF buffer and materialize its first audio track.
///
/// Companion to [`parse_video_track`]. Returns `Ok(None)` when the
/// `moov` contains no `soun` trak (e.g. video-only files), so the
/// caller can route a missing-audio movie into the
/// caller-defined omission response without a hard error.
/// Returns `Err(...)` only on malformed input.
pub fn parse_audio_track(bytes: &[u8]) -> IsobmffResult<Option<AudioTrack>> {
    let moov = find_top_level_box(bytes, b"moov")?.ok_or_else(|| parse_err("no moov box"))?;
    parse_audio_track_from_moov(moov)
}

/// Parse an audio track given just the `moov` box body. Returns
/// `Ok(None)` when no `soun` trak is present.
pub fn parse_audio_track_from_moov(moov_body: &[u8]) -> IsobmffResult<Option<AudioTrack>> {
    let mut cur = Cursor::new(moov_body);
    while cur.has_remaining() {
        let (hdr, body) = cur.read_box()?;
        if &hdr.kind == b"trak"
            && let Some(track) = try_parse_audio_trak(body)?
        {
            return Ok(Some(track));
        }
    }
    Ok(None)
}

/// Stream-scan top-level box headers from `reader` and return the
/// body of the first box matching `kind`. Other top-level boxes
/// (`ftyp`, `mdat`, `free`, …) are skipped via `seek`, so the reader
/// only fetches the bytes the caller asked for plus 8-16 bytes per
/// skipped header — load-bearing for files where `mdat` is many GB.
///
/// On success, the reader's cursor is left **at the end** of the
/// returned box.
pub fn read_top_level_box<R: std::io::Read + std::io::Seek>(
    reader: &mut R,
    kind: &[u8; 4],
) -> IsobmffResult<Vec<u8>> {
    use std::io::SeekFrom;
    loop {
        let here_before = reader.stream_position()?;
        let mut header = [0u8; 8];
        let got = read_full_or_eof(reader, &mut header)?;
        if got == 0 {
            return Err(parse_err(format!(
                "no {:?} box found in stream",
                std::str::from_utf8(kind).unwrap_or("?")
            )));
        }
        if got < 8 {
            return Err(parse_err(format!(
                "short box header at file offset {here_before}: got {got} bytes, need 8"
            )));
        }
        let size32 = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let this_kind = [header[4], header[5], header[6], header[7]];
        let (total, header_size) = match size32 {
            0 => {
                // 0 means "extends to EOF". Use the byte count from
                // here_before (start of header) to file end.
                let end = reader.seek(SeekFrom::End(0))?;
                // Rewind so the upcoming `read_exact` / `seek` lands at
                // the right place.
                let _ = reader.seek(SeekFrom::Start(here_before + 8))?;
                ((end - here_before) as usize, 8)
            }
            1 => {
                let mut ext = [0u8; 8];
                reader.read_exact(&mut ext)?;
                (u64::from_be_bytes(ext) as usize, 16)
            }
            n => (n as usize, 8),
        };
        let body_size = total.checked_sub(header_size).ok_or_else(|| {
            parse_err(format!(
                "box {this_kind:?} total {total} < header {header_size}"
            ))
        })?;
        if &this_kind == kind {
            let mut body = vec![0u8; body_size];
            reader.read_exact(&mut body)?;
            return Ok(body);
        } else {
            reader.seek(SeekFrom::Current(body_size as i64))?;
        }
    }
}

/// Read up to `buf.len()` bytes; returns the number read. `0` means
/// EOF on the first byte (no header at all).
fn read_full_or_eof<R: std::io::Read>(reader: &mut R, buf: &mut [u8]) -> IsobmffResult<usize> {
    let mut got = 0;
    while got < buf.len() {
        match reader.read(&mut buf[got..]) {
            Ok(0) => return Ok(got),
            Ok(n) => got += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(IsobmffError::Heif(format!("read: {e}"))),
        }
    }
    Ok(got)
}

// ─────────────────────────── walker ───────────────────────────

struct BoxHeader {
    kind: [u8; 4],
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn has_remaining(&self) -> bool {
        self.pos < self.buf.len()
    }
    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }
    fn read_slice(&mut self, n: usize) -> IsobmffResult<&'a [u8]> {
        if self.pos + n > self.buf.len() {
            return Err(parse_err(format!(
                "short read: need {n}, have {}",
                self.remaining()
            )));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn read_u8(&mut self) -> IsobmffResult<u8> {
        Ok(self.read_slice(1)?[0])
    }
    fn read_u16(&mut self) -> IsobmffResult<u16> {
        let s = self.read_slice(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn read_u32(&mut self) -> IsobmffResult<u32> {
        let s = self.read_slice(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn read_i32(&mut self) -> IsobmffResult<i32> {
        Ok(self.read_u32()? as i32)
    }
    fn read_u64(&mut self) -> IsobmffResult<u64> {
        let s = self.read_slice(8)?;
        Ok(u64::from_be_bytes([
            s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
        ]))
    }
    fn read_array<const N: usize>(&mut self) -> IsobmffResult<[u8; N]> {
        let s = self.read_slice(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(s);
        Ok(out)
    }
    fn skip(&mut self, n: usize) -> IsobmffResult<()> {
        let _ = self.read_slice(n)?;
        Ok(())
    }
    fn read_box(&mut self) -> IsobmffResult<(BoxHeader, &'a [u8])> {
        let size32 = self.read_u32()?;
        let kind = self.read_array::<4>()?;
        let (total, header_size) = match size32 {
            0 => (self.remaining() + 8, 8),
            1 => (self.read_u64()? as usize, 16),
            n => (n as usize, 8),
        };
        let body_size = total.checked_sub(header_size).ok_or_else(|| {
            parse_err(format!(
                "box {:?} total {total} < header {header_size}",
                kind
            ))
        })?;
        let body = self.read_slice(body_size)?;
        Ok((BoxHeader { kind }, body))
    }
}

/// Locate a top-level box by fourcc, returning its body slice. Scans
/// in box order; returns `Ok(None)` if absent.
fn find_top_level_box<'a>(bytes: &'a [u8], wanted: &[u8; 4]) -> IsobmffResult<Option<&'a [u8]>> {
    let mut cur = Cursor::new(bytes);
    while cur.has_remaining() {
        let (hdr, body) = cur.read_box()?;
        if &hdr.kind == wanted {
            return Ok(Some(body));
        }
    }
    Ok(None)
}

fn find_child<'a>(parent: &'a [u8], wanted: &[u8; 4]) -> IsobmffResult<Option<&'a [u8]>> {
    let mut cur = Cursor::new(parent);
    while cur.has_remaining() {
        let (hdr, body) = cur.read_box()?;
        if &hdr.kind == wanted {
            return Ok(Some(body));
        }
    }
    Ok(None)
}

fn require_child<'a>(parent: &'a [u8], wanted: &[u8; 4]) -> IsobmffResult<&'a [u8]> {
    find_child(parent, wanted)?
        .ok_or_else(|| parse_err(format!("missing required child box {:?}", wanted)))
}

fn parse_err(msg: impl Into<String>) -> IsobmffError {
    // Reuse the heif variant — it's the existing parse-error channel
    // in IsobmffError and renaming is out of scope for this work.
    IsobmffError::Heif(msg.into())
}

// ─────────────────────────── trak ───────────────────────────

fn try_parse_video_trak(trak: &[u8]) -> IsobmffResult<Option<VideoTrack>> {
    let tkhd = require_child(trak, b"tkhd")?;
    let mdia = require_child(trak, b"mdia")?;

    let hdlr = require_child(mdia, b"hdlr")?;
    if !is_video_handler(hdlr)? {
        return Ok(None);
    }

    let (width, height) = parse_tkhd_dims(tkhd)?;
    let mdhd = require_child(mdia, b"mdhd")?;
    let (timescale, duration) = parse_mdhd(mdhd)?;

    let minf = require_child(mdia, b"minf")?;
    let stbl = require_child(minf, b"stbl")?;

    let stsd = require_child(stbl, b"stsd")?;
    let (codec, codec_config, sw, sh) = parse_stsd_visual(stsd)?;

    // `tkhd` carries display dimensions (may include rotation); the
    // visual sample entry has the encoded dimensions. Prefer the
    // sample-entry dims — that's what the decoder consumes — falling
    // back to tkhd if the sample entry omitted them.
    let width = if sw > 0 { sw } else { width };
    let height = if sh > 0 { sh } else { height };

    let samples = build_sample_table(stbl)?;

    Ok(Some(VideoTrack {
        codec,
        codec_config,
        width,
        height,
        timescale,
        duration,
        samples,
    }))
}

fn is_video_handler(hdlr: &[u8]) -> IsobmffResult<bool> {
    // FullBox: version(1) + flags(3)
    // pre_defined u32, handler_type [u8;4], reserved[3]u32, name (utf8 + 0).
    let mut c = Cursor::new(hdlr);
    c.skip(4)?; // version + flags
    c.skip(4)?; // pre_defined
    let handler = c.read_array::<4>()?;
    Ok(&handler == b"vide")
}

fn parse_tkhd_dims(tkhd: &[u8]) -> IsobmffResult<(u16, u16)> {
    let mut c = Cursor::new(tkhd);
    let version = c.read_u8()?;
    c.skip(3)?; // flags
    let datetime_w = if version == 1 { 8 } else { 4 };
    c.skip(datetime_w * 2)?; // creation + modification time
    c.skip(4)?; // track_id
    c.skip(4)?; // reserved
    c.skip(datetime_w)?; // duration
    c.skip(8)?; // reserved[2] u32
    c.skip(2)?; // layer
    c.skip(2)?; // alternate_group
    c.skip(2)?; // volume
    c.skip(2)?; // reserved
    c.skip(9 * 4)?; // matrix
    // width / height fixed 16.16 — take integer part.
    let w_fp = c.read_u32()?;
    let h_fp = c.read_u32()?;
    Ok(((w_fp >> 16) as u16, (h_fp >> 16) as u16))
}

fn parse_mdhd(mdhd: &[u8]) -> IsobmffResult<(u32, u64)> {
    let mut c = Cursor::new(mdhd);
    let version = c.read_u8()?;
    c.skip(3)?;
    if version == 1 {
        c.skip(8 + 8)?; // creation, modification
        let timescale = c.read_u32()?;
        let duration = c.read_u64()?;
        Ok((timescale, duration))
    } else {
        c.skip(4 + 4)?;
        let timescale = c.read_u32()?;
        let duration = c.read_u32()? as u64;
        Ok((timescale, duration))
    }
}

// ─────────────────────────── stsd ───────────────────────────

fn parse_stsd_visual(stsd: &[u8]) -> IsobmffResult<(VideoCodec, Vec<u8>, u16, u16)> {
    let mut c = Cursor::new(stsd);
    c.skip(4)?; // version + flags
    let entry_count = c.read_u32()?;
    if entry_count == 0 {
        return Err(parse_err("stsd entry_count = 0"));
    }
    // First entry only — multiple visual sample entries in one trak
    // would imply codec switches mid-track, which we don't support.
    let (hdr, body) = c.read_box()?;
    let codec = match &hdr.kind {
        b"avc1" | b"avc3" => VideoCodec::Avc,
        b"hvc1" | b"hev1" => VideoCodec::Hevc,
        b"av01" => VideoCodec::Av1,
        other => {
            return Err(parse_err(format!(
                "unsupported visual sample entry: {:?}",
                std::str::from_utf8(other).unwrap_or("?")
            )));
        }
    };
    let (config, w, h) = parse_visual_sample_entry(codec, body)?;
    Ok((codec, config, w, h))
}

fn parse_visual_sample_entry(codec: VideoCodec, body: &[u8]) -> IsobmffResult<(Vec<u8>, u16, u16)> {
    let mut c = Cursor::new(body);
    // SampleEntry: reserved[6] + data_reference_index u16 = 8 bytes.
    c.skip(8)?;
    // VisualSampleEntry: pre_defined u16 + reserved u16 + pre_defined[3] u32
    // = 4 + 12 = 16 bytes.
    c.skip(16)?;
    let width = c.read_u16()?;
    let height = c.read_u16()?;
    c.skip(4 + 4)?; // horizresolution + vertresolution
    c.skip(4)?; // reserved
    c.skip(2)?; // frame_count
    c.skip(32)?; // compressorname
    c.skip(2)?; // depth
    c.skip(2)?; // pre_defined (-1)

    let want = match codec {
        VideoCodec::Avc => b"avcC",
        VideoCodec::Hevc => b"hvcC",
        VideoCodec::Av1 => b"av1C",
    };
    let mut config = None;
    while c.has_remaining() {
        let (hdr, body) = c.read_box()?;
        if &hdr.kind == want {
            config = Some(body.to_vec());
            break;
        }
    }
    let config = config.ok_or_else(|| {
        parse_err(format!(
            "visual sample entry missing config box {:?}",
            std::str::from_utf8(want).unwrap_or("?")
        ))
    })?;
    Ok((config, width, height))
}

// ─────────────────────────── audio trak ───────────────────────────

fn try_parse_audio_trak(trak: &[u8]) -> IsobmffResult<Option<AudioTrack>> {
    let mdia = require_child(trak, b"mdia")?;
    let hdlr = require_child(mdia, b"hdlr")?;
    if !is_audio_handler(hdlr)? {
        return Ok(None);
    }
    let mdhd = require_child(mdia, b"mdhd")?;
    let (timescale, duration) = parse_mdhd(mdhd)?;

    let minf = require_child(mdia, b"minf")?;
    let stbl = require_child(minf, b"stbl")?;
    let stsd = require_child(stbl, b"stsd")?;
    let (codec, codec_config, channel_count, sample_rate) = parse_stsd_audio(stsd)?;

    let samples = build_sample_table(stbl)?;

    Ok(Some(AudioTrack {
        codec,
        codec_config,
        channel_count,
        sample_rate,
        timescale,
        duration,
        samples,
    }))
}

fn is_audio_handler(hdlr: &[u8]) -> IsobmffResult<bool> {
    let mut c = Cursor::new(hdlr);
    c.skip(4)?; // version + flags
    c.skip(4)?; // pre_defined
    let handler = c.read_array::<4>()?;
    Ok(&handler == b"soun")
}

fn parse_stsd_audio(stsd: &[u8]) -> IsobmffResult<(AudioCodec, Vec<u8>, u16, u32)> {
    let mut c = Cursor::new(stsd);
    c.skip(4)?; // version + flags
    let entry_count = c.read_u32()?;
    if entry_count == 0 {
        return Err(parse_err("audio stsd entry_count = 0"));
    }
    let (hdr, body) = c.read_box()?;
    // AudioSampleEntry header: reserved[6] + data_reference_index u16
    // + reserved u64 + channel_count u16 + sample_size u16 +
    // pre_defined u16 + reserved u16 + sample_rate u32 (16.16 fixed,
    // upper 16 bits = integer Hz).
    let mut bc = Cursor::new(body);
    bc.skip(6)?; // reserved
    bc.skip(2)?; // data_reference_index
    bc.skip(8)?; // reserved (version+reserved+predefined)
    let channel_count = bc.read_u16()?;
    bc.skip(2)?; // sample_size
    bc.skip(2)?; // pre_defined
    bc.skip(2)?; // reserved
    let sample_rate_fixed = bc.read_u32()?;
    let sample_rate = sample_rate_fixed >> 16;
    let after_audio_header = body.len() - bc.remaining();

    // The remaining bytes of `body` are the codec-specific child boxes
    // (esds for mp4a, dfLa for fLaC, ...).
    let children = &body[after_audio_header..];

    match &hdr.kind {
        b"mp4a" => {
            let esds = find_child(children, b"esds")?
                .ok_or_else(|| parse_err("mp4a sample entry missing esds"))?;
            let (oti, asc) = parse_esds_asc(esds)?;
            let codec = match oti {
                0x40 => AudioCodec::Aac,
                0x69 | 0x6B => AudioCodec::Mp3,
                other => {
                    return Err(parse_err(format!(
                        "mp4a esds: unsupported object_type_indication 0x{other:02X}"
                    )));
                }
            };
            Ok((codec, asc, channel_count, sample_rate))
        }
        b"fLaC" | b"flac" => {
            let dfla = find_child(children, b"dfLa")?
                .ok_or_else(|| parse_err("fLaC sample entry missing dfLa"))?;
            Ok((AudioCodec::Flac, dfla.to_vec(), channel_count, sample_rate))
        }
        b".mp3" => Ok((AudioCodec::Mp3, Vec::new(), channel_count, sample_rate)),
        other => Err(parse_err(format!(
            "unsupported audio sample entry: {:?}",
            std::str::from_utf8(other).unwrap_or("?")
        ))),
    }
}

/// Parse `esds` → (object_type_indication, AudioSpecificConfig bytes).
/// Walks the nested ES descriptor tree, tolerating either the compact
/// or the 4-byte-expandable length encoding. The expandable encoding
/// uses `0x80 0x80 0x80 LEN` (4 bytes) or `0x81 LEN` etc.
fn parse_esds_asc(esds: &[u8]) -> IsobmffResult<(u8, Vec<u8>)> {
    let mut c = Cursor::new(esds);
    c.skip(4)?; // FullBox version + flags

    // ES_Descriptor (tag 0x03).
    let tag = c.read_u8()?;
    if tag != 0x03 {
        return Err(parse_err(format!(
            "esds: expected ES_Descriptor tag 0x03, got 0x{tag:02X}"
        )));
    }
    skip_descriptor_length(&mut c)?;
    c.skip(2)?; // ES_ID
    let flags = c.read_u8()?;
    let stream_dep = (flags & 0x80) != 0;
    let url_flag = (flags & 0x40) != 0;
    let ocr_flag = (flags & 0x20) != 0;
    if stream_dep {
        c.skip(2)?;
    }
    if url_flag {
        let url_len = c.read_u8()? as usize;
        c.skip(url_len)?;
    }
    if ocr_flag {
        c.skip(2)?;
    }

    // DecoderConfigDescriptor (tag 0x04).
    let tag = c.read_u8()?;
    if tag != 0x04 {
        return Err(parse_err(format!(
            "esds: expected DecoderConfigDescriptor tag 0x04, got 0x{tag:02X}"
        )));
    }
    skip_descriptor_length(&mut c)?;
    let oti = c.read_u8()?;
    c.skip(1)?; // streamType + upStream + reserved
    c.skip(3)?; // bufferSizeDB (u24)
    c.skip(4)?; // maxBitrate
    c.skip(4)?; // avgBitrate

    // DecoderSpecificInfo (tag 0x05) — the AudioSpecificConfig blob.
    let tag = c.read_u8()?;
    if tag != 0x05 {
        // Some encoders omit DecSpecificInfo (especially for MP3
        // where the stream is self-describing) — return empty config.
        return Ok((oti, Vec::new()));
    }
    let asc_len = read_descriptor_length(&mut c)?;
    let asc = c.read_slice(asc_len)?.to_vec();
    Ok((oti, asc))
}

/// Read the variable-length descriptor length field (MPEG-4 expandable
/// encoding). The high bit of each byte marks "more bytes follow"; the
/// low 7 bits accumulate into the length. Up to 4 bytes per spec.
fn read_descriptor_length(c: &mut Cursor<'_>) -> IsobmffResult<usize> {
    let mut len = 0usize;
    for _ in 0..4 {
        let b = c.read_u8()?;
        len = (len << 7) | (b & 0x7F) as usize;
        if (b & 0x80) == 0 {
            return Ok(len);
        }
    }
    Ok(len)
}

fn skip_descriptor_length(c: &mut Cursor<'_>) -> IsobmffResult<()> {
    let _ = read_descriptor_length(c)?;
    Ok(())
}

// ─────────────────────────── sample table ───────────────────────────

fn build_sample_table(stbl: &[u8]) -> IsobmffResult<Vec<SampleRef>> {
    let stts = require_child(stbl, b"stts")?;
    let stsc = require_child(stbl, b"stsc")?;
    let stsz = require_child(stbl, b"stsz")?;
    let stco = find_child(stbl, b"stco")?;
    let co64 = find_child(stbl, b"co64")?;
    let stss = find_child(stbl, b"stss")?;
    let ctts = find_child(stbl, b"ctts")?;

    let durations = parse_stts(stts)?;
    let sizes = parse_stsz(stsz)?;
    let chunks = parse_stsc(stsc)?;
    let chunk_offsets = match (stco, co64) {
        (Some(s), _) => parse_stco(s)?,
        (None, Some(s)) => parse_co64(s)?,
        (None, None) => return Err(parse_err("stbl missing stco/co64")),
    };
    let n = sizes.len();
    if durations.len() != n {
        return Err(parse_err(format!(
            "stts samples ({}) != stsz samples ({})",
            durations.len(),
            n
        )));
    }
    let keyframes_flags = build_keyframe_flags(stss, n)?;
    let cts_offsets = build_cts_offsets(ctts, n)?;
    let sample_offsets = compute_sample_offsets(&chunks, &chunk_offsets, &sizes)?;

    let mut out = Vec::with_capacity(n);
    let mut dts: u64 = 0;
    for i in 0..n {
        let s = SampleRef {
            offset: sample_offsets[i],
            size: sizes[i],
            dts,
            duration: durations[i],
            cts_offset: cts_offsets[i],
            is_keyframe: keyframes_flags[i],
        };
        dts = dts.saturating_add(durations[i] as u64);
        out.push(s);
    }
    Ok(out)
}

/// `stts` → per-sample duration table (decompressed run-length).
fn parse_stts(stts: &[u8]) -> IsobmffResult<Vec<u32>> {
    let mut c = Cursor::new(stts);
    c.skip(4)?; // version + flags
    let entry_count = c.read_u32()?;
    let mut out = Vec::new();
    for _ in 0..entry_count {
        let count = c.read_u32()?;
        let delta = c.read_u32()?;
        for _ in 0..count {
            out.push(delta);
        }
    }
    Ok(out)
}

/// `stsz` → per-sample size table. `sample_size != 0` → all samples
/// share that size.
fn parse_stsz(stsz: &[u8]) -> IsobmffResult<Vec<u32>> {
    let mut c = Cursor::new(stsz);
    c.skip(4)?; // version + flags
    let sample_size = c.read_u32()?;
    let sample_count = c.read_u32()?;
    if sample_size != 0 {
        return Ok(vec![sample_size; sample_count as usize]);
    }
    let mut out = Vec::with_capacity(sample_count as usize);
    for _ in 0..sample_count {
        out.push(c.read_u32()?);
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy)]
struct StscRun {
    first_chunk: u32,
    samples_per_chunk: u32,
    /// Sample description index — unused by the byte-offset machinery,
    /// kept around so future readers can validate it matches 1.
    _sample_description_index: u32,
}

fn parse_stsc(stsc: &[u8]) -> IsobmffResult<Vec<StscRun>> {
    let mut c = Cursor::new(stsc);
    c.skip(4)?;
    let entry_count = c.read_u32()?;
    let mut out = Vec::with_capacity(entry_count as usize);
    for _ in 0..entry_count {
        let first_chunk = c.read_u32()?;
        let samples_per_chunk = c.read_u32()?;
        let sample_description_index = c.read_u32()?;
        out.push(StscRun {
            first_chunk,
            samples_per_chunk,
            _sample_description_index: sample_description_index,
        });
    }
    Ok(out)
}

fn parse_stco(stco: &[u8]) -> IsobmffResult<Vec<u64>> {
    let mut c = Cursor::new(stco);
    c.skip(4)?;
    let entry_count = c.read_u32()?;
    let mut out = Vec::with_capacity(entry_count as usize);
    for _ in 0..entry_count {
        out.push(c.read_u32()? as u64);
    }
    Ok(out)
}

fn parse_co64(co64: &[u8]) -> IsobmffResult<Vec<u64>> {
    let mut c = Cursor::new(co64);
    c.skip(4)?;
    let entry_count = c.read_u32()?;
    let mut out = Vec::with_capacity(entry_count as usize);
    for _ in 0..entry_count {
        out.push(c.read_u64()?);
    }
    Ok(out)
}

fn build_keyframe_flags(stss: Option<&[u8]>, n: usize) -> IsobmffResult<Vec<bool>> {
    let Some(stss) = stss else {
        // Per ISO/IEC 14496-12 §8.6.2 — all samples are sync samples
        // when stss is absent.
        return Ok(vec![true; n]);
    };
    let mut c = Cursor::new(stss);
    c.skip(4)?;
    let entry_count = c.read_u32()?;
    let mut flags = vec![false; n];
    for _ in 0..entry_count {
        let one_based = c.read_u32()? as usize;
        if one_based == 0 || one_based > n {
            return Err(parse_err(format!(
                "stss sample_number {one_based} out of range 1..={n}"
            )));
        }
        flags[one_based - 1] = true;
    }
    Ok(flags)
}

fn build_cts_offsets(ctts: Option<&[u8]>, n: usize) -> IsobmffResult<Vec<i32>> {
    let Some(ctts) = ctts else {
        return Ok(vec![0; n]);
    };
    let mut c = Cursor::new(ctts);
    let version = c.read_u8()?;
    c.skip(3)?;
    let entry_count = c.read_u32()?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..entry_count {
        let count = c.read_u32()?;
        let offset = if version == 1 {
            c.read_i32()?
        } else {
            c.read_u32()? as i32
        };
        for _ in 0..count {
            if out.len() < n {
                out.push(offset);
            }
        }
    }
    out.resize(n, 0);
    Ok(out)
}

/// Walk `stsc` to compute per-sample byte offsets, given chunk byte
/// offsets (`stco`/`co64`) and per-sample sizes (`stsz`).
fn compute_sample_offsets(
    stsc: &[StscRun],
    chunk_offsets: &[u64],
    sizes: &[u32],
) -> IsobmffResult<Vec<u64>> {
    if stsc.is_empty() {
        return Err(parse_err("stsc has no entries"));
    }
    let mut sample_offsets = Vec::with_capacity(sizes.len());
    let mut sample_idx = 0usize;
    let nchunks = chunk_offsets.len() as u32;
    for run_idx in 0..stsc.len() {
        let run = stsc[run_idx];
        let next_first_chunk = if run_idx + 1 < stsc.len() {
            stsc[run_idx + 1].first_chunk
        } else {
            nchunks + 1
        };
        for chunk_one_based in run.first_chunk..next_first_chunk {
            let chunk_zero_based = (chunk_one_based - 1) as usize;
            if chunk_zero_based >= chunk_offsets.len() {
                // stsc declared more chunks than stco knows about;
                // stop emitting — remaining samples are unreachable.
                break;
            }
            let mut off = chunk_offsets[chunk_zero_based];
            for _ in 0..run.samples_per_chunk {
                if sample_idx >= sizes.len() {
                    return Ok(sample_offsets);
                }
                sample_offsets.push(off);
                off = off.saturating_add(sizes[sample_idx] as u64);
                sample_idx += 1;
            }
        }
    }
    if sample_offsets.len() != sizes.len() {
        return Err(parse_err(format!(
            "stsc/stco walk produced {} sample offsets, expected {} (stsz)",
            sample_offsets.len(),
            sizes.len()
        )));
    }
    Ok(sample_offsets)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_with_body(four_cc: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let total = 8 + body.len();
        let mut v = Vec::with_capacity(total);
        v.extend_from_slice(&(total as u32).to_be_bytes());
        v.extend_from_slice(four_cc);
        v.extend_from_slice(body);
        v
    }

    /// Synthesize a minimal H.264 mp4 with one trak, two samples, one
    /// keyframe + one delta. Sample 0 starts at offset 16 (after ftyp
    /// and the start of mdat). Sample 1 starts at offset 16 + 3.
    fn synth_minimal_avc_mp4() -> Vec<u8> {
        // Order: ftyp (size 16), moov (...), mdat (size 8 + payload).
        // Sample bytes live inside mdat at offset (file_offset_of_mdat + 8).
        let ftyp = {
            let mut body = Vec::new();
            body.extend_from_slice(b"isom");
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(b"isom");
            box_with_body(b"ftyp", &body)
        };

        // mdhd v0: timescale 90000, duration 6000 (~2 samples @ 33ms).
        let mdhd = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]); // version + flags
            body.extend_from_slice(&0u32.to_be_bytes()); // creation
            body.extend_from_slice(&0u32.to_be_bytes()); // modification
            body.extend_from_slice(&90_000u32.to_be_bytes()); // timescale
            body.extend_from_slice(&6_000u32.to_be_bytes()); // duration
            body.extend_from_slice(&0u16.to_be_bytes()); // lang
            body.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
            box_with_body(b"mdhd", &body)
        };

        let hdlr = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(b"vide");
            body.extend_from_slice(&[0u8; 12]); // reserved
            body.push(0); // name (single NUL)
            box_with_body(b"hdlr", &body)
        };

        // avc1 sample entry with a stub avcC.
        let avcc = box_with_body(b"avcC", &[0x01, 0x42, 0xC0, 0x1F, 0xFF, 0xE1, 0x00, 0x05]);
        let avc1 = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0u8; 6]); // reserved
            body.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
            body.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
            body.extend_from_slice(&0u16.to_be_bytes()); // reserved
            body.extend_from_slice(&[0u8; 12]); // pre_defined[3]
            body.extend_from_slice(&1920u16.to_be_bytes()); // width
            body.extend_from_slice(&1080u16.to_be_bytes()); // height
            body.extend_from_slice(&0u32.to_be_bytes()); // horiz
            body.extend_from_slice(&0u32.to_be_bytes()); // vert
            body.extend_from_slice(&0u32.to_be_bytes()); // reserved
            body.extend_from_slice(&1u16.to_be_bytes()); // frame_count
            body.extend_from_slice(&[0u8; 32]); // compressorname
            body.extend_from_slice(&24u16.to_be_bytes()); // depth
            body.extend_from_slice(&0xFFFFu16.to_be_bytes()); // pre_defined (-1)
            body.extend_from_slice(&avcc);
            box_with_body(b"avc1", &body)
        };
        let stsd = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&1u32.to_be_bytes());
            body.extend_from_slice(&avc1);
            box_with_body(b"stsd", &body)
        };

        // stts: 2 samples, duration 3000 each.
        let stts = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&1u32.to_be_bytes()); // entry count
            body.extend_from_slice(&2u32.to_be_bytes()); // sample count
            body.extend_from_slice(&3000u32.to_be_bytes()); // delta
            box_with_body(b"stts", &body)
        };
        // stss: sample 1 is sync.
        let stss = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&1u32.to_be_bytes()); // entry count
            body.extend_from_slice(&1u32.to_be_bytes()); // sample_number (1-based)
            box_with_body(b"stss", &body)
        };
        // stsc: 1 chunk, 2 samples in it.
        let stsc = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&1u32.to_be_bytes());
            body.extend_from_slice(&1u32.to_be_bytes()); // first_chunk
            body.extend_from_slice(&2u32.to_be_bytes()); // samples_per_chunk
            body.extend_from_slice(&1u32.to_be_bytes()); // sample_description_index
            box_with_body(b"stsc", &body)
        };
        // stsz: 2 samples, sizes 3 and 4 (variable).
        let stsz = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&0u32.to_be_bytes()); // sample_size (variable)
            body.extend_from_slice(&2u32.to_be_bytes()); // sample_count
            body.extend_from_slice(&3u32.to_be_bytes());
            body.extend_from_slice(&4u32.to_be_bytes());
            box_with_body(b"stsz", &body)
        };

        // stco — we won't know mdat's file offset until we lay everything
        // out. Compute lengths first to back-fill.
        let stbl_inner_without_stco = [&stsd, &stts, &stss, &stsc, &stsz]
            .iter()
            .map(|b| b.len())
            .sum::<usize>();
        // Mdat starts AFTER ftyp + moov. moov = mvhd + trak + 8 (header).
        // We'll compute mdat_offset after building the rest.

        // Placeholder; fix below.
        let stco_placeholder = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&1u32.to_be_bytes());
            body.extend_from_slice(&0u32.to_be_bytes()); // first chunk offset
            box_with_body(b"stco", &body)
        };

        let stbl_len = stbl_inner_without_stco + stco_placeholder.len() + 8;
        let dinf = {
            let dref_body = {
                let url = box_with_body(b"url ", &[0, 0, 0, 1]); // self-contained flag
                let mut body = Vec::new();
                body.extend_from_slice(&[0, 0, 0, 0]);
                body.extend_from_slice(&1u32.to_be_bytes()); // entry count
                body.extend_from_slice(&url);
                body
            };
            let dref = box_with_body(b"dref", &dref_body);
            box_with_body(b"dinf", &dref)
        };
        // minf wraps dinf + stbl (and a vmhd we don't bother parsing).
        let minf_len = dinf.len() + stbl_len + 8;
        let mdia_len = mdhd.len() + hdlr.len() + minf_len + 8;
        let trak_inner_tkhd = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&0u32.to_be_bytes()); // creation
            body.extend_from_slice(&0u32.to_be_bytes()); // modification
            body.extend_from_slice(&1u32.to_be_bytes()); // track_id
            body.extend_from_slice(&0u32.to_be_bytes()); // reserved
            body.extend_from_slice(&6_000u32.to_be_bytes()); // duration
            body.extend_from_slice(&[0u8; 8]); // reserved[2]
            body.extend_from_slice(&0u16.to_be_bytes()); // layer
            body.extend_from_slice(&0u16.to_be_bytes()); // alt group
            body.extend_from_slice(&0u16.to_be_bytes()); // volume
            body.extend_from_slice(&0u16.to_be_bytes()); // reserved
            body.extend_from_slice(&[0u8; 36]); // matrix[9]
            body.extend_from_slice(&(1920u32 << 16).to_be_bytes()); // width fp
            body.extend_from_slice(&(1080u32 << 16).to_be_bytes()); // height fp
            box_with_body(b"tkhd", &body)
        };
        let trak_len = trak_inner_tkhd.len() + mdia_len + 8;
        let mvhd = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&0u32.to_be_bytes()); // creation
            body.extend_from_slice(&0u32.to_be_bytes()); // modification
            body.extend_from_slice(&90_000u32.to_be_bytes()); // timescale
            body.extend_from_slice(&6_000u32.to_be_bytes()); // duration
            body.extend_from_slice(&(1u32 << 16).to_be_bytes()); // rate fp 1.0
            body.extend_from_slice(&(1u16 << 8).to_be_bytes()); // volume fp 1.0
            body.extend_from_slice(&[0u8; 10]); // reserved
            body.extend_from_slice(&[0u8; 36]); // matrix[9]
            body.extend_from_slice(&[0u8; 24]); // pre_defined[6]
            body.extend_from_slice(&2u32.to_be_bytes()); // next_track_id
            box_with_body(b"mvhd", &body)
        };
        let moov_len = mvhd.len() + trak_len + 8;

        // File: ftyp + moov + mdat.
        // mdat header (size + 'mdat') = 8 bytes; samples start at
        // mdat_offset + 8.
        let mdat_offset = ftyp.len() + moov_len;
        let chunk_offset = (mdat_offset + 8) as u32;

        let stco = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&1u32.to_be_bytes());
            body.extend_from_slice(&chunk_offset.to_be_bytes());
            box_with_body(b"stco", &body)
        };
        assert_eq!(stco.len(), stco_placeholder.len());

        let stbl = {
            let mut body = Vec::new();
            body.extend_from_slice(&stsd);
            body.extend_from_slice(&stts);
            body.extend_from_slice(&stss);
            body.extend_from_slice(&stsc);
            body.extend_from_slice(&stsz);
            body.extend_from_slice(&stco);
            box_with_body(b"stbl", &body)
        };
        assert_eq!(stbl.len(), stbl_len);
        let minf = {
            let mut body = Vec::new();
            body.extend_from_slice(&dinf);
            body.extend_from_slice(&stbl);
            box_with_body(b"minf", &body)
        };
        assert_eq!(minf.len(), minf_len);
        let mdia = {
            let mut body = Vec::new();
            body.extend_from_slice(&mdhd);
            body.extend_from_slice(&hdlr);
            body.extend_from_slice(&minf);
            box_with_body(b"mdia", &body)
        };
        assert_eq!(mdia.len(), mdia_len);
        let trak = {
            let mut body = Vec::new();
            body.extend_from_slice(&trak_inner_tkhd);
            body.extend_from_slice(&mdia);
            box_with_body(b"trak", &body)
        };
        assert_eq!(trak.len(), trak_len);
        let moov = {
            let mut body = Vec::new();
            body.extend_from_slice(&mvhd);
            body.extend_from_slice(&trak);
            box_with_body(b"moov", &body)
        };
        assert_eq!(moov.len(), moov_len);

        // mdat with 7 bytes of payload (sample 0 = 3 bytes, sample 1 = 4).
        let mdat = box_with_body(b"mdat", b"abcdefg");

        let mut out = Vec::new();
        out.extend_from_slice(&ftyp);
        out.extend_from_slice(&moov);
        out.extend_from_slice(&mdat);
        out
    }

    #[test]
    fn parses_minimal_avc() {
        let buf = synth_minimal_avc_mp4();
        let track = parse_video_track(&buf).unwrap();
        assert_eq!(track.codec, VideoCodec::Avc);
        assert_eq!(track.width, 1920);
        assert_eq!(track.height, 1080);
        assert_eq!(track.timescale, 90_000);
        assert_eq!(track.duration, 6_000);
        assert_eq!(
            track.codec_config,
            vec![0x01, 0x42, 0xC0, 0x1F, 0xFF, 0xE1, 0x00, 0x05]
        );
        assert_eq!(track.samples.len(), 2);
        assert_eq!(track.samples[0].size, 3);
        assert_eq!(track.samples[0].dts, 0);
        assert_eq!(track.samples[0].duration, 3_000);
        assert!(track.samples[0].is_keyframe);
        assert_eq!(track.samples[1].size, 4);
        assert_eq!(track.samples[1].dts, 3_000);
        assert!(!track.samples[1].is_keyframe);
        // Verify the byte offsets actually point into the synthesized mdat.
        let s0 = &buf[track.samples[0].offset as usize
            ..(track.samples[0].offset + track.samples[0].size as u64) as usize];
        let s1 = &buf[track.samples[1].offset as usize
            ..(track.samples[1].offset + track.samples[1].size as u64) as usize];
        assert_eq!(s0, b"abc");
        assert_eq!(s1, b"defg");
    }

    #[test]
    fn keyframe_index_for_time_picks_sync_sample() {
        let buf = synth_minimal_avc_mp4();
        let track = parse_video_track(&buf).unwrap();
        // Only sample 0 is sync. Any query should return it.
        assert_eq!(track.keyframe_index_for_time(0), Some(0));
        assert_eq!(track.keyframe_index_for_time(2_999), Some(0));
        assert_eq!(track.keyframe_index_for_time(5_999), Some(0));
    }

    /// Synthesize a minimal `moov` containing one AAC audio trak.
    /// 1 sample, 1024 frames, OTI 0x40, AudioSpecificConfig = `0x11 0x90`
    /// (AAC-LC / 48 kHz / stereo). The sample-table offsets point to
    /// fake byte 16 — the parser doesn't read sample bytes itself.
    fn synth_minimal_aac_moov() -> Vec<u8> {
        // mdhd: timescale 48000, duration 1024.
        let mdhd = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(&48_000u32.to_be_bytes());
            body.extend_from_slice(&1024u32.to_be_bytes());
            body.extend_from_slice(&0u16.to_be_bytes());
            body.extend_from_slice(&0u16.to_be_bytes());
            box_with_body(b"mdhd", &body)
        };
        let hdlr = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(b"soun");
            body.extend_from_slice(&[0u8; 12]);
            body.push(0);
            box_with_body(b"hdlr", &body)
        };

        // esds: ES_Descriptor (3) → DecoderConfig (4) → DecSpecific (5)
        // = 0x11 0x90. Compact-length form.
        let dec_specific_body = &[0x11u8, 0x90][..];
        let mut dec_specific = Vec::new();
        dec_specific.push(0x05);
        dec_specific.push(dec_specific_body.len() as u8);
        dec_specific.extend_from_slice(dec_specific_body);

        let mut dec_config_body = Vec::new();
        dec_config_body.push(0x40); // OTI AAC
        dec_config_body.push(0x15); // streamType<<2 | upStream | reserved
        dec_config_body.extend_from_slice(&[0u8; 3]); // bufferSizeDB
        dec_config_body.extend_from_slice(&0u32.to_be_bytes());
        dec_config_body.extend_from_slice(&0u32.to_be_bytes());
        dec_config_body.extend_from_slice(&dec_specific);
        let mut dec_config = Vec::new();
        dec_config.push(0x04);
        dec_config.push(dec_config_body.len() as u8);
        dec_config.extend_from_slice(&dec_config_body);

        let sl_config = vec![0x06u8, 0x01, 0x02];

        let mut es_body = Vec::new();
        es_body.extend_from_slice(&0u16.to_be_bytes()); // ES_ID
        es_body.push(0);
        es_body.extend_from_slice(&dec_config);
        es_body.extend_from_slice(&sl_config);
        let mut es_descriptor = Vec::new();
        es_descriptor.push(0x03);
        es_descriptor.push(es_body.len() as u8);
        es_descriptor.extend_from_slice(&es_body);

        let mut esds_body = Vec::new();
        esds_body.extend_from_slice(&[0, 0, 0, 0]); // FullBox header
        esds_body.extend_from_slice(&es_descriptor);
        let esds = box_with_body(b"esds", &esds_body);

        let mp4a = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0u8; 6]);
            body.extend_from_slice(&1u16.to_be_bytes()); // dref idx
            body.extend_from_slice(&[0u8; 8]);
            body.extend_from_slice(&2u16.to_be_bytes()); // channels
            body.extend_from_slice(&16u16.to_be_bytes()); // sample_size
            body.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
            body.extend_from_slice(&0u16.to_be_bytes()); // reserved
            body.extend_from_slice(&(48_000u32 << 16).to_be_bytes()); // sample_rate fp
            body.extend_from_slice(&esds);
            box_with_body(b"mp4a", &body)
        };
        let stsd = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&1u32.to_be_bytes());
            body.extend_from_slice(&mp4a);
            box_with_body(b"stsd", &body)
        };
        let stts = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&1u32.to_be_bytes());
            body.extend_from_slice(&1u32.to_be_bytes()); // sample_count
            body.extend_from_slice(&1024u32.to_be_bytes());
            box_with_body(b"stts", &body)
        };
        let stsc = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&1u32.to_be_bytes());
            body.extend_from_slice(&1u32.to_be_bytes());
            body.extend_from_slice(&1u32.to_be_bytes());
            body.extend_from_slice(&1u32.to_be_bytes());
            box_with_body(b"stsc", &body)
        };
        let stsz = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(&1u32.to_be_bytes());
            body.extend_from_slice(&500u32.to_be_bytes()); // size
            box_with_body(b"stsz", &body)
        };
        let stco = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&1u32.to_be_bytes());
            body.extend_from_slice(&16u32.to_be_bytes()); // dummy chunk offset
            box_with_body(b"stco", &body)
        };
        let stbl = {
            let mut body = Vec::new();
            for b in [&stsd, &stts, &stsc, &stsz, &stco] {
                body.extend_from_slice(b);
            }
            box_with_body(b"stbl", &body)
        };
        let dinf = {
            let dref_body = {
                let url = box_with_body(b"url ", &[0, 0, 0, 1]);
                let mut body = Vec::new();
                body.extend_from_slice(&[0, 0, 0, 0]);
                body.extend_from_slice(&1u32.to_be_bytes());
                body.extend_from_slice(&url);
                body
            };
            let dref = box_with_body(b"dref", &dref_body);
            box_with_body(b"dinf", &dref)
        };
        let minf = {
            let mut body = Vec::new();
            body.extend_from_slice(&dinf);
            body.extend_from_slice(&stbl);
            box_with_body(b"minf", &body)
        };
        let mdia = {
            let mut body = Vec::new();
            body.extend_from_slice(&mdhd);
            body.extend_from_slice(&hdlr);
            body.extend_from_slice(&minf);
            box_with_body(b"mdia", &body)
        };
        let tkhd = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(&1u32.to_be_bytes());
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(&1024u32.to_be_bytes());
            body.extend_from_slice(&[0u8; 8]);
            body.extend_from_slice(&0u16.to_be_bytes());
            body.extend_from_slice(&0u16.to_be_bytes());
            body.extend_from_slice(&(1u16 << 8).to_be_bytes()); // volume 1.0
            body.extend_from_slice(&0u16.to_be_bytes());
            body.extend_from_slice(&[0u8; 36]);
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(&0u32.to_be_bytes());
            box_with_body(b"tkhd", &body)
        };
        let trak = {
            let mut body = Vec::new();
            body.extend_from_slice(&tkhd);
            body.extend_from_slice(&mdia);
            box_with_body(b"trak", &body)
        };
        let mvhd = {
            let mut body = Vec::new();
            body.extend_from_slice(&[0, 0, 0, 0]);
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(&48_000u32.to_be_bytes());
            body.extend_from_slice(&1024u32.to_be_bytes());
            body.extend_from_slice(&(1u32 << 16).to_be_bytes());
            body.extend_from_slice(&(1u16 << 8).to_be_bytes());
            body.extend_from_slice(&[0u8; 10]);
            body.extend_from_slice(&[0u8; 36]);
            body.extend_from_slice(&[0u8; 24]);
            body.extend_from_slice(&2u32.to_be_bytes());
            box_with_body(b"mvhd", &body)
        };
        let mut body = Vec::new();
        body.extend_from_slice(&mvhd);
        body.extend_from_slice(&trak);
        box_with_body(b"moov", &body)
    }

    #[test]
    fn parses_minimal_aac_audio_track() {
        let moov = synth_minimal_aac_moov();
        let body = &moov[8..]; // strip outer moov header
        let track = parse_audio_track_from_moov(body).unwrap().unwrap();
        assert_eq!(track.codec, AudioCodec::Aac);
        assert_eq!(track.channel_count, 2);
        assert_eq!(track.sample_rate, 48_000);
        assert_eq!(track.timescale, 48_000);
        assert_eq!(track.duration, 1024);
        assert_eq!(track.codec_config, vec![0x11, 0x90]);
        assert_eq!(track.samples.len(), 1);
        assert_eq!(track.samples[0].duration, 1024);
    }

    #[test]
    fn parse_audio_track_returns_none_when_moov_has_only_video() {
        let buf = synth_minimal_avc_mp4();
        // Locate moov body in the file.
        let moov_pos = buf.windows(4).position(|w| w == b"moov").unwrap();
        let moov_size =
            u32::from_be_bytes(buf[moov_pos - 4..moov_pos].try_into().unwrap()) as usize;
        let moov_body = &buf[moov_pos + 4..moov_pos - 4 + moov_size];
        let track = parse_audio_track_from_moov(moov_body).unwrap();
        assert!(track.is_none());
    }
}
