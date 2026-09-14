//! Matroska / WebM muxer — inverse of [`crate::demux`] +
//! [`crate::cluster`].
//!
//! Two modes:
//! - **Static** (default): accumulate all clusters in memory; one
//!   [`Muxer::finalize`] call emits a complete, seekable Segment with
//!   SeekHead + Cues. The known-size encoding.
//! - **Streaming**: opt-in via [`Muxer::enable_streaming`].
//!   [`Muxer::begin_stream`] emits an init segment (EBML header +
//!   Segment-open with the unknown-size VINT + Info + Tracks), each
//!   [`Muxer::take_output`] drains completed Cluster bytes for a pipe
//!   to network / `SourceBuffer.appendBuffer`, and
//!   [`Muxer::finish_stream`] flushes the in-progress cluster. No
//!   SeekHead / Cues in streaming mode — the Segment closes implicitly
//!   at EOF and consumers seek by linear walk.
//!
//! Cluster boundary at every `cluster_duration_ns` of timeline
//! (default 1 s) or whenever a video keyframe arrives. SimpleBlock
//! only — no lacing, no BlockGroup. Frames inside one Cluster share
//! its base timestamp; the block delta is `i16` ticks at
//! `TIMESTAMP_SCALE = 1_000_000 ns`. DocType-gated codec validation
//! via [`crate::profile`] so a WebM muxer can't be tricked into
//! emitting an H.264 track.

use crate::demux::{AudioParams, TrackKind, VideoParams};
use crate::ebml::schema::{ids, track_type};
use crate::ebml::writer::*;
use crate::profile::{DocType, is_codec_allowed};
use crate::{Error, Result};

/// Constant timestamp scale — every block timestamp is scaled by this
/// many nanoseconds. 1 ms ticks matches the Matroska default and
/// keeps i16 block deltas comfortably wide (~32 s range per cluster).
const TIMESTAMP_SCALE_NS: u64 = 1_000_000;

/// Default cluster duration. Cluster size is bounded so block deltas
/// stay inside i16 ticks at 1 ms — i.e. ≤ 32_767 ms. We use 1 s, well
/// inside the limit and matching what ffmpeg emits by default.
const DEFAULT_CLUSTER_DURATION_NS: u64 = 1_000_000_000;

/// What the caller hands to [`Muxer::register_track`]. Everything
/// here flows straight into a `TrackEntry`; `number` is assigned by
/// the muxer.
#[derive(Debug, Clone)]
pub struct TrackDescriptor {
    pub kind: TrackKind,
    pub codec_id: String,
    pub codec_private: Option<Vec<u8>>,
    pub default_duration_ns: Option<u64>,
    pub codec_delay_ns: u64,
    pub seek_pre_roll_ns: u64,
    pub language: Option<String>,
    pub name: Option<String>,
    pub flag_default: bool,
    pub flag_forced: bool,
    pub video: Option<VideoParams>,
    pub audio: Option<AudioParams>,
}

impl Default for TrackDescriptor {
    fn default() -> Self {
        Self {
            kind: TrackKind::Other(0),
            codec_id: String::new(),
            codec_private: None,
            default_duration_ns: None,
            codec_delay_ns: 0,
            seek_pre_roll_ns: 0,
            language: None,
            name: None,
            flag_default: true,
            flag_forced: false,
            video: None,
            audio: None,
        }
    }
}

impl TrackDescriptor {
    pub fn audio(codec_id: &str, sampling_frequency: f64, channels: u32) -> Self {
        Self {
            kind: TrackKind::Audio,
            codec_id: codec_id.to_string(),
            audio: Some(AudioParams {
                sampling_frequency,
                output_sampling_frequency: None,
                channels,
                bit_depth: None,
            }),
            ..Self::default()
        }
    }

    pub fn video(codec_id: &str, pixel_width: u32, pixel_height: u32) -> Self {
        Self {
            kind: TrackKind::Video,
            codec_id: codec_id.to_string(),
            video: Some(VideoParams {
                pixel_width,
                pixel_height,
                display_width: None,
                display_height: None,
            }),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone)]
struct TrackState {
    desc: TrackDescriptor,
    /// 1-based track number assigned at registration time.
    number: u64,
}

/// One Cues index entry. Position is the byte offset of the cluster's
/// first byte relative to the *Segment payload* start — the value
/// CueClusterPosition encodes per the Matroska spec.
#[derive(Debug, Clone)]
struct CueEntry {
    ts_ticks: u64,
    track_number: u64,
    /// Offset within the accumulated [`Muxer::clusters`] buffer.
    /// Translated to a Segment-payload-relative position at finalize.
    cluster_offset_in_clusters_buf: usize,
}

pub struct Muxer {
    doc_type: DocType,
    tracks: Vec<TrackState>,
    /// Buffer of finished Cluster bytes ready to drop into the
    /// Segment payload.
    clusters: Vec<u8>,
    /// In-progress Cluster: timestamp + block bodies.
    cluster_start_ts_ticks: Option<u64>,
    cluster_blocks: Vec<u8>,
    /// Keyframes appended into the in-progress cluster, materialised
    /// into [`Muxer::cue_points`] when the cluster flushes.
    current_cluster_keyframes: Vec<(u64, u64)>,
    cue_points: Vec<CueEntry>,
    cluster_duration_ns: u64,
    muxing_app: String,
    writing_app: String,
    /// Streaming mode opt-in. When set, [`Muxer::begin_stream`] /
    /// [`Muxer::take_output`] / [`Muxer::finish_stream`] are the
    /// emit surface; [`Muxer::finalize`] is rejected.
    streaming: bool,
    /// True once [`Muxer::begin_stream`] has emitted the init segment.
    stream_initialized: bool,
    /// In streaming mode, how many bytes of [`Muxer::clusters`] have
    /// already been handed back to the caller via `take_output`.
    clusters_drained: usize,
    /// Latest presentation END time seen by [`Muxer::append`], in ns —
    /// the block's timestamp plus its track's `default_duration_ns`
    /// where one is declared. Becomes `Info/Duration` at finalize.
    ///
    /// The end, not the last timestamp: a player that trusts Duration
    /// literally would otherwise drop the final frame, and for a
    /// one-frame file the two answers are "one frame long" and "zero".
    /// A track with no declared frame duration contributes only its
    /// timestamp, which is the honest answer when nothing in the file
    /// says how long the last frame is shown for.
    max_end_ns: u64,
}

impl Muxer {
    pub fn new(doc_type: DocType) -> Self {
        Self {
            doc_type,
            tracks: Vec::new(),
            clusters: Vec::new(),
            cluster_start_ts_ticks: None,
            cluster_blocks: Vec::new(),
            current_cluster_keyframes: Vec::new(),
            cue_points: Vec::new(),
            cluster_duration_ns: DEFAULT_CLUSTER_DURATION_NS,
            muxing_app: "media-mkv-webm".to_string(),
            writing_app: "media-mkv-webm".to_string(),
            streaming: false,
            stream_initialized: false,
            clusters_drained: 0,
            max_end_ns: 0,
        }
    }

    /// Switch into streaming mode. Must be called before
    /// [`Muxer::begin_stream`]; mixing modes (calling `finalize` after
    /// `begin_stream`, or `begin_stream` without `enable_streaming`)
    /// returns [`Error::Unsupported`].
    pub fn enable_streaming(&mut self) {
        self.streaming = true;
    }

    /// Returns `true` if [`Muxer::enable_streaming`] was called.
    pub fn is_streaming(&self) -> bool {
        self.streaming
    }

    /// Override the target Cluster duration (in nanoseconds). The
    /// emitted cluster may still be cut earlier on a video keyframe.
    /// Pinning a value > i16 ticks @ 1 ms (≈ 32 s) is rejected.
    pub fn set_cluster_duration_ns(&mut self, ns: u64) -> Result<()> {
        let max_delta_ns = (i16::MAX as u64) * TIMESTAMP_SCALE_NS;
        if ns == 0 || ns > max_delta_ns {
            return Err(Error::Unsupported {
                what: "cluster duration out of i16 block-delta range",
            });
        }
        self.cluster_duration_ns = ns;
        Ok(())
    }

    pub fn set_writing_app(&mut self, name: impl Into<String>) {
        self.writing_app = name.into();
    }

    /// Register a track. Returns the assigned 1-based track number
    /// that the caller passes back to [`Muxer::append`].
    pub fn register_track(&mut self, desc: TrackDescriptor) -> Result<u64> {
        // The Tracks element is emitted by begin_stream — late
        // registrations would silently fall off the track table.
        if self.stream_initialized {
            return Err(Error::Unsupported {
                what: "register_track after begin_stream",
            });
        }
        // Profile gate: webm refuses everything outside its whitelist.
        // For non-recognised codec IDs we still allow MKV (the policy
        // matches our demux side — unknown codec passes through, but
        // doctype mismatch is hard-rejected).
        if self.doc_type == DocType::Webm && !is_codec_allowed(self.doc_type, &desc.codec_id) {
            return Err(Error::Unsupported {
                what: "codec not allowed in WebM profile",
            });
        }
        // Sanity-check codec-id prefix vs track kind.
        let prefix_ok = match desc.kind {
            TrackKind::Video => desc.codec_id.starts_with("V_"),
            TrackKind::Audio => desc.codec_id.starts_with("A_"),
            TrackKind::Subtitle => desc.codec_id.starts_with("S_"),
            _ => true,
        };
        if !prefix_ok {
            return Err(Error::Malformed("CodecID prefix doesn't match TrackType"));
        }

        let number = self.tracks.len() as u64 + 1;
        self.tracks.push(TrackState { desc, number });
        Ok(number)
    }

    /// Append one already-encoded codec packet on `track_number` with
    /// presentation timestamp `ts_ns`. `keyframe=true` for video I-frames
    /// (or audio always, since audio frames are independently decodable).
    pub fn append(
        &mut self,
        track_number: u64,
        packet: &[u8],
        ts_ns: u64,
        keyframe: bool,
    ) -> Result<()> {
        if self.streaming && !self.stream_initialized {
            return Err(Error::Unsupported {
                what: "append before begin_stream in streaming mode",
            });
        }
        let track = self
            .tracks
            .iter()
            .find(|t| t.number == track_number)
            .ok_or(Error::Malformed("unknown track number in append()"))?;
        let is_video = matches!(track.desc.kind, TrackKind::Video);
        let frame_duration_ns = track.desc.default_duration_ns.unwrap_or(0);

        let ts_ticks = ts_ns / TIMESTAMP_SCALE_NS;

        // Decide whether to flush the in-progress cluster first.
        let must_flush = match self.cluster_start_ts_ticks {
            None => false,
            Some(start) => {
                let delta_ns = ts_ns.saturating_sub(start * TIMESTAMP_SCALE_NS);
                // Cut at duration boundary OR on a video keyframe (keyframes
                // start a new cluster so seekable points line up cleanly).
                delta_ns >= self.cluster_duration_ns || (is_video && keyframe && delta_ns > 0)
            }
        };
        if must_flush {
            self.flush_cluster();
        }
        if self.cluster_start_ts_ticks.is_none() {
            self.cluster_start_ts_ticks = Some(ts_ticks);
        }

        let cluster_start = self.cluster_start_ts_ticks.unwrap();
        let delta_ticks = (ts_ticks as i64) - (cluster_start as i64);
        if !(i16::MIN as i64..=i16::MAX as i64).contains(&delta_ticks) {
            return Err(Error::Malformed(
                "block delta exceeds i16 range; cluster too long",
            ));
        }

        write_simple_block(
            track_number,
            delta_ticks as i16,
            keyframe,
            packet,
            &mut self.cluster_blocks,
        );
        if keyframe {
            self.current_cluster_keyframes
                .push((track_number, ts_ticks));
        }
        self.max_end_ns = self.max_end_ns.max(ts_ns.saturating_add(frame_duration_ns));
        Ok(())
    }

    /// Flush the in-progress cluster (if any) and return the complete
    /// `.mkv` / `.webm` byte buffer.
    pub fn finalize(mut self) -> Result<Vec<u8>> {
        if self.streaming {
            return Err(Error::Unsupported {
                what: "finalize() not valid in streaming mode — use finish_stream",
            });
        }
        self.flush_cluster();

        let mut out = Vec::new();
        write_ebml_header(self.doc_type, &mut out);

        // === Pre-build Info + Tracks bodies =========================
        let mut info_body = Vec::new();
        write_uint(ids::TIMESTAMP_SCALE, TIMESTAMP_SCALE_NS, &mut info_body);
        // Duration, in TimestampScale units, as a float — the element a
        // player builds its timeline from. Matroska makes it optional and
        // this muxer used to omit it, which is survivable for a live
        // stream and wrong for a file: without it a reader discovers the
        // end by running out of clusters, and stops partway through
        // something completely intact. `demux.rs` has always READ it.
        //
        // Written only when something was actually appended, since a
        // declared zero is a different (and false) claim from silence.
        if self.max_end_ns > 0 {
            write_float64(
                ids::DURATION,
                self.max_end_ns as f64 / TIMESTAMP_SCALE_NS as f64,
                &mut info_body,
            );
        }
        write_utf8(ids::MUXING_APP, &self.muxing_app, &mut info_body);
        write_utf8(ids::WRITING_APP, &self.writing_app, &mut info_body);
        let mut info_elem = Vec::new();
        write_master(ids::INFO, &info_body, &mut info_elem);

        let mut tracks_body = Vec::new();
        for t in &self.tracks {
            let entry = build_track_entry(t);
            write_master(ids::TRACK_ENTRY, &entry, &mut tracks_body);
        }
        let mut tracks_elem = Vec::new();
        write_master(ids::TRACKS, &tracks_body, &mut tracks_elem);

        // === Plan SeekHead size (fixed-width SeekPosition) ===========
        // SeekPosition is pinned to 8 bytes so SeekHead's total size
        // is computable before we know any offset.
        let has_cues = !self.cue_points.is_empty();
        let mut seek_targets: Vec<u64> = vec![ids::INFO, ids::TRACKS];
        if has_cues {
            seek_targets.push(ids::CUES);
        }
        let seek_head_body_size: usize = seek_targets.iter().map(|&id| seek_entry_size(id)).sum();
        let seek_head_size = element_size(ids::SEEK_HEAD, seek_head_body_size);

        // Segment-payload-relative offsets of each top-level child.
        let info_offset = seek_head_size;
        let tracks_offset = info_offset + info_elem.len();
        let clusters_offset = tracks_offset + tracks_elem.len();
        let cues_offset = clusters_offset + self.clusters.len();

        // === Build Cues element =====================================
        let cues_elem = if has_cues {
            let mut body = Vec::new();
            for cp in &self.cue_points {
                let abs_cluster_pos = (clusters_offset + cp.cluster_offset_in_clusters_buf) as u64;
                let mut ctp_body = Vec::new();
                write_uint(ids::CUE_TRACK, cp.track_number, &mut ctp_body);
                write_uint(ids::CUE_CLUSTER_POSITION, abs_cluster_pos, &mut ctp_body);
                let mut cue_point_body = Vec::new();
                write_uint(ids::CUE_TIME, cp.ts_ticks, &mut cue_point_body);
                write_master(ids::CUE_TRACK_POSITIONS, &ctp_body, &mut cue_point_body);
                write_master(ids::CUE_POINT, &cue_point_body, &mut body);
            }
            let mut e = Vec::new();
            write_master(ids::CUES, &body, &mut e);
            e
        } else {
            Vec::new()
        };

        // === Build SeekHead =========================================
        let mut seek_head_body = Vec::new();
        for &target in &seek_targets {
            let position = match target {
                id if id == ids::INFO => info_offset as u64,
                id if id == ids::TRACKS => tracks_offset as u64,
                id if id == ids::CUES => cues_offset as u64,
                _ => unreachable!("unexpected seek target"),
            };
            push_seek_entry(target, position, &mut seek_head_body);
        }
        debug_assert_eq!(seek_head_body.len(), seek_head_body_size);
        let mut seek_head_elem = Vec::new();
        write_master(ids::SEEK_HEAD, &seek_head_body, &mut seek_head_elem);
        debug_assert_eq!(seek_head_elem.len(), seek_head_size);

        // === Assemble Segment payload ===============================
        let mut seg = Vec::new();
        seg.extend_from_slice(&seek_head_elem);
        seg.extend_from_slice(&info_elem);
        seg.extend_from_slice(&tracks_elem);
        seg.extend_from_slice(&self.clusters);
        seg.extend_from_slice(&cues_elem);

        write_master(ids::SEGMENT, &seg, &mut out);
        Ok(out)
    }

    /// Emit the init segment: EBML header + Segment-open (unknown
    /// size) + Info + Tracks. Must be called once, after every
    /// [`Muxer::register_track`], before any [`Muxer::append`].
    ///
    /// The returned bytes are a valid Matroska/WebM prefix that
    /// demuxers can parse on the fly: the Segment is left
    /// unterminated, every subsequent Cluster appends as a sibling.
    pub fn begin_stream(&mut self) -> Result<Vec<u8>> {
        if !self.streaming {
            return Err(Error::Unsupported {
                what: "begin_stream requires enable_streaming",
            });
        }
        if self.stream_initialized {
            return Err(Error::Unsupported {
                what: "begin_stream already called",
            });
        }

        let mut out = Vec::new();
        write_ebml_header(self.doc_type, &mut out);

        // Segment id + unknown-size VINT. The Segment never closes —
        // its end is signalled by EOF, per the Matroska spec.
        open_master_unknown_size(ids::SEGMENT, &mut out);

        // Info + Tracks land right behind the Segment header so a
        // streaming consumer can parse the track table from the first
        // chunk it receives.
        //
        // No Duration here, unlike `finalize`: these bytes go out before
        // the first block is appended, and the Segment is opened with an
        // unknown size precisely because the length is not knowable yet.
        // A streaming consumer signals the end with EOF instead.
        let mut info_body = Vec::new();
        write_uint(ids::TIMESTAMP_SCALE, TIMESTAMP_SCALE_NS, &mut info_body);
        write_utf8(ids::MUXING_APP, &self.muxing_app, &mut info_body);
        write_utf8(ids::WRITING_APP, &self.writing_app, &mut info_body);
        write_master(ids::INFO, &info_body, &mut out);

        let mut tracks_body = Vec::new();
        for t in &self.tracks {
            let entry = build_track_entry(t);
            write_master(ids::TRACK_ENTRY, &entry, &mut tracks_body);
        }
        write_master(ids::TRACKS, &tracks_body, &mut out);

        self.stream_initialized = true;
        Ok(out)
    }

    /// Drain any cluster bytes finished since the last call. Returns
    /// an empty `Vec` when no new clusters have closed yet. Safe to
    /// poll after every [`Muxer::append`].
    pub fn take_output(&mut self) -> Vec<u8> {
        if !self.streaming || !self.stream_initialized {
            return Vec::new();
        }
        let out = self.clusters[self.clusters_drained..].to_vec();
        self.clusters_drained = self.clusters.len();
        out
    }

    /// Flush the in-progress cluster and return all remaining cluster
    /// bytes. No closing Segment tag is emitted — the unknown-size
    /// encoding terminates at EOF.
    pub fn finish_stream(mut self) -> Result<Vec<u8>> {
        if !self.streaming {
            return Err(Error::Unsupported {
                what: "finish_stream requires enable_streaming",
            });
        }
        if !self.stream_initialized {
            return Err(Error::Unsupported {
                what: "finish_stream before begin_stream",
            });
        }
        self.flush_cluster();
        Ok(self.take_output())
    }

    fn flush_cluster(&mut self) {
        if self.cluster_blocks.is_empty() {
            self.cluster_start_ts_ticks = None;
            self.current_cluster_keyframes.clear();
            return;
        }
        let cluster_offset = self.clusters.len();
        let mut cluster = Vec::new();
        write_uint(
            ids::TIMESTAMP,
            self.cluster_start_ts_ticks.unwrap_or(0),
            &mut cluster,
        );
        cluster.extend_from_slice(&self.cluster_blocks);
        write_master(ids::CLUSTER, &cluster, &mut self.clusters);

        for (track_number, ts_ticks) in self.current_cluster_keyframes.drain(..) {
            self.cue_points.push(CueEntry {
                ts_ticks,
                track_number,
                cluster_offset_in_clusters_buf: cluster_offset,
            });
        }
        self.cluster_blocks.clear();
        self.cluster_start_ts_ticks = None;
    }
}

fn write_ebml_header(doc_type: DocType, out: &mut Vec<u8>) {
    let mut hdr = Vec::new();
    write_uint(ids::EBML_VERSION, 1, &mut hdr);
    write_uint(ids::EBML_READ_VERSION, 1, &mut hdr);
    write_uint(ids::EBML_MAX_ID_LENGTH, 4, &mut hdr);
    write_uint(ids::EBML_MAX_SIZE_LENGTH, 8, &mut hdr);
    write_ascii(ids::DOC_TYPE, doc_type.as_str(), &mut hdr);
    write_uint(ids::DOC_TYPE_VERSION, 4, &mut hdr);
    write_uint(ids::DOC_TYPE_READ_VERSION, 2, &mut hdr);
    write_master(ids::EBML, &hdr, out);
}

fn build_track_entry(t: &TrackState) -> Vec<u8> {
    let mut e = Vec::new();
    write_uint(ids::TRACK_NUMBER, t.number, &mut e);
    write_uint(
        ids::TRACK_UID,
        t.number.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
        &mut e,
    );

    let track_type_val = match t.desc.kind {
        TrackKind::Video => track_type::VIDEO,
        TrackKind::Audio => track_type::AUDIO,
        TrackKind::Subtitle => track_type::SUBTITLE,
        TrackKind::Other(v) => v as u64,
    };
    write_uint(ids::TRACK_TYPE, track_type_val, &mut e);
    write_uint(ids::FLAG_ENABLED, 1, &mut e);
    write_uint(ids::FLAG_DEFAULT, t.desc.flag_default as u64, &mut e);
    write_uint(ids::FLAG_FORCED, t.desc.flag_forced as u64, &mut e);
    write_uint(ids::FLAG_LACING, 0, &mut e); // we don't emit lacing

    write_ascii(ids::CODEC_ID, &t.desc.codec_id, &mut e);
    if let Some(cp) = &t.desc.codec_private {
        write_binary(ids::CODEC_PRIVATE, cp, &mut e);
    }
    if let Some(d) = t.desc.default_duration_ns {
        write_uint(ids::DEFAULT_DURATION, d, &mut e);
    }
    if t.desc.codec_delay_ns != 0 {
        write_uint(ids::CODEC_DELAY, t.desc.codec_delay_ns, &mut e);
    }
    if t.desc.seek_pre_roll_ns != 0 {
        write_uint(ids::SEEK_PRE_ROLL, t.desc.seek_pre_roll_ns, &mut e);
    }
    if let Some(lang) = &t.desc.language {
        write_ascii(ids::LANGUAGE, lang, &mut e);
    }
    if let Some(name) = &t.desc.name {
        write_utf8(ids::NAME, name, &mut e);
    }

    if let Some(v) = &t.desc.video {
        let mut vbody = Vec::new();
        write_uint(ids::PIXEL_WIDTH, v.pixel_width as u64, &mut vbody);
        write_uint(ids::PIXEL_HEIGHT, v.pixel_height as u64, &mut vbody);
        if let Some(dw) = v.display_width {
            write_uint(ids::DISPLAY_WIDTH, dw as u64, &mut vbody);
        }
        if let Some(dh) = v.display_height {
            write_uint(ids::DISPLAY_HEIGHT, dh as u64, &mut vbody);
        }
        write_master(ids::VIDEO, &vbody, &mut e);
    }
    if let Some(a) = &t.desc.audio {
        let mut abody = Vec::new();
        write_float64(ids::SAMPLING_FREQUENCY, a.sampling_frequency, &mut abody);
        if let Some(osf) = a.output_sampling_frequency {
            write_float64(ids::OUTPUT_SAMPLING_FREQUENCY, osf, &mut abody);
        }
        write_uint(ids::CHANNELS, a.channels as u64, &mut abody);
        if let Some(bd) = a.bit_depth {
            write_uint(ids::BIT_DEPTH, bd as u64, &mut abody);
        }
        write_master(ids::AUDIO, &abody, &mut e);
    }

    e
}

/// Total bytes of one `Seek` element (master + SeekID binary + 8-byte
/// fixed-width SeekPosition uint) pointing at `target_id`.
fn seek_entry_size(target_id: u64) -> usize {
    let id_width = id_width_of(target_id);
    let seek_id_inner = element_size(ids::SEEK_ID, id_width);
    let seek_pos_inner = element_size(ids::SEEK_POSITION, 8);
    element_size(ids::SEEK, seek_id_inner + seek_pos_inner)
}

/// Append one `Seek` entry into a SeekHead body.
fn push_seek_entry(target_id: u64, position: u64, out: &mut Vec<u8>) {
    let id_width = id_width_of(target_id);
    let be = target_id.to_be_bytes();
    let raw_id = &be[8 - id_width..];

    let mut body = Vec::new();
    write_binary(ids::SEEK_ID, raw_id, &mut body);
    write_uint_fixed_width(ids::SEEK_POSITION, position, 8, &mut body);
    write_master(ids::SEEK, &body, out);
}

fn write_simple_block(
    track_number: u64,
    delta_ticks: i16,
    keyframe: bool,
    data: &[u8],
    out: &mut Vec<u8>,
) {
    let mut body = Vec::new();
    write_size_vint(track_number, &mut body);
    body.extend_from_slice(&delta_ticks.to_be_bytes());
    let flags: u8 = if keyframe { 0b1000_0000 } else { 0 };
    body.push(flags);
    body.extend_from_slice(data);
    write_master(ids::SIMPLE_BLOCK, &body, out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::Frames;
    use crate::demux::Demuxer;
    use crate::ebml::schema::codec_id;

    #[test]
    fn webm_refuses_h264_track() {
        let mut m = Muxer::new(DocType::Webm);
        let err = m
            .register_track(TrackDescriptor::video(
                codec_id::V_MPEG4_ISO_AVC,
                1920,
                1080,
            ))
            .unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }));
    }

    /// `finalize` must declare `Info/Duration`, and it must cover the LAST
    /// frame rather than stopping at its timestamp.
    ///
    /// Omitting it was survivable for a live stream and wrong for a file: a
    /// player has nothing to build a timeline from, so it discovers the end
    /// by running out of clusters and stops early on a file whose every
    /// sample is intact. `demux.rs` has always read this element; `mux.rs`
    /// never wrote it.
    #[test]
    fn finalize_declares_a_duration_that_covers_the_last_frame() {
        let mut m = Muxer::new(DocType::Matroska);
        let mut desc = TrackDescriptor::video(codec_id::V_MPEG4_ISO_AVC, 320, 240);
        desc.default_duration_ns = Some(1_000_000_000 / 30);
        let t = m.register_track(desc).unwrap();
        for i in 0..90u64 {
            m.append(t, b"sample", i * 1_000_000_000 / 30, i % 30 == 0)
                .unwrap();
        }
        let bytes = m.finalize().unwrap();

        let d = Demuxer::parse(&bytes).unwrap();
        let ticks = d
            .duration_ticks
            .expect("finalize wrote no Info/Duration — a player stops early on this");
        let ms = ticks * d.timestamp_scale_ns as f64 / 1_000_000.0;
        // 90 frames at 30 fps is three seconds. The frame's own duration is
        // included, which is what makes this 3000 and not 2967: a reader that
        // trusts Duration literally would otherwise drop the final frame.
        assert!(
            (ms - 3000.0).abs() < 1.0,
            "declared {ms} ms for 90 frames at 30 fps"
        );
    }

    /// A track that declares no frame duration still gets a Duration, built
    /// from the last timestamp alone. That is the honest answer — nothing in
    /// the file says how long the final frame is shown for — and it beats
    /// declaring nothing.
    #[test]
    fn duration_falls_back_to_the_last_timestamp_without_a_frame_duration() {
        let mut m = Muxer::new(DocType::Matroska);
        let t = m
            .register_track(TrackDescriptor::audio(codec_id::A_OPUS, 48000.0, 2))
            .unwrap();
        m.append(t, b"a", 0, true).unwrap();
        m.append(t, b"b", 500_000_000, true).unwrap();
        let d = Demuxer::parse(&m.finalize().unwrap()).unwrap();
        let ms = d.duration_ticks.expect("no Duration") * d.timestamp_scale_ns as f64 / 1e6;
        assert!((ms - 500.0).abs() < 1.0, "declared {ms} ms");
    }

    /// An empty file declares nothing rather than declaring zero — those are
    /// different claims and only one of them is true.
    #[test]
    fn an_empty_file_declares_no_duration_rather_than_zero() {
        let mut m = Muxer::new(DocType::Matroska);
        m.register_track(TrackDescriptor::audio(codec_id::A_OPUS, 48000.0, 2))
            .unwrap();
        let d = Demuxer::parse(&m.finalize().unwrap()).unwrap();
        assert_eq!(d.duration_ticks, None);
    }

    #[test]
    fn round_trip_opus_stereo_two_frames() {
        let mut m = Muxer::new(DocType::Matroska);
        let t = m
            .register_track(TrackDescriptor::audio(codec_id::A_OPUS, 48000.0, 2))
            .unwrap();
        m.append(t, b"frame-A", 0, true).unwrap();
        m.append(t, b"frame-B-longer", 20_000_000, true).unwrap();
        let bytes = m.finalize().unwrap();

        let d = Demuxer::parse(&bytes).unwrap();
        assert_eq!(d.doc_type, DocType::Matroska);
        assert_eq!(d.timestamp_scale_ns, TIMESTAMP_SCALE_NS);
        assert_eq!(d.tracks.len(), 1);
        let track = &d.tracks[0];
        assert_eq!(track.kind, TrackKind::Audio);
        assert_eq!(track.codec_id, codec_id::A_OPUS);
        let audio = track.audio.as_ref().unwrap();
        assert_eq!(audio.sampling_frequency as u32, 48000);
        assert_eq!(audio.channels, 2);

        let mut frames = Frames::new(&bytes, &d);
        let f1 = frames.next_frame().unwrap().unwrap();
        assert_eq!(f1.data, b"frame-A");
        assert_eq!(f1.timestamp_ns, 0);
        let f2 = frames.next_frame().unwrap().unwrap();
        assert_eq!(f2.data, b"frame-B-longer");
        assert_eq!(f2.timestamp_ns, 20_000_000);
        assert!(frames.next_frame().unwrap().is_none());
    }

    #[test]
    fn cues_emitted_for_video_keyframes() {
        // Two keyframes → two clusters → two Cues entries. Each
        // CueClusterPosition must point at the SimpleBlock-bearing
        // CLUSTER element when added to segment_payload_start.
        let mut m = Muxer::new(DocType::Webm);
        let v = m
            .register_track(TrackDescriptor::video(codec_id::V_VP9, 320, 240))
            .unwrap();
        m.append(v, b"I0", 0, true).unwrap();
        m.append(v, b"P0", 33_000_000, false).unwrap();
        m.append(v, b"I1", 66_000_000, true).unwrap();
        m.append(v, b"P1", 100_000_000, false).unwrap();
        let bytes = m.finalize().unwrap();

        let d = Demuxer::parse(&bytes).unwrap();
        assert_eq!(d.cues.len(), 2, "one cue per video keyframe");
        assert_eq!(d.cues[0].ts_ns, 0);
        assert_eq!(d.cues[0].track_number, v);
        assert_eq!(d.cues[1].ts_ns, 66_000_000);

        // Each cue position resolves to a CLUSTER element id in the
        // input buffer.
        for cp in &d.cues {
            let abs = d.segment_payload_start + cp.cluster_position as usize;
            // CLUSTER id is 4 bytes: 0x1F 0x43 0xB6 0x75
            assert_eq!(&bytes[abs..abs + 4], &[0x1F, 0x43, 0xB6, 0x75]);
        }
    }

    #[test]
    fn no_cues_when_no_keyframes() {
        // A muxer that only sees non-keyframes (uncommon in practice
        // but legal) emits no Cues and a 2-entry SeekHead.
        let mut m = Muxer::new(DocType::Matroska);
        let a = m
            .register_track(TrackDescriptor::audio(codec_id::A_OPUS, 48000.0, 2))
            .unwrap();
        // Mark Opus packets as non-keyframes so the cue table stays empty.
        m.append(a, b"opus-A", 0, false).unwrap();
        m.append(a, b"opus-B", 20_000_000, false).unwrap();
        let bytes = m.finalize().unwrap();
        let d = Demuxer::parse(&bytes).unwrap();
        assert!(d.cues.is_empty());
    }

    #[test]
    fn streaming_round_trip() {
        // Build a streaming mux: init segment + per-cluster chunks +
        // final flush. The concatenated bytes must parse cleanly
        // through our Demuxer (unknown-size Segment is supported).
        let mut m = Muxer::new(DocType::Webm);
        m.enable_streaming();
        let v = m
            .register_track(TrackDescriptor::video(codec_id::V_VP9, 320, 240))
            .unwrap();

        let mut wire = m.begin_stream().unwrap();
        // Append should fan out into one cluster per keyframe.
        m.append(v, b"key-0", 0, true).unwrap();
        m.append(v, b"p-0", 33_000_000, false).unwrap();
        wire.extend_from_slice(&m.take_output());
        m.append(v, b"key-1", 66_000_000, true).unwrap();
        wire.extend_from_slice(&m.take_output());
        m.append(v, b"p-1", 100_000_000, false).unwrap();
        wire.extend_from_slice(&m.finish_stream().unwrap());

        let d = Demuxer::parse(&wire).unwrap();
        assert_eq!(d.doc_type, DocType::Webm);
        // No SeekHead / Cues in streaming mode.
        assert!(d.cues.is_empty());
        assert_eq!(d.tracks.len(), 1);
        assert_eq!(d.tracks[0].codec_id, codec_id::V_VP9);

        let mut frames = Frames::new(&wire, &d);
        let got: Vec<_> = std::iter::from_fn(|| frames.next_frame().unwrap())
            .map(|f| (f.timestamp_ns, f.data.to_vec(), f.is_keyframe))
            .collect();
        assert_eq!(
            got,
            vec![
                (0, b"key-0".to_vec(), Some(true)),
                (33_000_000, b"p-0".to_vec(), Some(false)),
                (66_000_000, b"key-1".to_vec(), Some(true)),
                (100_000_000, b"p-1".to_vec(), Some(false)),
            ]
        );
    }

    #[test]
    fn streaming_state_machine_rejects_misuse() {
        // begin_stream without enable_streaming.
        let mut m = Muxer::new(DocType::Webm);
        m.register_track(TrackDescriptor::video(codec_id::V_VP9, 1, 1))
            .unwrap();
        assert!(matches!(m.begin_stream(), Err(Error::Unsupported { .. })));

        // append before begin_stream in streaming mode.
        let mut m = Muxer::new(DocType::Webm);
        m.enable_streaming();
        let t = m
            .register_track(TrackDescriptor::video(codec_id::V_VP9, 1, 1))
            .unwrap();
        assert!(matches!(
            m.append(t, b"x", 0, true),
            Err(Error::Unsupported { .. })
        ));

        // finalize() in streaming mode.
        let mut m = Muxer::new(DocType::Webm);
        m.enable_streaming();
        assert!(matches!(m.finalize(), Err(Error::Unsupported { .. })));

        // register_track after begin_stream, and double begin_stream.
        let mut m = Muxer::new(DocType::Webm);
        m.enable_streaming();
        m.register_track(TrackDescriptor::video(codec_id::V_VP9, 1, 1))
            .unwrap();
        let _ = m.begin_stream().unwrap();
        assert!(matches!(
            m.register_track(TrackDescriptor::video(codec_id::V_VP9, 1, 1)),
            Err(Error::Unsupported { .. })
        ));
        assert!(matches!(m.begin_stream(), Err(Error::Unsupported { .. })));
    }

    #[test]
    fn streaming_init_segment_has_unknown_size_marker() {
        // Bytes 0..N is the EBML master; immediately after is the
        // Segment id (1F 43 B6 75) followed by 0xFF (the 1-byte
        // unknown-size VINT).
        let mut m = Muxer::new(DocType::Matroska);
        m.enable_streaming();
        m.register_track(TrackDescriptor::audio(codec_id::A_OPUS, 48000.0, 2))
            .unwrap();
        let init = m.begin_stream().unwrap();

        // Locate the Segment id in the prefix.
        let seg_id = [0x18, 0x53, 0x80, 0x67];
        let pos = init
            .windows(4)
            .position(|w| w == seg_id)
            .expect("Segment id present in init segment");
        assert_eq!(
            init[pos + 4],
            0xFF,
            "expected unknown-size VINT immediately after Segment id"
        );
    }

    #[test]
    fn video_keyframe_starts_new_cluster() {
        let mut m = Muxer::new(DocType::Webm);
        let v = m
            .register_track(TrackDescriptor::video(codec_id::V_VP9, 640, 480))
            .unwrap();
        m.append(v, b"I-frame-1", 0, true).unwrap();
        m.append(v, b"P-frame-1", 33_000_000, false).unwrap();
        m.append(v, b"I-frame-2", 66_000_000, true).unwrap();
        let bytes = m.finalize().unwrap();

        let d = Demuxer::parse(&bytes).unwrap();
        let mut frames = Frames::new(&bytes, &d);
        let f1 = frames.next_frame().unwrap().unwrap();
        let f2 = frames.next_frame().unwrap().unwrap();
        let f3 = frames.next_frame().unwrap().unwrap();
        assert_eq!(f1.timestamp_ns, 0);
        assert_eq!(f2.timestamp_ns, 33_000_000);
        assert_eq!(f3.timestamp_ns, 66_000_000);
        assert_eq!(f1.is_keyframe, Some(true));
        assert_eq!(f2.is_keyframe, Some(false));
        assert_eq!(f3.is_keyframe, Some(true));
    }
}
