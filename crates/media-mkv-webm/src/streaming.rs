//! Built-in bounded Read+Seek Matroska/WebM streaming. `open_streaming` uses safe
//! default budgets; `open_streaming_with_limits` configures them. Open discovers
//! Segment metadata by walking element boundaries, not a fixed-size head probe.
//! Unselected blocks are seek-skipped; selected packets use `next_frame_into`.
//! See `StreamingLimits` and `streaming/README.md` for the supported subset.

use std::io::{BufReader, Read, Seek, SeekFrom, Write};

use crate::demux::{parse_cues, parse_ebml_header, parse_info, parse_tracks};
use crate::ebml::{element::Reader, schema::ids, varint::read_vint_raw};
use crate::profile::DocType;
use crate::{Demuxer, Error, Result};

mod bounded;
use bounded::{BudgetReader, WalkBudget, checked_end};
pub use bounded::{StreamingLimits, open_streaming_with_limits};

/// A streaming MKV demuxer that owns its reader. Returned by
/// [`open_streaming`]; iterate frames with [`StreamingDemuxer::next_frame_into`].
pub struct StreamingDemuxer<R: Read + Seek> {
    pub demuxer: Demuxer,
    reader: BufReader<BudgetReader<R>>,
    /// Total file length, captured at open. Used to bound the segment
    /// when the Segment element declares unknown size.
    file_len: u64,
    /// Walker state — file position cursor + current cluster, if we're
    /// mid-cluster.
    walk: WalkState,
    /// When `Some(tracks)`, `next_frame` reads ONLY the track-VINT
    /// prefix of each block; blocks whose track number isn't in the
    /// allow-list have large payloads skipped via `Seek`, without
    /// allocating a payload Vec. Buffered read-ahead can still read a prefix.
    ///
    /// `None` (default) returns every block, matching the pre-filter
    /// behavior so the diff-vs-oracle tests stay valid.
    track_filter: Option<Vec<u64>>,
    budget: WalkBudget,
}

struct WalkState {
    pos: u64,
    cluster: Option<ClusterCursor>,
}

/// State for walking the child elements of one cluster directly off
/// the underlying reader. `pos`/`end` are absolute file offsets; the
/// `BufReader` is positioned at `pos` between calls so we never seek
/// inside a cluster (cluster children are read strictly sequentially).
struct ClusterCursor {
    /// Absolute file offset of the next cluster child element header.
    /// Equals the underlying reader's logical position between calls.
    pos: u64,
    /// Absolute file offset where the cluster's payload ends.
    end: u64,
    /// `Cluster->Timestamp` (already in segment ticks). Multiplied by
    /// the segment timestamp scale on each frame yield.
    ts_ticks: u64,
    has_timestamp: bool,
}

/// Frame metadata returned by [`StreamingDemuxer::next_frame_into`].
/// The frame's payload bytes have already been written to the caller-
/// supplied `&mut dyn Write` by the time this header is returned;
/// `data_size` is how many bytes ended up there.
///
/// `timestamp_ns` is presentation time (Matroska Block timestamp). For
/// codecs with reorder (H.264/HEVC B-frames), consecutive frames in
/// decode order have non-monotonic PTS — the muxer reconstructs DTS +
/// CTS offsets from `block_duration_ns` + `reference_block_count`.
#[derive(Debug, Clone)]
pub struct FrameHeader {
    pub track: u64,
    pub timestamp_ns: u64,
    pub is_keyframe: Option<bool>,
    pub is_invisible: bool,
    pub is_discardable: Option<bool>,
    /// Authoritative per-frame duration (ns) when the source declared
    /// `BlockDuration` inside the BlockGroup. SimpleBlock has no
    /// equivalent — `None` for those, and for BlockGroups that omit it.
    pub block_duration_ns: Option<u64>,
    /// Number of `ReferenceBlock` children inside the BlockGroup. 0 →
    /// keyframe (matches `is_keyframe = Some(true)`); 1 → P-frame; 2 →
    /// B-frame referencing both past and future. `None` for SimpleBlock.
    pub reference_block_count: Option<u8>,
    /// Bytes the demuxer streamed into the caller's writer for this
    /// frame (= block payload size − inner block header). Use this for
    /// the trun's `sample_size` without re-measuring.
    pub data_size: u32,
}

/// Frame returned by [`StreamingDemuxer::next_frame`]. Same shape as
/// [`FrameHeader`] but with the payload bytes owned in `data` — the
/// compatibility wrapper for callers that want the buffer materialized.
/// Prefer [`StreamingDemuxer::next_frame_into`] for the muxer path.
#[derive(Debug, Clone)]
pub struct OwnedFrame {
    pub track: u64,
    pub timestamp_ns: u64,
    pub is_keyframe: Option<bool>,
    pub is_invisible: bool,
    pub is_discardable: Option<bool>,
    /// Authoritative per-frame duration (ns) when the source declared
    /// `BlockDuration` inside the BlockGroup. SimpleBlock has no
    /// equivalent — `None` for those, and for BlockGroups that omit it.
    pub block_duration_ns: Option<u64>,
    /// Number of `ReferenceBlock` children inside the BlockGroup. 0 →
    /// keyframe (matches `is_keyframe = Some(true)`); 1 → P-frame; 2 →
    /// B-frame referencing both past and future. Useful for sizing
    /// decoder DPB and reconstructing DTS offsets when reordering.
    /// `None` for SimpleBlock (which encodes the key-vs-delta bit in
    /// flags but says nothing about prediction structure).
    pub reference_block_count: Option<u8>,
    pub data: Vec<u8>,
}

/// Open native bounded streaming with safe defaults. Uses Read+Seek, not Read-only.
pub fn open_streaming<R: Read + Seek>(reader: R) -> Result<StreamingDemuxer<R>> {
    open_streaming_with_limits(reader, StreamingLimits::default())
}

impl<R: Read + Seek> StreamingDemuxer<R> {
    /// Restrict frame emission to a specific allow-list of track
    /// numbers. Blocks for other tracks have their payload `Seek`'d
    /// past on disk instead of being read+allocated. Pass `None` (or
    /// don't call this) to keep the default "emit every block".
    ///
    /// Sets the policy for *subsequent* `next_frame` calls; the
    /// underlying file position is unchanged, so it's safe to call
    /// mid-walk.
    pub fn set_track_filter(&mut self, tracks: Option<Vec<u64>>) {
        self.track_filter = tracks;
    }

    /// Seek the frame cursor to a specific byte offset. Use with
    /// [`Demuxer::cues`] / `segment_payload_start` to jump to a
    /// cluster near a target timestamp.
    pub fn seek_to_byte(&mut self, pos: u64) -> Result<()> {
        if pos < self.demuxer.segment_payload_start as u64 || pos > self.file_len {
            return Err(Error::SizeOverflow);
        }
        self.walk.pos = pos;
        self.walk.cluster = None;
        self.reader.seek(SeekFrom::Start(pos))?;
        Ok(())
    }

    /// Jump the cursor to the cluster whose Cues entry is at or
    /// before `ts_ns` (for the given `track_number`, or any track if
    /// `track_number` is `None`).
    ///
    /// When no Cue qualifies (request lands before the first entry, or
    /// the file has no Cues at all), rewind to the start of the
    /// clusters region rather than leaving the cursor where it was —
    /// otherwise a `seek_to_time(0)` after an earlier walk leaves the
    /// cursor mid-stream, and the caller silently starts reading from
    /// whatever cluster the walk last touched (e.g. a probe pass).
    pub fn seek_to_time(&mut self, ts_ns: u64, track_number: Option<u64>) -> Result<()> {
        let cue = self
            .demuxer
            .cues
            .iter()
            .filter(|c| track_number.is_none_or(|t| c.track_number == t))
            .filter(|c| c.ts_ns <= ts_ns)
            .max_by_key(|c| c.ts_ns);
        let target = match cue {
            Some(c) => (self.demuxer.segment_payload_start as u64)
                .checked_add(c.cluster_position)
                .filter(|&p| p < self.demuxer.segment_end as u64)
                .ok_or(Error::SizeOverflow)?,
            None => self.demuxer.clusters_offset as u64,
        };
        self.seek_to_byte(target)?;
        Ok(())
    }

    /// Streaming-output equivalent of [`Self::next_frame`]: walks until
    /// the next target-track frame, writes its payload bytes directly
    /// into `out` via `io::copy`, and returns the frame's metadata.
    ///
    /// Zero per-frame allocations: the caller's `out` is the only
    /// buffer the payload bytes touch (plus `io::copy`'s 8 KB stack
    /// buffer that streams them through).
    pub fn next_frame_into(&mut self, out: &mut dyn Write) -> Result<Option<FrameHeader>> {
        // Filtered walks need compact coalesced headers, not a 64KiB prefix of
        // every unwanted packet. Keep the buffer and cumulative budgets across
        // filter changes/seeks; selected payload copies temporarily lift the cap.
        self.reader.get_mut().max_read = if self.track_filter.is_some() {
            64
        } else {
            usize::MAX
        };
        let segment_end = self.demuxer.segment_end as u64;
        let scale = self.demuxer.timestamp_scale_ns;
        loop {
            // ---- Outside a cluster: find the next CLUSTER element ----
            if self.walk.cluster.is_none() {
                if self.walk.pos >= segment_end || self.walk.pos >= self.file_len {
                    if self.reader.seek(SeekFrom::End(0))? != self.file_len {
                        return Err(Error::Malformed("file length changed during strict walk"));
                    }
                    return Ok(None);
                }
                let hdr = read_element_header_at(&mut self.reader, self.walk.pos)?;
                self.budget.element()?;
                checked_end(&hdr, segment_end)?;
                if hdr.id == ids::CLUSTER {
                    let payload_end = hdr.payload_end.unwrap_or(segment_end);
                    // Reader is already at hdr.payload_start (post-
                    // header read). Cluster children walk forward from
                    // there without any seek.
                    self.walk.pos = payload_end;
                    self.walk.cluster = Some(ClusterCursor {
                        pos: hdr.payload_start,
                        end: payload_end,
                        ts_ticks: 0,
                        has_timestamp: false,
                    });
                    continue;
                }
                // Non-cluster siblings — Cues, SeekHead, Tags, Void.
                // Just advance.
                self.walk.pos = hdr.payload_end.unwrap_or(segment_end);
                continue;
            }

            // ---- Inside a cluster: walk its child blocks ------------
            let done = {
                let c = self.walk.cluster.as_ref().unwrap();
                c.pos >= c.end
            };
            if done {
                self.walk.cluster = None;
                continue;
            }

            let outcome = {
                let c = self.walk.cluster.as_mut().unwrap();
                walk_cluster_child(
                    &mut self.reader,
                    c,
                    scale,
                    self.track_filter.as_deref(),
                    &mut self.budget,
                    out,
                )?
            };
            match outcome {
                ChildOutcome::Frame(h) => return Ok(Some(h)),
                ChildOutcome::Continue => continue,
            }
        }
    }

    /// Compatibility wrapper around [`Self::next_frame_into`] that
    /// materializes each frame's payload into an owned `Vec<u8>`. New
    /// muxer code should use `next_frame_into` directly so the bytes
    /// land in `mdat` without a per-frame allocation; this entry point
    /// stays for the diff-against-oracle tests and any caller that
    /// truly wants an owned buffer.
    pub fn next_frame(&mut self) -> Result<Option<OwnedFrame>> {
        let mut buf: Vec<u8> = Vec::new();
        let hdr = match self.next_frame_into(&mut buf)? {
            Some(h) => h,
            None => return Ok(None),
        };
        Ok(Some(OwnedFrame {
            track: hdr.track,
            timestamp_ns: hdr.timestamp_ns,
            is_keyframe: hdr.is_keyframe,
            is_invisible: hdr.is_invisible,
            is_discardable: hdr.is_discardable,
            block_duration_ns: hdr.block_duration_ns,
            reference_block_count: hdr.reference_block_count,
            data: buf,
        }))
    }
}

enum ChildOutcome {
    Frame(FrameHeader),
    Continue,
}

/// Pull the next child element from the cluster, streaming bytes off
/// `reader` as it goes. Yields a frame for SimpleBlock / BlockGroup;
/// returns `Continue` for Timestamp / Void / unknown children.
///
/// Invariant on entry / exit: `reader` is positioned at `cluster.pos`.
/// Cluster children are read strictly sequentially so the underlying
/// `BufReader` never has to discard buffered bytes mid-cluster.
fn walk_cluster_child<R: Read + Seek>(
    reader: &mut BufReader<BudgetReader<R>>,
    cluster: &mut ClusterCursor,
    scale: u64,
    track_filter: Option<&[u64]>,
    budget: &mut WalkBudget,
    out: &mut dyn Write,
) -> Result<ChildOutcome> {
    let hdr = read_element_header_streaming(reader, &mut cluster.pos)?;
    budget.element()?;
    checked_end(&hdr, cluster.end)?;
    let pe = hdr.payload_end.unwrap_or(cluster.end);
    let payload_len = pe.saturating_sub(cluster.pos);
    match hdr.id {
        ids::TIMESTAMP => {
            if cluster.has_timestamp {
                return Err(Error::Malformed("duplicate cluster timestamp"));
            }
            cluster.has_timestamp = true;
            cluster.ts_ticks = read_be_uint(reader, payload_len as usize)?;
            cluster.pos = pe;
            Ok(ChildOutcome::Continue)
        }
        ids::SIMPLE_BLOCK => {
            if !cluster.has_timestamp {
                return Err(Error::Malformed("block before cluster timestamp"));
            }
            let outcome = stream_block_into(
                reader,
                payload_len,
                cluster.ts_ticks,
                scale,
                track_filter,
                out,
                true,
            )?;
            cluster.pos = pe;
            Ok(outcome)
        }
        ids::BLOCK_GROUP => {
            if !cluster.has_timestamp {
                return Err(Error::Malformed("block before cluster timestamp"));
            }
            let mut block_seen = false;
            // Walk inner children for BLOCK + REFERENCE_BLOCK + BLOCK_DURATION.
            // Block's frame bytes stream straight into `out`; the
            // surrounding group's reference/duration children only
            // produce metadata that we fold into the FrameHeader.
            let group_end = pe;
            let mut frame_header: Option<FrameHeader> = None;
            let mut reference_count: u8 = 0;
            let mut block_duration_ticks: Option<u64> = None;
            while cluster.pos < group_end {
                let ch = read_element_header_streaming(reader, &mut cluster.pos)?;
                budget.element()?;
                checked_end(&ch, group_end)?;
                let cpe = ch.payload_end.unwrap_or(group_end);
                let child_len = cpe.saturating_sub(cluster.pos);
                match ch.id {
                    ids::BLOCK => {
                        if block_seen {
                            return Err(Error::Malformed("multiple Blocks in BlockGroup"));
                        }
                        block_seen = true;
                        match stream_block_into(
                            reader,
                            child_len,
                            cluster.ts_ticks,
                            scale,
                            track_filter,
                            out,
                            false,
                        )? {
                            ChildOutcome::Frame(h) => frame_header = Some(h),
                            ChildOutcome::Continue => {} // unselected track
                        }
                    }
                    ids::REFERENCE_BLOCK => {
                        reference_count =
                            reference_count.checked_add(1).ok_or(Error::SizeOverflow)?;
                        if child_len == 0 || child_len > 8 {
                            return Err(Error::Malformed("invalid ReferenceBlock"));
                        }
                        skip_n(reader, child_len)?;
                    }
                    ids::BLOCK_DURATION => {
                        if block_duration_ticks.is_some() || child_len == 0 || child_len > 8 {
                            return Err(Error::Malformed("invalid/duplicate BlockDuration"));
                        }
                        block_duration_ticks = Some(read_be_uint(reader, child_len as usize)?);
                    }
                    _ => {
                        if !matches!(ch.id, ids::VOID | ids::CRC32) {
                            return Err(Error::Unsupported {
                                what: "strict BlockGroup child",
                            });
                        }
                        skip_n(reader, child_len)?;
                    }
                }
                cluster.pos = cpe;
            }
            cluster.pos = group_end;
            if !block_seen {
                return Err(Error::Malformed("BlockGroup missing Block"));
            }
            if let Some(mut h) = frame_header {
                h.is_keyframe = Some(reference_count == 0);
                h.reference_block_count = Some(reference_count);
                // BlockDuration is in segment ticks (TimestampScale-scaled).
                h.block_duration_ns = block_duration_ticks
                    .map(|t| t.checked_mul(scale).ok_or(Error::SizeOverflow))
                    .transpose()?;
                Ok(ChildOutcome::Frame(h))
            } else {
                Ok(ChildOutcome::Continue)
            }
        }
        ids::VOID | ids::CRC32 => {
            skip_n(reader, payload_len)?;
            cluster.pos = pe;
            Ok(ChildOutcome::Continue)
        }
        _ => {
            if !matches!(hdr.id, 0xAB | 0xA7) {
                return Err(Error::Unsupported {
                    what: "strict Cluster child",
                });
            }
            // PrevSize and Position do not affect frame interpretation.
            skip_n(reader, payload_len)?;
            cluster.pos = pe;
            Ok(ChildOutcome::Continue)
        }
    }
}

/// Inner block header is `[track VINT (1–8 B)][ts_delta i16 BE (2 B)]
/// [flags u8 (1 B)]`. We read up to 11 bytes (max VINT + ts + flags)
/// into a stack buffer, parse the header, then either stream the
/// remaining frame bytes into `out` or seek past them. No heap
/// allocation in either path.
fn stream_block_into<R: Read + Seek>(
    reader: &mut BufReader<BudgetReader<R>>,
    payload_len: u64,
    cluster_ts_ticks: u64,
    scale: u64,
    track_filter: Option<&[u64]>,
    out: &mut dyn Write,
    is_simple: bool,
) -> Result<ChildOutcome> {
    // Inner-header peek: VINT (1–8) + ts_delta (2) + flags (1) = ≤ 11 B.
    const MAX_INNER_HDR: usize = 11;
    let payload_len_usize = payload_len as usize;
    let peek_n = payload_len_usize.min(MAX_INNER_HDR);
    let mut hdr_buf = [0u8; MAX_INNER_HDR];
    reader.read_exact(&mut hdr_buf[..peek_n])?;

    // Parse track VINT.
    let mut p = 0;
    let (track, _, _) = read_vint_raw(&hdr_buf[..peek_n], &mut p)?;

    // We need 3 more bytes (ts_delta + flags) after the VINT to have
    // a complete inner header. A malformed block shorter than that is
    // skipped silently — same posture as the lacing-Unsupported case.
    if p + 3 > peek_n {
        return Err(Error::Malformed("short block header"));
    }

    let ts_delta = i16::from_be_bytes([hdr_buf[p], hdr_buf[p + 1]]);
    p += 2;
    let flags = hdr_buf[p];
    p += 1;
    let inner_hdr_size = p;

    // Reject unsupported framing only on selected tracks; unselected payloads are skipped.
    if track_filter.is_some_and(|filter| !filter.contains(&track)) {
        skip_block_payload(reader, payload_len.saturating_sub(peek_n as u64))?;
        return Ok(ChildOutcome::Continue);
    }
    if (flags >> 1) & 0b11 != 0 {
        return Err(Error::Unsupported {
            what: "selected block lacing",
        });
    }
    let ticks = i128::from(cluster_ts_ticks) + i128::from(ts_delta);
    let ns = ticks
        .checked_mul(i128::from(scale))
        .ok_or(Error::SizeOverflow)?;
    let timestamp_ns =
        u64::try_from(ns).map_err(|_| Error::Malformed("negative/overflowing block timestamp"))?;
    if payload_len - inner_hdr_size as u64 > u64::from(u32::MAX) {
        return Err(Error::SizeOverflow);
    }

    // Target track. Bytes we've already pulled past the inner header
    // (still in `hdr_buf`) are part of the frame payload — write them
    // first, then stream the rest from the reader.
    let in_buf_frame_bytes = peek_n - inner_hdr_size;
    if in_buf_frame_bytes > 0 {
        out.write_all(&hdr_buf[inner_hdr_size..peek_n])
            .map_err(Error::Io)?;
    }
    let remaining = payload_len.saturating_sub(peek_n as u64);
    if remaining > 0 {
        let cap = reader.get_ref().max_read;
        reader.get_mut().max_read = usize::MAX;
        let copied = std::io::copy(&mut reader.by_ref().take(remaining), out);
        reader.get_mut().max_read = cap;
        let copied = copied.map_err(Error::Io)?;
        if copied != remaining {
            return Err(Error::UnexpectedEof("frame data short read"));
        }
    }

    let data_size = (payload_len_usize - inner_hdr_size) as u32;
    let invisible = (flags & 0b0000_1000) != 0;
    // SimpleBlock encodes key/discardable in its flags; Block (inside
    // BlockGroup) leaves both as `None` so the BlockGroup walker can
    // populate them from ReferenceBlock children.
    let (is_keyframe, is_discardable) = if is_simple {
        (
            Some((flags & 0b1000_0000) != 0),
            Some((flags & 0b0000_0001) != 0),
        )
    } else {
        (None, None)
    };
    Ok(ChildOutcome::Frame(FrameHeader {
        track,
        timestamp_ns,
        is_keyframe,
        is_invisible: invisible,
        is_discardable,
        block_duration_ns: None,
        reference_block_count: None,
        data_size,
    }))
}

/// Read an element header from the current reader position, advancing
/// `*pos` to the byte after the header. Unlike
/// [`read_element_header_at`], does not seek — the caller asserts the
/// reader is already at `*pos`.
fn read_element_header_streaming<R: Read>(r: &mut R, pos: &mut u64) -> Result<IoHeader> {
    let initial = *pos;
    let mut buf = [0u8; 16];
    // id-width comes from the leading zeros of the first byte.
    r.read_exact(&mut buf[..1])?;
    let first = buf[0];
    if first == 0 {
        return Err(Error::InvalidVint);
    }
    let id_w = first.leading_zeros() as usize + 1;
    if id_w > 4 {
        return Err(Error::InvalidVint);
    }
    if id_w > 1 {
        r.read_exact(&mut buf[1..id_w])?;
    }
    // Then size's first byte.
    r.read_exact(&mut buf[id_w..id_w + 1])?;
    let sfirst = buf[id_w];
    if sfirst == 0 {
        return Err(Error::InvalidVint);
    }
    let size_w = sfirst.leading_zeros() as usize + 1;
    if size_w > 1 {
        r.read_exact(&mut buf[id_w + 1..id_w + size_w])?;
    }
    let mut id_unmasked = 0u64;
    for &byte in &buf[..id_w] {
        id_unmasked = (id_unmasked << 8) | u64::from(byte);
    }
    let mut sp = id_w;
    let (size_val, _, size_unknown) = read_vint_raw(&buf[..id_w + size_w], &mut sp)?;
    let header_bytes = id_w + size_w;
    let payload_start = initial
        .checked_add(header_bytes as u64)
        .ok_or(Error::SizeOverflow)?;
    *pos = payload_start;
    let payload_end = if size_unknown {
        None
    } else {
        Some(
            payload_start
                .checked_add(size_val)
                .ok_or(Error::SizeOverflow)?,
        )
    };
    Ok(IoHeader {
        id: id_unmasked,
        payload_start,
        payload_end,
    })
}

/// Read `n` big-endian bytes into a u64. `n` must be ≤ 8.
fn read_be_uint<R: Read>(r: &mut R, n: usize) -> Result<u64> {
    if n == 0 || n > 8 {
        return Err(Error::Malformed("integer payload out of range"));
    }
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf[..n])?;
    let mut v = 0u64;
    for &b in &buf[..n] {
        v = (v << 8) | b as u64;
    }
    Ok(v)
}

/// Discard `n` bytes from `r`. Cheaper than `read_exact` into a fresh
/// Vec when the caller doesn't need the bytes (REFERENCE_BLOCK, Void,
/// unknown children).
fn skip_n<R: Read>(r: &mut R, n: u64) -> Result<()> {
    if n == 0 {
        return Ok(());
    }
    let copied = std::io::copy(&mut r.take(n), &mut std::io::sink()).map_err(Error::Io)?;
    if copied != n {
        return Err(Error::UnexpectedEof("skip_n short read"));
    }
    Ok(())
}

/// Skip within already-paid buffered bytes when possible, otherwise seek.
/// Even a small unwanted audio packet must not trigger payload refills merely
/// because it is below the buffer's capacity. Parent extents were checked first.
fn skip_block_payload<R: Read + Seek>(reader: &mut BufReader<R>, n: u64) -> Result<()> {
    if n != 0 {
        reader.seek_relative(i64::try_from(n).map_err(|_| Error::SizeOverflow)?)?;
    }
    Ok(())
}

/// Minimal element-header parser that works directly off a Read+Seek
/// source. Reads only the bytes needed for the id+size VINTs (up to
/// 16 bytes). `pos` is the absolute byte offset where this header
/// starts in the file.
struct IoHeader {
    id: u64,
    payload_start: u64,
    payload_end: Option<u64>,
}

fn read_element_header_at<R: Read + Seek>(r: &mut R, mut pos: u64) -> Result<IoHeader> {
    r.seek(SeekFrom::Start(pos))?;
    read_element_header_streaming(r, &mut pos)
}

fn read_at<R: Read + Seek>(r: &mut R, pos: u64, len: u64) -> Result<Vec<u8>> {
    r.seek(SeekFrom::Start(pos))?;
    read_exact_n(r, usize::try_from(len).map_err(|_| Error::SizeOverflow)?)
}

fn read_exact_n<R: Read>(r: &mut R, len: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}
