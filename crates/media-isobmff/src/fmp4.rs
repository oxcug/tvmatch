//! Fragmented MP4 helpers, originating in media-core.
//!
//! Native box helpers construct initialization segments and fragments.
//! Codec-specific configuration bytes remain caller-owned.
//!
//! `write_fragment` writes a single-track fragment with `track_id = 1`
//! and Apple HLS brands (`msdh` / `msix`). Other public helpers support
//! caller-selected track IDs and multi-track segment construction.
//! Production writing uses [`crate::boxes`], with no `mp4-atom` calls or
//! bytewise patch passes.

use crate::IsobmffResult;
use crate::boxes::{
    Av01, Av1C, Avc1, Avc3, AvcC, Codec, Dinf, Dref, Edts, Encode, FixedPoint, FourCC, Ftyp, Hdlr,
    Hev1, Hvc1, HvcC, Mdat, Mdhd, Mdia, Mehd, Mfhd, Minf, Moof, Moov, Mvex, Mvhd, Pasp, Smhd, Stbl,
    Stco, Stsc, Stsd, Stsz, StszSamples, Stts, Styp, Tfdt, Tfhd, Tkhd, Traf, Trak, Trex, Trun,
    TrunEntry, Url, Visual, Vmhd,
};

/// Stub kept for callers that still want to express preroll intent —
/// returns `None` unconditionally. Initially the MKV pass-through path
/// wrote an `edts/elst` here to map wall-clock 0 → media-time =
/// `preroll`, but in practice neither ffmpeg's fragmented MP4 nor
/// Chrome's MSE pipeline behave correctly with that edit list: ffmpeg
/// emits no elst at all (cts values absorb the preroll instead), and
/// Chrome stalls when the elst is present but its `segment_duration`
/// doesn't match the MSE timeline. Matching ffmpeg's shape — no elst,
/// first sample's PTS = `preroll` — and letting the consumer apply
/// `SourceBuffer.timestampOffset = -preroll_secs` is the
/// MSE-canonical solution.
fn build_preroll_edts(_preroll_shift: u64, _segment_duration: u64) -> Option<Edts> {
    None
}

/// Build the common `Stbl` with empty sample tables, suitable for
/// fragmented MP4 (samples live in `traf`/`trun`, not `stbl`).
///
/// Origin: media-core.
pub fn empty_stbl(stsd: Stsd) -> Stbl {
    Stbl {
        stsd,
        stts: Stts { entries: vec![] },
        stsc: Stsc { entries: vec![] },
        stsz: Stsz {
            samples: StszSamples::Different { sizes: vec![] },
        },
        stco: Some(Stco { entries: vec![] }),
    }
}

/// Write a single-track `(styp + moof + mdat)` fragment to `out`.
///
/// - `track_id` is hard-coded to `1`.
/// - Brands are `msdh` (major) + `msdh`, `msix` (compatible) — the
///   Apple HLS fMP4 segment shape.
/// - `default_base_is_moof` is baked into the native `Tfhd` — no
///   post-encoding patch needed.
///
/// The two-pass encode (encode `moof` once to measure its size, then
/// re-encode with a correct `data_offset`) matches the original
/// `build_media_segment` and is required because `trun.data_offset`
/// is byte-offset-from-`moof`-start.
///
/// Origin: media-core.
pub fn write_fragment<W: std::io::Write>(
    out: &mut W,
    encoded_data: &[u8],
    trun_entries: Vec<TrunEntry>,
    sequence_number: u32,
    base_decode_time: u64,
    default_sample_duration: u32,
) -> IsobmffResult<()> {
    write_fragment_inner(
        out,
        encoded_data,
        trun_entries,
        sequence_number,
        base_decode_time,
        default_sample_duration,
        true,
    )
}

/// A fragment with NO `styp` — the form a standalone FILE wants.
///
/// `styp` marks the start of a SEGMENT, and a segment is a unit a reader may
/// reset its decoder at. Emitting one per fragment is right for MSE, where
/// each fragment is separately handed to `appendBuffer` and the caller has
/// declared it a segment by doing so. In a file it is a claim that the
/// fragment stands alone, and a fragment carrying P-frames does not: a reader
/// that honours the marker flushes its references and can only produce a
/// picture where a fragment happens to begin with an IDR.
///
/// Pair this with fragments cut at random-access points — one per GOP — which
/// is what a conventional fragmented file looks like. `trun_entries` may carry
/// many samples; `encoded_data` is their concatenation in the same order, each
/// entry's `size` selecting its slice.
pub fn write_media_fragment<W: std::io::Write>(
    out: &mut W,
    encoded_data: &[u8],
    trun_entries: Vec<TrunEntry>,
    sequence_number: u32,
    base_decode_time: u64,
    default_sample_duration: u32,
) -> IsobmffResult<()> {
    write_fragment_inner(
        out,
        encoded_data,
        trun_entries,
        sequence_number,
        base_decode_time,
        default_sample_duration,
        false,
    )
}

fn write_fragment_inner<W: std::io::Write>(
    out: &mut W,
    encoded_data: &[u8],
    trun_entries: Vec<TrunEntry>,
    sequence_number: u32,
    base_decode_time: u64,
    default_sample_duration: u32,
    segment_header: bool,
) -> IsobmffResult<()> {
    let native_entries = trun_entries;

    let styp = Styp {
        major_brand: crate::boxes::FourCC::new(b"msdh"),
        minor_version: 0,
        compatible_brands: vec![
            crate::boxes::FourCC::new(b"msdh"),
            crate::boxes::FourCC::new(b"msix"),
        ],
    };

    let tfhd = Tfhd {
        track_id: 1,
        base_data_offset: None,
        sample_description_index: Some(1),
        default_sample_duration: Some(default_sample_duration),
        default_sample_size: None,
        default_sample_flags: None,
    };
    let tfdt = Tfdt {
        base_media_decode_time: base_decode_time,
    };

    let mut buf = Vec::new();
    if segment_header {
        styp.encode(&mut buf)?;
    }

    // Pass 1: encode moof with placeholder data_offset to measure size.
    let moof_tmp = Moof {
        mfhd: Mfhd { sequence_number },
        traf: vec![Traf {
            tfhd: tfhd.clone(),
            tfdt: Some(tfdt.clone()),
            trun: vec![Trun {
                data_offset: Some(0),
                entries: native_entries.clone(),
            }],
        }],
    };
    let mut moof_buf = Vec::new();
    moof_tmp.encode(&mut moof_buf)?;
    let moof_size = moof_buf.len() as i32;

    // Pass 2: re-encode with correct data_offset = moof_size + 8 (mdat header).
    let moof = Moof {
        mfhd: Mfhd { sequence_number },
        traf: vec![Traf {
            tfhd,
            tfdt: Some(tfdt),
            trun: vec![Trun {
                data_offset: Some(moof_size + 8),
                entries: native_entries,
            }],
        }],
    };
    moof.encode(&mut buf)?;

    let mdat = Mdat {
        data: encoded_data.to_vec(),
    };
    mdat.encode(&mut buf)?;

    out.write_all(&buf)?;
    Ok(())
}

/// Which sample-entry / config-record pair the init segment emits.
/// `Avc` wraps the bytes in `avc1` + `avcC`; `Hevc` in `hvc1` + `hvcC`.
/// `HevcInband` is the same `hvcC` body wrapped in `hev1` instead of
/// `hvc1` — the brand difference is whether in-band parameter set NALs
/// (VPS/SPS/PPS) are permitted inside mdat samples (`hev1` permits,
/// `hvc1` forbids per ISO/IEC 14496-15 §8.4.3). Use `HevcInband` for
/// container pass-through (MKV / WebM / TS), where the source's
/// bitstream may carry inline PS that we don't strip — Firefox's
/// MSE-HEVC decoder rejects `hvc1` when PS NALs appear in mdat,
/// surfacing as `NS_ERROR_DOM_MEDIA_FATAL_ERR` from
/// `MediaDecoderStateMachineBase`. Use `Hevc` only when the caller has
/// verified that samples contain no in-band parameter sets.
///
/// `AvcInband` is the same `avcC` body wrapped in `avc3` instead of
/// `avc1` — the AVC counterpart of the `Hevc` / `HevcInband` pair above,
/// and it exists for the same reason. `avc1` forbids in-band parameter
/// set NALs in mdat; `avc3` permits them. Hardware encoders (VA-API
/// among them) emit SPS/PPS alongside every IDR, so an encoder-driven
/// path muxing as `avc1` writes a non-conformant file — and one that
/// may escape a decoder-only check on concatenated samples.
///
/// Do not infer parameter-set placement merely from the encoder API.
/// Prefer the in-band brands unless the caller verifies or removes in-band
/// parameter-set NALs: both
/// permit in-band sets without requiring them, so they stay correct for
/// an encoder that emits none.
///
/// `Av1` wraps the bytes in `av01` + `av1C` — the `configuration_record`
/// is the AV1CodecConfigurationRecord body (4-byte header + sequence
/// header OBU) supplied by the caller. AV1 has no in-band-vs-out-of-band brand split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitCodec {
    Avc,
    Hevc,
    HevcInband,
    AvcInband,
    Av1,
}

/// Parameters for a single-video-track fMP4 init segment.
///
/// `timescale` is the media-track timescale, conventionally 90000 for
/// streamed video. `configuration_record` is the raw decoder
/// configuration record bytes (`AVCDecoderConfigurationRecord` for
/// `Avc`, `HEVCDecoderConfigurationRecord` for `Hevc`) — the muxer
/// wraps them in the matching atom without reparsing.
pub struct VideoTrackParams {
    pub width: u16,
    pub height: u16,
    pub timescale: u32,
    pub configuration_record: Vec<u8>,
    pub codec: InitCodec,
}

/// Write a single-video-track fMP4 init segment (`ftyp + moov`) to `out`.
///
/// The init segment is codec-specific only via the
/// `stsd → {avc1 | hvc1}` payload — the surrounding `moov / trak /
/// stsd` shape is reused across codecs. Audio is out of scope: the
/// moov contains exactly one trak, with `vmhd` and no `smhd`.
///
/// Track 1 is the video track; mvex carries a single trex pointing at
/// it. Movie + media timescales are both set to `params.timescale` so
/// fragment `tfdt.base_media_decode_time` and the movie clock agree.
/// Declares no duration, which is correct for MSE and for live capture:
/// the length is not known when the init segment goes out, and the page
/// supplies it out of band. Use
/// [`write_video_init_segment_with_duration`] when writing a STANDALONE
/// file — a player has nothing else to build a timeline from, and one
/// that has to discover the end by running out of fragments will stop
/// early and look like a truncated encode.
pub fn write_video_init_segment<W: std::io::Write>(
    out: &mut W,
    params: VideoTrackParams,
) -> IsobmffResult<()> {
    write_video_init_segment_inner(out, params, 0, 0, 0)
}

/// Same as [`write_video_init_segment`] but declares the movie's total
/// length: `mehd.fragment_duration` plus matching `mvhd` / `tkhd` /
/// `mdhd` durations, all in `params.timescale` ticks.
///
/// This is the standalone-file form. `mehd` is the one that matters for
/// a fragmented movie — the header durations describe the samples listed
/// in `moov`, and a fragmented movie lists none there — but the headers
/// are written too, because readers disagree about which they trust and
/// a file that answers only one of them plays partway in the other half.
///
/// A `duration_ticks` of 0 is indistinguishable from not knowing, so it
/// takes the same path as [`write_video_init_segment`] rather than
/// declaring a zero-length movie.
pub fn write_video_init_segment_with_duration<W: std::io::Write>(
    out: &mut W,
    params: VideoTrackParams,
    duration_ticks: u64,
) -> IsobmffResult<()> {
    write_video_init_segment_inner(out, params, 0, 0, duration_ticks)
}

/// Same as [`write_video_init_segment`] but embeds an `edts/elst` that
/// shifts wall-clock 0 → `media_time = video_preroll_ticks`. Used by the
/// MKV pass-through path so it can emit DTS values shifted forward by
/// the source's reorder depth (guaranteeing `dts ≤ pts` per ISO/IEC
/// 14496-12 §8.6.1.2) without affecting where playback starts.
///
/// `video_segment_duration_ticks` is the track's playable duration in
/// the mvhd timescale (= `params.timescale` here). Must be non-zero
/// or MSE will silently drop the elst entry and refuse to start
/// playback (the buffered range stays anchored at `preroll`, leaving
/// `currentTime = 0` in the unbuffered zone).
///
/// MP4 pass-through and every transcode path get their DTS values
/// straight from the source's sample table or the encoder's
/// `EncodedPacket`, so they never need preroll — they call the
/// non-`_with_preroll` form.
pub fn write_video_init_segment_with_preroll<W: std::io::Write>(
    out: &mut W,
    params: VideoTrackParams,
    video_preroll_ticks: u64,
    video_segment_duration_ticks: u64,
) -> IsobmffResult<()> {
    write_video_init_segment_inner(
        out,
        params,
        video_preroll_ticks,
        video_segment_duration_ticks,
        0,
    )
}

fn write_video_init_segment_inner<W: std::io::Write>(
    out: &mut W,
    params: VideoTrackParams,
    video_preroll_ticks: u64,
    video_segment_duration_ticks: u64,
    total_duration_ticks: u64,
) -> IsobmffResult<()> {
    let mut buf = Vec::new();

    let ftyp = Ftyp {
        major_brand: FourCC::new(b"iso5"),
        minor_version: 0,
        compatible_brands: vec![
            FourCC::new(b"iso6"),
            FourCC::new(b"msdh"),
            FourCC::new(b"dash"),
        ],
    };
    ftyp.encode(&mut buf)?;

    let visual = Visual {
        data_reference_index: 1,
        width: params.width,
        height: params.height,
    };
    let codec = match params.codec {
        InitCodec::Avc => Codec::Avc1(Avc1 {
            visual,
            avcc: AvcC {
                configuration_record: params.configuration_record,
            },
            // 1:1 SAR — modern square-pixel sources. ffmpeg writes
            // pasp on every video sample entry; Safari is known to
            // stall MSE init when it's absent.
            pasp: Some(Pasp::default()),
            btrt: None,
        }),
        InitCodec::Hevc => Codec::Hvc1(Hvc1 {
            visual,
            hvcc: HvcC {
                configuration_record: params.configuration_record,
            },
            pasp: Some(Pasp::default()),
            btrt: None,
        }),
        InitCodec::HevcInband => Codec::Hev1(Hev1 {
            visual,
            hvcc: HvcC {
                configuration_record: params.configuration_record,
            },
            pasp: Some(Pasp::default()),
            btrt: None,
        }),
        InitCodec::AvcInband => Codec::Avc3(Avc3 {
            visual,
            avcc: AvcC {
                configuration_record: params.configuration_record,
            },
            pasp: Some(Pasp::default()),
            btrt: None,
        }),
        InitCodec::Av1 => Codec::Av01(Av01 {
            visual,
            av1c: Av1C {
                configuration_record: params.configuration_record,
            },
            pasp: Some(Pasp::default()),
            btrt: None,
        }),
    };
    let stsd = Stsd {
        codecs: vec![codec],
    };
    let stbl = empty_stbl(stsd);

    let trak = Trak {
        tkhd: Tkhd {
            track_id: 1,
            width: FixedPoint::new(params.width, 0),
            height: FixedPoint::new(params.height, 0),
            duration: total_duration_ticks,
            ..Tkhd::default()
        },
        edts: build_preroll_edts(video_preroll_ticks, video_segment_duration_ticks),
        mdia: Mdia {
            mdhd: Mdhd {
                timescale: params.timescale,
                language: "und".into(),
                duration: total_duration_ticks,
                ..Mdhd::default()
            },
            hdlr: Hdlr {
                handler: FourCC::new(b"vide"),
                name: "VideoHandler".into(),
            },
            minf: Minf {
                smhd: None,
                vmhd: Some(Vmhd::default()),
                dinf: Dinf {
                    dref: Dref {
                        urls: vec![Url::default()],
                    },
                },
                stbl,
            },
        },
    };

    let moov = Moov {
        mvhd: Mvhd {
            timescale: params.timescale,
            duration: total_duration_ticks,
            next_track_id: 2,
            ..Mvhd::default()
        },
        trak: vec![trak],
        mvex: Some(Mvex {
            // The declaration that actually carries a fragmented movie's
            // length. Absent when the writer does not know it, which is
            // the streaming case and is legal — see `Mehd`.
            mehd: (total_duration_ticks > 0).then_some(Mehd {
                fragment_duration: total_duration_ticks,
            }),
            trex: vec![Trex {
                track_id: 1,
                default_sample_description_index: 1,
                ..Trex::default()
            }],
        }),
    };
    moov.encode(&mut buf)?;

    out.write_all(&buf)?;
    Ok(())
}

// ─────────────────────────── audio + multi-track ───────────────────────────

/// Sample-entry shape for the audio trak inside a muxed A+V init
/// segment. Mirrors [`InitCodec`] for video; the muxer treats each
/// codec's config record as opaque pass-through bytes so it doesn't
/// have to reparse esds variable-length descriptors / dfLa metadata
/// blocks for the streams it forwards verbatim from a source MP4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioInitCodec {
    /// AAC — sample entry `mp4a`. `configuration_record` is the raw
    /// AudioSpecificConfig bytes (typically 2 for AAC-LC).
    Aac,
    /// FLAC — sample entry `fLaC`. `configuration_record` is the raw
    /// `dfLa` body (FullBox v0 header + STREAMINFO + ...).
    Flac,
    /// MP3 — sample entry `.mp3`. `configuration_record` is ignored
    /// (the stream is self-describing).
    Mp3,
    /// Opus — sample entry `Opus`. `configuration_record` is the raw
    /// `dOps` body (RFC 7845 §5.2: Version + OutputChannelCount +
    /// PreSkip + InputSampleRate + OutputGain + ChannelMappingFamily).
    Opus,
}

/// Audio-track side of a muxed fMP4 init segment. The receiver builds
/// its MSE `audio` mime from this — `mp4a.40.2` for AAC, `flac` for
/// FLAC, `mp4a.40.34` for MP3.
pub struct AudioTrackParams {
    pub codec: AudioInitCodec,
    pub channel_count: u16,
    pub sample_rate: u32,
    /// 16-bit by spec for PCM-derived sample entries; FLAC/AAC ignore.
    pub sample_size: u16,
    /// Track media timescale. Conventionally equals `sample_rate` for
    /// audio tracks.
    pub timescale: u32,
    /// Codec-specific config (see [`AudioInitCodec`]).
    pub configuration_record: Vec<u8>,
}

/// Build the raw bytes of an `mp4a` sample entry given AudioSpecificConfig.
///
/// The audio sample-entry header is fixed-shape (28 bytes); only the
/// nested esds is variable. We hand-build the esds with the 4-byte
/// expandable length encoding (`0x80 0x80 0x80 LEN`) that Apple
/// CoreMedia requires.
fn build_mp4a_entry_body(
    channel_count: u16,
    sample_rate: u32,
    sample_size: u16,
    asc: &[u8],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(64 + asc.len());
    // AudioSampleEntry header (ISO/IEC 14496-12 §8.5.2).
    body.extend_from_slice(&[0u8; 6]); // reserved
    body.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    body.extend_from_slice(&[0u8; 8]); // reserved (version + reserved + pre_defined)
    body.extend_from_slice(&channel_count.to_be_bytes());
    body.extend_from_slice(&sample_size.to_be_bytes());
    body.extend_from_slice(&[0u8; 2]); // pre_defined
    body.extend_from_slice(&[0u8; 2]); // reserved
    body.extend_from_slice(&((sample_rate << 16).to_be_bytes())); // 16.16 fixed

    // esds: FullBox v0 wrapping ES_Descriptor → DecoderConfigDescriptor
    //       → DecoderSpecificInfo (the AAC AudioSpecificConfig).
    let mut esds_body = Vec::with_capacity(40 + asc.len());
    esds_body.extend_from_slice(&[0, 0, 0, 0]); // version + flags

    // ES_Descriptor (tag 0x03).
    let dec_specific = build_descriptor(0x05, asc);
    let mut dec_config_body = Vec::with_capacity(13 + dec_specific.len());
    dec_config_body.push(0x40); // objectTypeIndication = AAC
    dec_config_body.push((0x05 << 2) | 1); // streamType=AudioStream(5), reserved bit
    dec_config_body.extend_from_slice(&[0u8; 3]); // bufferSizeDB
    dec_config_body.extend_from_slice(&0u32.to_be_bytes()); // maxBitrate
    dec_config_body.extend_from_slice(&0u32.to_be_bytes()); // avgBitrate
    dec_config_body.extend_from_slice(&dec_specific);
    let dec_config = build_descriptor(0x04, &dec_config_body);

    let sl_config = build_descriptor(0x06, &[2]); // pre-defined = MP4

    let mut es_body = Vec::with_capacity(3 + dec_config.len() + sl_config.len());
    es_body.extend_from_slice(&0u16.to_be_bytes()); // ES_ID
    es_body.push(0); // flags
    es_body.extend_from_slice(&dec_config);
    es_body.extend_from_slice(&sl_config);
    let es_descriptor = build_descriptor(0x03, &es_body);

    esds_body.extend_from_slice(&es_descriptor);

    // Wrap esds_body in the `esds` box header.
    let esds_total = (esds_body.len() + 8) as u32;
    body.extend_from_slice(&esds_total.to_be_bytes());
    body.extend_from_slice(b"esds");
    body.extend_from_slice(&esds_body);

    body
}

/// Build `[tag] [0x80 0x80 0x80 LEN] [body]` — the 4-byte-expandable
/// MPEG-4 descriptor encoding Apple CoreMedia accepts. Patch #3 in
/// `crate::patches`.
fn build_descriptor(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + body.len());
    out.push(tag);
    out.extend_from_slice(&[0x80, 0x80, 0x80, (body.len() & 0x7F) as u8]);
    out.extend_from_slice(body);
    out
}

/// Build the raw bytes of an `fLaC` sample entry given the dfLa body.
fn build_flac_entry_body(
    channel_count: u16,
    sample_rate: u32,
    sample_size: u16,
    dfla_body: &[u8],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(64 + dfla_body.len());
    body.extend_from_slice(&[0u8; 6]);
    body.extend_from_slice(&1u16.to_be_bytes());
    body.extend_from_slice(&[0u8; 8]);
    body.extend_from_slice(&channel_count.to_be_bytes());
    body.extend_from_slice(&sample_size.to_be_bytes());
    body.extend_from_slice(&[0u8; 2]);
    body.extend_from_slice(&[0u8; 2]);
    body.extend_from_slice(&((sample_rate << 16).to_be_bytes()));

    let dfla_total = (dfla_body.len() + 8) as u32;
    body.extend_from_slice(&dfla_total.to_be_bytes());
    body.extend_from_slice(b"dfLa");
    body.extend_from_slice(dfla_body);
    body
}

/// Build the raw bytes of an `Opus` sample entry given the `dOps` body
/// (RFC 7845 §5). The AudioSampleEntry `samplerate` field is conventionally
/// 48000 for Opus (it always decodes at 48 kHz); the original rate lives in
/// the `dOps` InputSampleRate field. `dops_body` is written verbatim inside
/// the child `dOps` box (same opaque-config contract as `dfLa`).
fn build_opus_entry_body(
    channel_count: u16,
    sample_rate: u32,
    sample_size: u16,
    dops_body: &[u8],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(40 + dops_body.len());
    body.extend_from_slice(&[0u8; 6]);
    body.extend_from_slice(&1u16.to_be_bytes());
    body.extend_from_slice(&[0u8; 8]);
    body.extend_from_slice(&channel_count.to_be_bytes());
    body.extend_from_slice(&sample_size.to_be_bytes());
    body.extend_from_slice(&[0u8; 2]);
    body.extend_from_slice(&[0u8; 2]);
    body.extend_from_slice(&((sample_rate << 16).to_be_bytes()));

    let dops_total = (dops_body.len() + 8) as u32;
    body.extend_from_slice(&dops_total.to_be_bytes());
    body.extend_from_slice(b"dOps");
    body.extend_from_slice(dops_body);
    body
}

/// Build the raw bytes of a `.mp3` sample entry. MP3 is self-describing,
/// so the entry is just the AudioSampleEntry header with no child config.
fn build_mp3_entry_body(channel_count: u16, sample_rate: u32, sample_size: u16) -> Vec<u8> {
    let mut body = Vec::with_capacity(32);
    body.extend_from_slice(&[0u8; 6]);
    body.extend_from_slice(&1u16.to_be_bytes());
    body.extend_from_slice(&[0u8; 8]);
    body.extend_from_slice(&channel_count.to_be_bytes());
    body.extend_from_slice(&sample_size.to_be_bytes());
    body.extend_from_slice(&[0u8; 2]);
    body.extend_from_slice(&[0u8; 2]);
    body.extend_from_slice(&((sample_rate << 16).to_be_bytes()));
    body
}

fn audio_codec_entry(p: &AudioTrackParams) -> Codec {
    let sample_size = if p.sample_size == 0 {
        16
    } else {
        p.sample_size
    };
    match p.codec {
        AudioInitCodec::Aac => Codec::RawEntry {
            kind: FourCC::new(b"mp4a"),
            body: build_mp4a_entry_body(
                p.channel_count,
                p.sample_rate,
                sample_size,
                &p.configuration_record,
            ),
        },
        AudioInitCodec::Flac => Codec::RawEntry {
            kind: FourCC::new(b"fLaC"),
            body: build_flac_entry_body(
                p.channel_count,
                p.sample_rate,
                sample_size,
                &p.configuration_record,
            ),
        },
        AudioInitCodec::Mp3 => Codec::RawEntry {
            kind: FourCC::new(b".mp3"),
            body: build_mp3_entry_body(p.channel_count, p.sample_rate, sample_size),
        },
        AudioInitCodec::Opus => Codec::RawEntry {
            kind: FourCC::new(b"Opus"),
            body: build_opus_entry_body(
                p.channel_count,
                p.sample_rate,
                sample_size,
                &p.configuration_record,
            ),
        },
    }
}

fn audio_trak(track_id: u32, p: &AudioTrackParams) -> Trak {
    let stsd = Stsd {
        codecs: vec![audio_codec_entry(p)],
    };
    let stbl = empty_stbl(stsd);
    Trak {
        tkhd: Tkhd {
            track_id,
            // Audio traks set tkhd width/height to 0.
            width: FixedPoint::new(0, 0),
            height: FixedPoint::new(0, 0),
            volume: FixedPoint::new(1, 0),
            ..Tkhd::default()
        },
        edts: None,
        mdia: Mdia {
            mdhd: Mdhd {
                timescale: p.timescale,
                language: "und".into(),
                ..Mdhd::default()
            },
            hdlr: Hdlr {
                handler: FourCC::new(b"soun"),
                name: "SoundHandler".into(),
            },
            minf: Minf {
                smhd: Some(Smhd::default()),
                vmhd: None,
                dinf: Dinf {
                    dref: Dref {
                        urls: vec![Url::default()],
                    },
                },
                stbl,
            },
        },
    }
}

/// Write a muxed video + optional audio fMP4 init segment.
///
/// Video is track 1, audio (when present) is track 2. The receiver
/// builds its MSE mime string with both codec suffixes (e.g.
/// `video/mp4; codecs="avc1.640028, mp4a.40.2"`) and opens one
/// SourceBuffer; subsequent fragments are produced by
/// [`write_av_fragment`].
///
/// `mvhd.timescale` is taken from the video track to keep the movie
/// clock aligned with the video sample-timing math; per-track
/// timescales are independent.
pub fn write_av_init_segment<W: std::io::Write>(
    out: &mut W,
    video: VideoTrackParams,
    audio: Option<AudioTrackParams>,
) -> IsobmffResult<()> {
    write_av_init_segment_inner(out, video, audio, 0, 0)
}

/// Same as [`write_av_init_segment`] but embeds an `edts/elst` shifting
/// wall-clock 0 → `media_time = video_preroll_ticks` on the video trak.
/// `video_segment_duration_ticks` is the video track's total playable
/// duration in mvhd-timescale ticks — non-zero, or MSE drops the
/// edit-list entry. See [`write_video_init_segment_with_preroll`].
pub fn write_av_init_segment_with_preroll<W: std::io::Write>(
    out: &mut W,
    video: VideoTrackParams,
    audio: Option<AudioTrackParams>,
    video_preroll_ticks: u64,
    video_segment_duration_ticks: u64,
) -> IsobmffResult<()> {
    write_av_init_segment_inner(
        out,
        video,
        audio,
        video_preroll_ticks,
        video_segment_duration_ticks,
    )
}

fn write_av_init_segment_inner<W: std::io::Write>(
    out: &mut W,
    video: VideoTrackParams,
    audio: Option<AudioTrackParams>,
    video_preroll_ticks: u64,
    video_segment_duration_ticks: u64,
) -> IsobmffResult<()> {
    let mut buf = Vec::new();

    let ftyp = Ftyp {
        major_brand: FourCC::new(b"iso5"),
        minor_version: 0,
        compatible_brands: vec![
            FourCC::new(b"iso6"),
            FourCC::new(b"msdh"),
            FourCC::new(b"dash"),
        ],
    };
    ftyp.encode(&mut buf)?;

    let visual = Visual {
        data_reference_index: 1,
        width: video.width,
        height: video.height,
    };
    let video_codec = match video.codec {
        InitCodec::Avc => Codec::Avc1(Avc1 {
            visual,
            avcc: AvcC {
                configuration_record: video.configuration_record,
            },
            pasp: Some(Pasp::default()),
            btrt: None,
        }),
        InitCodec::Hevc => Codec::Hvc1(Hvc1 {
            visual,
            hvcc: HvcC {
                configuration_record: video.configuration_record,
            },
            pasp: Some(Pasp::default()),
            btrt: None,
        }),
        InitCodec::HevcInband => Codec::Hev1(Hev1 {
            visual,
            hvcc: HvcC {
                configuration_record: video.configuration_record,
            },
            pasp: Some(Pasp::default()),
            btrt: None,
        }),
        InitCodec::AvcInband => Codec::Avc3(Avc3 {
            visual,
            avcc: AvcC {
                configuration_record: video.configuration_record,
            },
            pasp: Some(Pasp::default()),
            btrt: None,
        }),
        InitCodec::Av1 => Codec::Av01(Av01 {
            visual,
            av1c: Av1C {
                configuration_record: video.configuration_record,
            },
            pasp: Some(Pasp::default()),
            btrt: None,
        }),
    };
    let video_stsd = Stsd {
        codecs: vec![video_codec],
    };
    let video_stbl = empty_stbl(video_stsd);

    let video_trak = Trak {
        tkhd: Tkhd {
            track_id: 1,
            width: FixedPoint::new(video.width, 0),
            height: FixedPoint::new(video.height, 0),
            ..Tkhd::default()
        },
        edts: build_preroll_edts(video_preroll_ticks, video_segment_duration_ticks),
        mdia: Mdia {
            mdhd: Mdhd {
                timescale: video.timescale,
                language: "und".into(),
                ..Mdhd::default()
            },
            hdlr: Hdlr {
                handler: FourCC::new(b"vide"),
                name: "VideoHandler".into(),
            },
            minf: Minf {
                smhd: None,
                vmhd: Some(Vmhd::default()),
                dinf: Dinf {
                    dref: Dref {
                        urls: vec![Url::default()],
                    },
                },
                stbl: video_stbl,
            },
        },
    };

    let mut traks = vec![video_trak];
    let mut trex_entries = vec![Trex {
        track_id: 1,
        default_sample_description_index: 1,
        ..Trex::default()
    }];
    let next_track_id = match &audio {
        Some(a) => {
            traks.push(audio_trak(2, a));
            trex_entries.push(Trex {
                track_id: 2,
                default_sample_description_index: 1,
                ..Trex::default()
            });
            3
        }
        None => 2,
    };

    let moov = Moov {
        mvhd: Mvhd {
            timescale: video.timescale,
            next_track_id,
            ..Mvhd::default()
        },
        trak: traks,
        // No `mehd`: this streaming A+V API does not accept a duration.
        // Only the video-only `_with_duration` API writes that declaration.
        mvex: Some(Mvex {
            mehd: None,
            trex: trex_entries,
        }),
    };
    moov.encode(&mut buf)?;

    out.write_all(&buf)?;
    Ok(())
}

/// Per-track data carried in one fragment.
pub struct FragmentTrack<'a> {
    pub track_id: u32,
    pub trun_entries: Vec<TrunEntry>,
    pub default_sample_duration: u32,
    pub base_decode_time: u64,
    /// Track sample bytes, concatenated in trun order. They land in the
    /// shared `mdat` after all previous tracks' bytes.
    pub sample_data: &'a [u8],
}

/// Write a multi-track `(styp + moof + mdat)` fragment.
///
/// One `moof` carries N `traf`s (one per `tracks` entry); the single
/// `mdat` carries each track's `sample_data` concatenated in input
/// order. Each traf's `trun.data_offset` points to its slice inside
/// the mdat. Track 1 is conventionally video; track 2 audio (when
/// present).
pub fn write_av_fragment<W: std::io::Write>(
    out: &mut W,
    tracks: &[FragmentTrack<'_>],
    sequence_number: u32,
) -> IsobmffResult<()> {
    if tracks.is_empty() {
        return Ok(());
    }

    let styp = Styp {
        major_brand: FourCC::new(b"msdh"),
        minor_version: 0,
        compatible_brands: vec![FourCC::new(b"msdh"), FourCC::new(b"msix")],
    };

    // Build moof_tmp with placeholder data_offsets to measure its
    // encoded size. Then re-encode with real data_offsets that land
    // each track's bytes at the right position inside the mdat.
    let trafs_tmp: Vec<Traf> = tracks
        .iter()
        .map(|t| Traf {
            tfhd: Tfhd {
                track_id: t.track_id,
                base_data_offset: None,
                sample_description_index: Some(1),
                default_sample_duration: Some(t.default_sample_duration),
                default_sample_size: None,
                default_sample_flags: None,
            },
            tfdt: Some(Tfdt {
                base_media_decode_time: t.base_decode_time,
            }),
            trun: vec![Trun {
                data_offset: Some(0),
                entries: t.trun_entries.clone(),
            }],
        })
        .collect();
    let moof_tmp = Moof {
        mfhd: Mfhd { sequence_number },
        traf: trafs_tmp,
    };
    let mut moof_buf = Vec::new();
    moof_tmp.encode(&mut moof_buf)?;
    let moof_size = moof_buf.len() as i32;

    // Real moof: each traf's data_offset = moof_size + 8 (mdat header)
    // + cumulative byte offset of its track inside mdat.
    let mut buf = Vec::new();
    styp.encode(&mut buf)?;

    let mut cumulative: i32 = 0;
    let trafs: Vec<Traf> = tracks
        .iter()
        .map(|t| {
            let data_offset = moof_size + 8 + cumulative;
            cumulative += t.sample_data.len() as i32;
            Traf {
                tfhd: Tfhd {
                    track_id: t.track_id,
                    base_data_offset: None,
                    sample_description_index: Some(1),
                    default_sample_duration: Some(t.default_sample_duration),
                    default_sample_size: None,
                    default_sample_flags: None,
                },
                tfdt: Some(Tfdt {
                    base_media_decode_time: t.base_decode_time,
                }),
                trun: vec![Trun {
                    data_offset: Some(data_offset),
                    entries: t.trun_entries.clone(),
                }],
            }
        })
        .collect();
    let moof = Moof {
        mfhd: Mfhd { sequence_number },
        traf: trafs,
    };
    moof.encode(&mut buf)?;

    // Concatenated mdat.
    let mut mdat_bytes = Vec::with_capacity(cumulative as usize);
    for t in tracks {
        mdat_bytes.extend_from_slice(t.sample_data);
    }
    let mdat = Mdat { data: mdat_bytes };
    mdat.encode(&mut buf)?;

    out.write_all(&buf)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_stsd() -> Stsd {
        // Minimal valid Stsd. Codec-agnostic; just needs to encode without
        // panicking. Empty entries vec is acceptable per ISO/IEC 14496-12.
        Stsd { codecs: vec![] }
    }

    #[test]
    fn empty_stbl_has_zero_sample_tables() {
        let stbl = empty_stbl(dummy_stsd());
        assert!(stbl.stts.entries.is_empty());
        assert!(stbl.stsc.entries.is_empty());
        match stbl.stsz.samples {
            StszSamples::Different { ref sizes } => assert!(sizes.is_empty()),
            _ => panic!("expected Different samples"),
        }
        assert!(stbl.stco.is_some());
    }

    #[test]
    fn write_fragment_emits_styp_moof_mdat_in_order() {
        let trun_entries = vec![TrunEntry {
            duration: Some(1024),
            size: Some(8),
            flags: None,
            cts: None,
        }];
        let mut out = Vec::new();
        write_fragment(
            &mut out,
            b"abcdefgh", // 8 bytes of fake encoded data
            trun_entries,
            1,    // sequence_number
            0,    // base_decode_time
            1024, // default_sample_duration
        )
        .unwrap();

        // Confirm box order: styp comes before moof comes before mdat.
        let styp_pos = out.windows(4).position(|w| w == b"styp").unwrap();
        let moof_pos = out.windows(4).position(|w| w == b"moof").unwrap();
        let mdat_pos = out.windows(4).position(|w| w == b"mdat").unwrap();
        assert!(styp_pos < moof_pos);
        assert!(moof_pos < mdat_pos);
    }

    #[test]
    fn write_fragment_applies_tfhd_patch() {
        let trun_entries = vec![TrunEntry {
            duration: Some(1024),
            size: Some(4),
            flags: None,
            cts: None,
        }];
        let mut out = Vec::new();
        write_fragment(&mut out, b"abcd", trun_entries, 1, 0, 1024).unwrap();

        // tfhd flags high byte should have 0x02 set (default-base-is-moof).
        let tfhd_pos = out.windows(4).position(|w| w == b"tfhd").unwrap();
        assert_eq!(out[tfhd_pos + 5] & 0x02, 0x02);
    }

    #[test]
    fn write_fragment_data_offset_resolves_to_mdat_payload() {
        // Two samples of 4 bytes each → mdat payload = 8 bytes.
        let trun_entries = vec![
            TrunEntry {
                duration: Some(1024),
                size: Some(4),
                flags: None,
                cts: None,
            },
            TrunEntry {
                duration: Some(1024),
                size: Some(4),
                flags: None,
                cts: None,
            },
        ];
        let mut out = Vec::new();
        write_fragment(&mut out, b"AAAABBBB", trun_entries, 7, 0, 1024).unwrap();

        // The mdat payload starts 8 bytes (mdat header) past the mdat fourcc.
        // The trun.data_offset (read at runtime) should land at that position
        // when interpreted as offset-from-moof-start.
        let moof_pos = out.windows(4).position(|w| w == b"moof").unwrap();
        let mdat_pos = out.windows(4).position(|w| w == b"mdat").unwrap();
        let mdat_payload_pos = mdat_pos + 4; // after "mdat" fourcc
        let expected_offset = mdat_payload_pos as i32 - (moof_pos - 4) as i32;

        // Find the trun's data_offset field. trun layout:
        // [size(4)] trun(4) version(1) flags(3) sample_count(4) data_offset(4)?
        let trun_pos = out.windows(4).position(|w| w == b"trun").unwrap();
        let data_offset_pos = trun_pos + 4 + 4 + 4; // after fourcc + version/flags + sample_count
        let data_offset = i32::from_be_bytes(
            out[data_offset_pos..data_offset_pos + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(data_offset, expected_offset);
    }

    /// Sentinel AVCDecoderConfigurationRecord — not a real one. We only
    /// need a recognizable byte pattern so the test can confirm the
    /// init-segment writer wraps it in `avcC` verbatim.
    const FAKE_AVCC: &[u8] = &[
        0x01, 0x42, 0xC0, 0x1F, 0xFF, 0xE1, 0x00, 0x05, 0x67, 0x42, 0xC0, 0x1F, 0x00, 0x01, 0x00,
        0x00,
    ];

    #[test]
    fn write_video_init_segment_emits_ftyp_then_moov() {
        let mut out = Vec::new();
        write_video_init_segment(
            &mut out,
            VideoTrackParams {
                width: 1920,
                height: 1080,
                timescale: 90000,
                configuration_record: FAKE_AVCC.to_vec(),
                codec: InitCodec::Avc,
            },
        )
        .unwrap();
        let ftyp = out.windows(4).position(|w| w == b"ftyp").unwrap();
        let moov = out.windows(4).position(|w| w == b"moov").unwrap();
        assert!(ftyp < moov);
        // No styp/moof/mdat in an init segment.
        assert!(out.windows(4).position(|w| w == b"styp").is_none());
        assert!(out.windows(4).position(|w| w == b"moof").is_none());
        assert!(out.windows(4).position(|w| w == b"mdat").is_none());
    }

    #[test]
    fn write_video_init_segment_has_vmhd_and_no_smhd() {
        let mut out = Vec::new();
        write_video_init_segment(
            &mut out,
            VideoTrackParams {
                width: 640,
                height: 360,
                timescale: 90000,
                configuration_record: FAKE_AVCC.to_vec(),
                codec: InitCodec::Avc,
            },
        )
        .unwrap();
        assert!(out.windows(4).any(|w| w == b"vmhd"), "vmhd missing");
        assert!(
            !out.windows(4).any(|w| w == b"smhd"),
            "smhd must not appear in a video init segment"
        );
        // Handler must be `vide`. Find hdlr, skip fullbox header + 4 bytes
        // pre_defined, read the handler 4cc.
        let hdlr_pos = out.windows(4).position(|w| w == b"hdlr").unwrap();
        let handler = &out[hdlr_pos + 4 + 4 + 4..hdlr_pos + 4 + 4 + 8];
        assert_eq!(handler, b"vide");
    }

    #[test]
    fn write_video_init_segment_carries_avcc_verbatim() {
        let mut out = Vec::new();
        write_video_init_segment(
            &mut out,
            VideoTrackParams {
                width: 1280,
                height: 720,
                timescale: 90000,
                configuration_record: FAKE_AVCC.to_vec(),
                codec: InitCodec::Avc,
            },
        )
        .unwrap();
        let avcc = out
            .windows(4)
            .position(|w| w == b"avcC")
            .expect("avcC missing");
        // avcC body starts immediately after fourcc (Atom, not FullBox).
        let body_start = avcc + 4;
        assert_eq!(&out[body_start..body_start + FAKE_AVCC.len()], FAKE_AVCC);
    }

    #[test]
    fn write_video_init_segment_propagates_dimensions_into_tkhd_and_avc1() {
        let mut out = Vec::new();
        write_video_init_segment(
            &mut out,
            VideoTrackParams {
                width: 1280,
                height: 720,
                timescale: 90000,
                configuration_record: FAKE_AVCC.to_vec(),
                codec: InitCodec::Avc,
            },
        )
        .unwrap();
        // tkhd v0 layout from the fourcc: fourcc(4) + version(1) + flags(3)
        // + ct(4) + mt(4) + track_id(4) + reserved(4) + duration(4) +
        // reserved(8) + layer(2) + alt_group(2) + volume(2) + reserved(2)
        // + matrix(36) = 4 + 4 + 72 = 80 bytes before width(u16,u16).
        // (v1 would push everything +12 bytes; we now adapt-emit v0
        // when duration fits in u32, which is always true here since
        // the init segment writes duration=0.)
        let tkhd = out.windows(4).position(|w| w == b"tkhd").unwrap();
        let width_off = tkhd + 80;
        let w_int = u16::from_be_bytes(out[width_off..width_off + 2].try_into().unwrap());
        let h_int = u16::from_be_bytes(out[width_off + 4..width_off + 6].try_into().unwrap());
        assert_eq!(w_int, 1280);
        assert_eq!(h_int, 720);

        // avc1 sample entry: header(8) + SampleEntry(8) + pre_defined(2) +
        // reserved(2) + pre_defined[3](12) = 32 bytes before width/height.
        let avc1 = out.windows(4).position(|w| w == b"avc1").unwrap();
        let vis_dim = avc1 + 4 + 6 + 2 + 2 + 2 + 12; // = avc1 + 28
        let vw = u16::from_be_bytes(out[vis_dim..vis_dim + 2].try_into().unwrap());
        let vh = u16::from_be_bytes(out[vis_dim + 2..vis_dim + 4].try_into().unwrap());
        assert_eq!(vw, 1280);
        assert_eq!(vh, 720);
    }

    #[test]
    fn write_video_init_segment_writes_one_trex_for_track_1() {
        let mut out = Vec::new();
        write_video_init_segment(
            &mut out,
            VideoTrackParams {
                width: 16,
                height: 16,
                timescale: 90000,
                configuration_record: FAKE_AVCC.to_vec(),
                codec: InitCodec::Avc,
            },
        )
        .unwrap();
        let trex = out
            .windows(4)
            .position(|w| w == b"trex")
            .expect("trex missing");
        // trex body: FullBox(4) + track_id(4) + dsdi(4) + ...
        let track_id_off = trex + 4 + 4;
        let track_id = u32::from_be_bytes(out[track_id_off..track_id_off + 4].try_into().unwrap());
        assert_eq!(track_id, 1);
        // Single trex inside mvex; the next 4cc after this one shouldn't
        // be another trex.
        let after = trex + 8;
        let next_trex = out[after..].windows(4).position(|w| w == b"trex");
        assert!(next_trex.is_none(), "expected exactly one trex");
    }

    // ─────────── A+V init / fragment ───────────

    fn video_params() -> VideoTrackParams {
        VideoTrackParams {
            width: 1280,
            height: 720,
            timescale: 90_000,
            configuration_record: FAKE_AVCC.to_vec(),
            codec: InitCodec::Avc,
        }
    }

    /// AAC AudioSpecificConfig: AAC-LC (profile=2), 48 kHz (freq_idx=3),
    /// stereo (chan_conf=2) → 2 bytes = 0x11 0x90.
    const FAKE_AASC: &[u8] = &[0x11, 0x90];

    fn audio_aac_params() -> AudioTrackParams {
        AudioTrackParams {
            codec: AudioInitCodec::Aac,
            channel_count: 2,
            sample_rate: 48_000,
            sample_size: 16,
            timescale: 48_000,
            configuration_record: FAKE_AASC.to_vec(),
        }
    }

    #[test]
    fn write_av_init_segment_emits_video_only_when_audio_is_none() {
        let mut out = Vec::new();
        write_av_init_segment(&mut out, video_params(), None).unwrap();
        // Two traks would mean two `tkhd` boxes; verify only one.
        let tkhds: Vec<_> = out
            .windows(4)
            .enumerate()
            .filter(|(_, w)| *w == b"tkhd")
            .collect();
        assert_eq!(
            tkhds.len(),
            1,
            "expected exactly one tkhd when audio is None"
        );
        assert!(
            !out.windows(4).any(|w| w == b"soun"),
            "soun handler must not appear"
        );
        assert!(
            !out.windows(4).any(|w| w == b"smhd"),
            "smhd must not appear"
        );
    }

    #[test]
    fn write_av_init_segment_emits_video_plus_aac() {
        let mut out = Vec::new();
        write_av_init_segment(&mut out, video_params(), Some(audio_aac_params())).unwrap();

        // Two tkhd (video + audio).
        let tkhds: Vec<_> = out
            .windows(4)
            .enumerate()
            .filter(|(_, w)| *w == b"tkhd")
            .collect();
        assert_eq!(tkhds.len(), 2);

        // Both vmhd (video) and smhd (audio) present.
        assert!(out.windows(4).any(|w| w == b"vmhd"));
        assert!(out.windows(4).any(|w| w == b"smhd"));

        // Audio handler is `soun`.
        assert!(out.windows(4).any(|w| w == b"soun"));

        // Sample entry is mp4a; its esds carries our AudioSpecificConfig.
        assert!(out.windows(4).any(|w| w == b"mp4a"));
        assert!(out.windows(4).any(|w| w == b"esds"));
        // Our 2-byte ASC should appear verbatim in the esds blob.
        assert!(out.windows(2).any(|w| w == FAKE_AASC));

        // mvex carries two trex entries.
        let mvex = out.windows(4).position(|w| w == b"mvex").unwrap();
        let trex_count = out[mvex..].windows(4).filter(|w| *w == b"trex").count();
        assert_eq!(trex_count, 2);
    }

    #[test]
    fn write_av_init_segment_emits_opus_dops() {
        // dOps body (RFC 7845 §5.2): version=0, 2ch, pre_skip=312,
        // input_sr=48000, gain=0, mapping_family=0.
        let dops_body: Vec<u8> = vec![
            0, // Version
            2, // OutputChannelCount
            0x01, 0x38, // PreSkip = 312
            0x00, 0x00, 0xBB, 0x80, // InputSampleRate = 48000
            0x00, 0x00, // OutputGain
            0x00, // ChannelMappingFamily
        ];
        let mut params = audio_aac_params();
        params.codec = AudioInitCodec::Opus;
        params.configuration_record = dops_body.clone();

        let mut out = Vec::new();
        write_av_init_segment(&mut out, video_params(), Some(params)).unwrap();

        // `Opus` sample entry + child `dOps` box present.
        assert!(
            out.windows(4).any(|w| w == b"Opus"),
            "Opus sample entry missing"
        );
        let dops = out
            .windows(4)
            .position(|w| w == b"dOps")
            .expect("dOps missing");
        // The dOps body immediately follows its 8-byte box header, verbatim.
        let start = dops + 4;
        assert_eq!(&out[start..start + dops_body.len()], dops_body.as_slice());
        assert!(out.windows(4).any(|w| w == b"soun"));
    }

    #[test]
    fn write_av_init_segment_emits_flac_dfla() {
        let mut dfla_body: Vec<u8> = vec![
            0, 0, 0, 0, // FullBox header
            0x80, 0x00, 0x00, 0x22, // last-block + StreamInfo (type 0, len 34)
        ];
        dfla_body.extend_from_slice(&[0u8; 34]); // fake STREAMINFO body
        let mut params = audio_aac_params();
        params.codec = AudioInitCodec::Flac;
        params.configuration_record = dfla_body;

        let mut out = Vec::new();
        write_av_init_segment(&mut out, video_params(), Some(params)).unwrap();
        assert!(out.windows(4).any(|w| w == b"fLaC"));
        assert!(out.windows(4).any(|w| w == b"dfLa"));
    }

    #[test]
    fn write_av_init_segment_emits_mp3_sample_entry() {
        let mut params = audio_aac_params();
        params.codec = AudioInitCodec::Mp3;
        params.configuration_record = Vec::new();
        let mut out = Vec::new();
        write_av_init_segment(&mut out, video_params(), Some(params)).unwrap();
        assert!(out.windows(4).any(|w| w == b".mp3"));
        // No esds for MP3 (the stream is self-describing).
        assert!(!out.windows(4).any(|w| w == b"esds"));
    }

    #[test]
    fn write_av_fragment_emits_one_moof_with_two_trafs() {
        let v_entries = vec![TrunEntry {
            duration: Some(3000),
            size: Some(4),
            flags: None,
            cts: None,
        }];
        let a_entries = vec![TrunEntry {
            duration: Some(1024),
            size: Some(3),
            flags: None,
            cts: None,
        }];
        let mut out = Vec::new();
        write_av_fragment(
            &mut out,
            &[
                FragmentTrack {
                    track_id: 1,
                    trun_entries: v_entries,
                    default_sample_duration: 3000,
                    base_decode_time: 0,
                    sample_data: b"VVVV",
                },
                FragmentTrack {
                    track_id: 2,
                    trun_entries: a_entries,
                    default_sample_duration: 1024,
                    base_decode_time: 0,
                    sample_data: b"AAA",
                },
            ],
            1,
        )
        .unwrap();

        // Two trafs.
        let traf_count = out.windows(4).filter(|w| *w == b"traf").count();
        assert_eq!(traf_count, 2);

        // One mdat.
        let mdat_count = out.windows(4).filter(|w| *w == b"mdat").count();
        assert_eq!(mdat_count, 1);

        // mdat payload = "VVVVAAA" (video bytes followed by audio).
        let mdat = out.windows(4).position(|w| w == b"mdat").unwrap();
        let payload_start = mdat + 4;
        assert_eq!(&out[payload_start..payload_start + 7], b"VVVVAAA");
    }

    #[test]
    fn write_av_fragment_data_offsets_land_at_right_track_bytes() {
        let v_entries = vec![TrunEntry {
            duration: Some(3000),
            size: Some(4),
            flags: None,
            cts: None,
        }];
        let a_entries = vec![TrunEntry {
            duration: Some(1024),
            size: Some(3),
            flags: None,
            cts: None,
        }];
        let mut out = Vec::new();
        write_av_fragment(
            &mut out,
            &[
                FragmentTrack {
                    track_id: 1,
                    trun_entries: v_entries,
                    default_sample_duration: 3000,
                    base_decode_time: 0,
                    sample_data: b"VVVV",
                },
                FragmentTrack {
                    track_id: 2,
                    trun_entries: a_entries,
                    default_sample_duration: 1024,
                    base_decode_time: 0,
                    sample_data: b"AAA",
                },
            ],
            7,
        )
        .unwrap();

        // Both truns should carry data_offset. The first (video) lands
        // at the start of mdat payload; the second (audio) lands 4 bytes
        // later.
        let moof_pos = out.windows(4).position(|w| w == b"moof").unwrap();
        let trun_positions: Vec<_> = out
            .windows(4)
            .enumerate()
            .filter(|(_, w)| *w == b"trun")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(trun_positions.len(), 2);
        let read_offset = |trun_pos: usize| -> i32 {
            let data_offset_pos = trun_pos + 4 + 4 + 4; // fourcc + version+flags + sample_count
            i32::from_be_bytes(
                out[data_offset_pos..data_offset_pos + 4]
                    .try_into()
                    .unwrap(),
            )
        };
        let v_offset = read_offset(trun_positions[0]);
        let a_offset = read_offset(trun_positions[1]);

        // Audio data_offset = video data_offset + 4 (length of video sample bytes).
        assert_eq!(a_offset - v_offset, 4);

        // Video data_offset should resolve to the start of mdat payload
        // relative to moof start.
        let mdat = out.windows(4).position(|w| w == b"mdat").unwrap();
        let mdat_payload = mdat + 4;
        let expected_v = mdat_payload as i32 - (moof_pos - 4) as i32;
        assert_eq!(v_offset, expected_v);
    }

    // ── Brand selection: avc1/avc3 and hvc1/hev1 ──────────────────────────
    //
    // The in-band brands differ from their out-of-band twins in the FourCC
    // and NOTHING else, so these assert both halves: the right 4cc, and a
    // body identical to the twin's. A future edit that gave `avc3` its own
    // body shape would be caught here rather than in a player.
    //
    // Worth having because the absence of exactly this test is how the
    // muxer came to write `avc1` over a VA-API stream that carries SPS/PPS
    // per IDR: `HevcInband` had existed for a while with no coverage at
    // all, and `AvcInband` did not exist.

    fn video_init(codec: InitCodec, config: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_video_init_segment(
            &mut out,
            VideoTrackParams {
                width: 320,
                height: 240,
                timescale: 90000,
                configuration_record: config.to_vec(),
                codec,
            },
        )
        .unwrap();
        out
    }

    /// The sample entry a 4cc names, as `(offset, body)` — everything after
    /// the 8-byte box header, so two brands can be compared directly.
    fn entry_body(seg: &[u8], fourcc: &[u8; 4]) -> Vec<u8> {
        let at = seg
            .windows(4)
            .position(|w| w == fourcc)
            .unwrap_or_else(|| panic!("no {} sample entry", String::from_utf8_lossy(fourcc)));
        let size = u32::from_be_bytes(seg[at - 4..at].try_into().unwrap()) as usize;
        seg[at + 4..at - 4 + size].to_vec()
    }

    #[test]
    fn avc_inband_writes_an_avc3_sample_entry_and_plain_avc_writes_avc1() {
        let inband = video_init(InitCodec::AvcInband, FAKE_AVCC);
        let plain = video_init(InitCodec::Avc, FAKE_AVCC);

        assert!(
            inband.windows(4).any(|w| w == b"avc3"),
            "AvcInband must write avc3"
        );
        assert!(
            !inband.windows(4).any(|w| w == b"avc1"),
            "AvcInband must NOT also leave an avc1 entry — the brand is the whole signal"
        );
        assert!(
            plain.windows(4).any(|w| w == b"avc1"),
            "Avc must write avc1"
        );
        assert!(
            !plain.windows(4).any(|w| w == b"avc3"),
            "Avc must not write avc3"
        );
    }

    #[test]
    fn avc3_and_avc1_differ_in_the_fourcc_and_nothing_else() {
        let inband = video_init(InitCodec::AvcInband, FAKE_AVCC);
        let plain = video_init(InitCodec::Avc, FAKE_AVCC);
        assert_eq!(
            entry_body(&inband, b"avc3"),
            entry_body(&plain, b"avc1"),
            "avc3 is avc1 with a different brand; a divergent body is a bug"
        );
        // Same length overall, so nothing was added or dropped alongside it.
        assert_eq!(inband.len(), plain.len());
    }

    #[test]
    fn the_avcc_survives_into_the_avc3_entry() {
        // avc3 PERMITS in-band parameter sets; it does not excuse dropping
        // the out-of-band copy, and a decoder configured from the sample
        // entry alone must still find one.
        let seg = video_init(InitCodec::AvcInband, FAKE_AVCC);
        let body = entry_body(&seg, b"avc3");
        assert!(
            body.windows(FAKE_AVCC.len()).any(|w| w == FAKE_AVCC),
            "the avcC body must still be present under avc3"
        );
    }

    #[test]
    fn hevc_inband_writes_hev1_and_is_otherwise_hvc1() {
        // In-band branding changes the entry type, not its configuration body.
        let inband = video_init(InitCodec::HevcInband, FAKE_AVCC);
        let plain = video_init(InitCodec::Hevc, FAKE_AVCC);
        assert!(
            inband.windows(4).any(|w| w == b"hev1"),
            "HevcInband must write hev1"
        );
        assert!(!inband.windows(4).any(|w| w == b"hvc1"), "and not hvc1");
        assert!(
            plain.windows(4).any(|w| w == b"hvc1"),
            "Hevc must write hvc1"
        );
        assert_eq!(entry_body(&inband, b"hev1"), entry_body(&plain, b"hvc1"));
    }

    #[test]
    fn the_av_init_path_picks_the_same_brands_as_the_video_only_path() {
        // Two separate exhaustive matches build these entries
        // (write_video_init_segment and write_av_init_segment); both must
        // choose the same codec brands.
        for (codec, fourcc) in [
            (InitCodec::Avc, b"avc1"),
            (InitCodec::AvcInband, b"avc3"),
            (InitCodec::Hevc, b"hvc1"),
            (InitCodec::HevcInband, b"hev1"),
        ] {
            let mut av = Vec::new();
            write_av_init_segment(
                &mut av,
                VideoTrackParams {
                    width: 320,
                    height: 240,
                    timescale: 90000,
                    configuration_record: FAKE_AVCC.to_vec(),
                    codec,
                },
                None,
            )
            .unwrap();
            assert!(
                av.windows(4).any(|w| w == fourcc),
                "write_av_init_segment must write {} for {codec:?}",
                String::from_utf8_lossy(fourcc)
            );
        }
    }
}
