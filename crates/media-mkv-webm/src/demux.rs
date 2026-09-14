//! Matroska/WebM metadata demuxing.
//!
//! Walk the EBML header, validate DocType, then descend into the Segment
//! to harvest metadata. Cluster and Block walking lives in [`crate::cluster`].

use crate::ebml::element::Reader;
use crate::ebml::schema::{ids, track_type};
use crate::profile::DocType;
use crate::{Error, Result};

/// Demuxed container metadata. Frame data is not parsed here — callers
/// get the byte position where clusters begin (`clusters_offset`) and
/// step in with a separate API.
#[derive(Debug, Clone)]
pub struct Demuxer {
    pub doc_type: DocType,
    pub doc_type_version: u32,
    pub timestamp_scale_ns: u64,
    /// Total duration in segment ticks (multiply by timestamp_scale_ns
    /// for nanoseconds). `None` if the muxer didn't fill it in (live
    /// streams).
    pub duration_ticks: Option<f64>,
    pub title: Option<String>,
    pub muxing_app: Option<String>,
    pub writing_app: Option<String>,
    pub tracks: Vec<TrackInfo>,
    /// Byte offset (relative to the input slice) at which Cluster
    /// elements begin. Callers stream samples from here via
    /// [`crate::cluster::Frames`].
    pub clusters_offset: usize,
    /// One-past-the-end of the Segment in input coordinates. Defaults
    /// to `input.len()` for the unknown-size Segment encoding used by
    /// streaming muxers.
    pub segment_end: usize,
    /// Byte offset of the Segment *payload* (first byte after the
    /// Segment id + size VINT). Combine with a [`CuePoint::cluster_position`]
    /// to seek directly to that cluster in the input buffer.
    pub segment_payload_start: usize,
    /// Cues index, if the muxer wrote one. Empty for live streams or
    /// older muxers that omitted it.
    pub cues: Vec<CuePoint>,
}

/// One Cues entry resolved into nanosecond timestamps. `cluster_position`
/// stays in Segment-payload-relative form so it round-trips losslessly
/// vs. the Matroska spec; add [`Demuxer::segment_payload_start`] to
/// translate into the input buffer.
#[derive(Debug, Clone)]
pub struct CuePoint {
    pub ts_ns: u64,
    pub track_number: u64,
    pub cluster_position: u64,
}

/// One TrackEntry projected into the shape we care about.
#[derive(Debug, Clone)]
pub struct TrackInfo {
    pub number: u64,
    pub uid: u64,
    pub kind: TrackKind,
    pub codec_id: String,
    pub codec_private: Option<Vec<u8>>,
    /// Per-frame duration in nanoseconds (DefaultDuration). `None`
    /// means variable-frame-rate; ts must come from each block.
    pub default_duration_ns: Option<u64>,
    pub codec_delay_ns: u64,
    pub seek_pre_roll_ns: u64,
    pub language: Option<String>,
    pub name: Option<String>,
    pub flag_enabled: bool,
    pub flag_default: bool,
    pub flag_forced: bool,
    pub video: Option<VideoParams>,
    pub audio: Option<AudioParams>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
    Subtitle,
    Other(u8),
}

impl TrackKind {
    fn from_u64(v: u64) -> Self {
        match v {
            track_type::VIDEO => Self::Video,
            track_type::AUDIO => Self::Audio,
            track_type::SUBTITLE => Self::Subtitle,
            other => Self::Other(other as u8),
        }
    }
}

#[derive(Debug, Clone)]
pub struct VideoParams {
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub display_width: Option<u32>,
    pub display_height: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct AudioParams {
    pub sampling_frequency: f64,
    pub output_sampling_frequency: Option<f64>,
    pub channels: u32,
    pub bit_depth: Option<u32>,
}

impl Demuxer {
    /// Walk the EBML header + Segment metadata from a byte slice.
    pub fn parse(input: &[u8]) -> Result<Self> {
        let mut r = Reader::new(input);

        // --- EBML header --------------------------------------------------
        let ebml = r.read_header()?;
        if ebml.id.value != ids::EBML {
            return Err(Error::NotMatroska);
        }
        let mut ebml_inner = r.descend(&ebml)?;
        let (doc_type, doc_type_version) = parse_ebml_header(&mut ebml_inner)?;
        r.skip_payload(&ebml)?;

        // --- Segment ------------------------------------------------------
        // Skip Void / CRC32 between EBML and Segment.
        let segment = loop {
            let hdr = r.read_header()?;
            match hdr.id.value {
                ids::VOID | ids::CRC32 => {
                    r.skip_payload(&hdr)?;
                    continue;
                }
                ids::SEGMENT => break hdr,
                _ => return Err(Error::Malformed("expected Segment after EBML header")),
            }
        };
        // Segment is normally unknown-size — walk children unbounded.
        let segment_payload_start = segment.payload_start;
        let segment_end = match segment.payload_end() {
            Some(e) => e,
            None => input.len(),
        };
        let mut seg = match segment.size {
            Some(_) => r.descend(&segment)?,
            None => r.descend_unbounded(&segment),
        };

        let mut timestamp_scale_ns = 1_000_000u64; // matroska default
        let mut duration_ticks = None;
        let mut title = None;
        let mut muxing_app = None;
        let mut writing_app = None;
        let mut tracks: Vec<TrackInfo> = Vec::new();
        let mut clusters_offset: Option<usize> = None;
        let mut cues: Vec<CuePoint> = Vec::new();

        while !seg.eof() {
            let child = match seg.read_header() {
                Ok(h) => h,
                Err(Error::UnexpectedEof(_)) => break,
                Err(e) => return Err(e),
            };
            match child.id.value {
                ids::INFO => {
                    let mut info = seg.descend(&child)?;
                    parse_info(
                        &mut info,
                        &mut timestamp_scale_ns,
                        &mut duration_ticks,
                        &mut title,
                        &mut muxing_app,
                        &mut writing_app,
                    )?;
                    seg.skip_payload(&child)?;
                }
                ids::TRACKS => {
                    let mut t = seg.descend(&child)?;
                    parse_tracks(&mut t, &mut tracks)?;
                    seg.skip_payload(&child)?;
                }
                ids::CLUSTER => {
                    // First cluster found — record its position and
                    // keep walking so a trailing Cues element after the
                    // clusters still lands in our index. `child.element_start`
                    // is relative to the Segment payload slice; add the
                    // segment payload's absolute offset to get back into
                    // input-buffer coordinates.
                    if clusters_offset.is_none() {
                        clusters_offset = Some(segment_payload_start + child.element_start);
                    }
                    seg.skip_payload(&child)?;
                }
                ids::CUES => {
                    let mut c = seg.descend(&child)?;
                    parse_cues(&mut c, timestamp_scale_ns, &mut cues)?;
                    seg.skip_payload(&child)?;
                }
                ids::SEEK_HEAD
                | ids::ATTACHMENTS
                | ids::CHAPTERS
                | ids::TAGS
                | ids::VOID
                | ids::CRC32 => {
                    // Acknowledged but not parsed by this API.
                    seg.skip_payload(&child)?;
                }
                _ => {
                    // Unknown element — skip so unfamiliar muxers don't
                    // brick the walk.
                    seg.skip_payload(&child)?;
                }
            }
        }

        let doc_type = DocType::parse(&doc_type).ok_or(Error::NotMatroska)?;

        Ok(Self {
            doc_type,
            doc_type_version,
            timestamp_scale_ns,
            duration_ticks,
            title,
            muxing_app,
            writing_app,
            tracks,
            clusters_offset: clusters_offset.unwrap_or(segment_end),
            segment_end,
            segment_payload_start,
            cues,
        })
    }
}

pub(crate) fn parse_cues(
    r: &mut Reader<'_>,
    timestamp_scale_ns: u64,
    cues: &mut Vec<CuePoint>,
) -> Result<()> {
    while !r.eof() {
        let cp = match r.read_header() {
            Ok(h) => h,
            Err(Error::UnexpectedEof(_)) => break,
            Err(e) => return Err(e),
        };
        if cp.id.value == ids::CUE_POINT {
            let mut inner = r.descend(&cp)?;
            parse_cue_point(&mut inner, timestamp_scale_ns, cues)?;
        }
        r.skip_payload(&cp)?;
    }
    Ok(())
}

fn parse_cue_point(
    r: &mut Reader<'_>,
    timestamp_scale_ns: u64,
    cues: &mut Vec<CuePoint>,
) -> Result<()> {
    let mut ts_ticks: u64 = 0;
    let mut positions: Vec<(u64, u64)> = Vec::new();
    while !r.eof() {
        let child = match r.read_header() {
            Ok(h) => h,
            Err(Error::UnexpectedEof(_)) => break,
            Err(e) => return Err(e),
        };
        match child.id.value {
            ids::CUE_TIME => ts_ticks = r.read_uint(&child)?,
            ids::CUE_TRACK_POSITIONS => {
                let mut ctp = r.descend(&child)?;
                let (track, pos) = parse_cue_track_positions(&mut ctp)?;
                positions.push((track, pos));
            }
            _ => {}
        }
        r.skip_payload(&child)?;
    }
    let ts_ns = ts_ticks
        .checked_mul(timestamp_scale_ns)
        .ok_or(Error::SizeOverflow)?;
    for (track_number, cluster_position) in positions {
        cues.push(CuePoint {
            ts_ns,
            track_number,
            cluster_position,
        });
    }
    Ok(())
}

fn parse_cue_track_positions(r: &mut Reader<'_>) -> Result<(u64, u64)> {
    let mut track = 0u64;
    let mut position = 0u64;
    while !r.eof() {
        let child = match r.read_header() {
            Ok(h) => h,
            Err(Error::UnexpectedEof(_)) => break,
            Err(e) => return Err(e),
        };
        match child.id.value {
            ids::CUE_TRACK => track = r.read_uint(&child)?,
            ids::CUE_CLUSTER_POSITION => position = r.read_uint(&child)?,
            _ => {}
        }
        r.skip_payload(&child)?;
    }
    Ok((track, position))
}

pub(crate) fn parse_ebml_header(r: &mut Reader<'_>) -> Result<(String, u32)> {
    let mut doc_type: Option<String> = None;
    let mut doc_type_version = 1u32;
    while !r.eof() {
        let child = match r.read_header() {
            Ok(h) => h,
            Err(Error::UnexpectedEof(_)) => break,
            Err(e) => return Err(e),
        };
        match child.id.value {
            ids::DOC_TYPE => doc_type = Some(r.read_ascii(&child)?.to_string()),
            ids::DOC_TYPE_VERSION => doc_type_version = r.read_uint(&child)? as u32,
            _ => {}
        }
        r.skip_payload(&child)?;
    }
    let doc_type = doc_type.ok_or(Error::NotMatroska)?;
    Ok((doc_type, doc_type_version))
}

pub(crate) fn parse_info(
    r: &mut Reader<'_>,
    timestamp_scale_ns: &mut u64,
    duration_ticks: &mut Option<f64>,
    title: &mut Option<String>,
    muxing_app: &mut Option<String>,
    writing_app: &mut Option<String>,
) -> Result<()> {
    while !r.eof() {
        let child = match r.read_header() {
            Ok(h) => h,
            Err(Error::UnexpectedEof(_)) => break,
            Err(e) => return Err(e),
        };
        match child.id.value {
            ids::TIMESTAMP_SCALE => *timestamp_scale_ns = r.read_uint(&child)?,
            ids::DURATION => *duration_ticks = Some(r.read_float(&child)?),
            ids::TITLE => *title = Some(r.read_utf8(&child)?.to_string()),
            ids::MUXING_APP => *muxing_app = Some(r.read_utf8(&child)?.to_string()),
            ids::WRITING_APP => *writing_app = Some(r.read_utf8(&child)?.to_string()),
            _ => {}
        }
        r.skip_payload(&child)?;
    }
    Ok(())
}

pub(crate) fn parse_tracks(r: &mut Reader<'_>, tracks: &mut Vec<TrackInfo>) -> Result<()> {
    while !r.eof() {
        let entry = match r.read_header() {
            Ok(h) => h,
            Err(Error::UnexpectedEof(_)) => break,
            Err(e) => return Err(e),
        };
        if entry.id.value == ids::TRACK_ENTRY {
            let mut t = r.descend(&entry)?;
            tracks.push(parse_track_entry(&mut t)?);
        }
        r.skip_payload(&entry)?;
    }
    Ok(())
}

fn parse_track_entry(r: &mut Reader<'_>) -> Result<TrackInfo> {
    let mut number = 0u64;
    let mut uid = 0u64;
    let mut kind = TrackKind::Other(0);
    let mut codec_id_str = String::new();
    let mut codec_private = None;
    let mut default_duration_ns = None;
    let mut codec_delay_ns = 0u64;
    let mut seek_pre_roll_ns = 0u64;
    let mut language = None;
    let mut language_ietf = None;
    let mut name = None;
    let mut flag_enabled = true;
    let mut flag_default = true;
    let mut flag_forced = false;
    let mut video = None;
    let mut audio = None;

    while !r.eof() {
        let child = match r.read_header() {
            Ok(h) => h,
            Err(Error::UnexpectedEof(_)) => break,
            Err(e) => return Err(e),
        };
        match child.id.value {
            ids::TRACK_NUMBER => number = r.read_uint(&child)?,
            ids::TRACK_UID => uid = r.read_uint(&child)?,
            ids::TRACK_TYPE => kind = TrackKind::from_u64(r.read_uint(&child)?),
            ids::CODEC_ID => codec_id_str = r.read_ascii(&child)?.to_string(),
            ids::CODEC_PRIVATE => codec_private = Some(r.payload(&child)?.to_vec()),
            ids::DEFAULT_DURATION => default_duration_ns = Some(r.read_uint(&child)?),
            ids::CODEC_DELAY => codec_delay_ns = r.read_uint(&child)?,
            ids::SEEK_PRE_ROLL => seek_pre_roll_ns = r.read_uint(&child)?,
            ids::LANGUAGE => language = Some(r.read_ascii(&child)?.to_string()),
            ids::LANGUAGE_IETF => {
                // IETF supersedes the 3-letter language regardless of element order.
                language_ietf = Some(r.read_ascii(&child)?.to_string());
            }
            ids::NAME => name = Some(r.read_utf8(&child)?.to_string()),
            ids::FLAG_ENABLED => flag_enabled = r.read_uint(&child)? != 0,
            ids::FLAG_DEFAULT => flag_default = r.read_uint(&child)? != 0,
            ids::FLAG_FORCED => flag_forced = r.read_uint(&child)? != 0,
            ids::VIDEO => {
                let mut v = r.descend(&child)?;
                video = Some(parse_video(&mut v)?);
            }
            ids::AUDIO => {
                let mut a = r.descend(&child)?;
                audio = Some(parse_audio(&mut a)?);
            }
            _ => {}
        }
        r.skip_payload(&child)?;
    }

    // Snap mistakes early: video tracks should carry codec ID prefix "V_",
    // audio "A_", subtitles "S_". Use it to catch malformed track entries
    // without depending on the whitelist (still permit unknown codecs).
    let prefix_ok = match kind {
        TrackKind::Video => codec_id_str.starts_with("V_"),
        TrackKind::Audio => codec_id_str.starts_with("A_"),
        TrackKind::Subtitle => codec_id_str.starts_with("S_"),
        _ => true,
    };
    if !prefix_ok {
        return Err(Error::Malformed("CodecID prefix doesn't match TrackType"));
    }

    Ok(TrackInfo {
        number,
        uid,
        kind,
        codec_id: codec_id_str,
        codec_private,
        default_duration_ns,
        codec_delay_ns,
        seek_pre_roll_ns,
        language: language_ietf.or(language),
        name,
        flag_enabled,
        flag_default,
        flag_forced,
        video,
        audio,
    })
}

fn parse_video(r: &mut Reader<'_>) -> Result<VideoParams> {
    let mut pixel_width = 0u32;
    let mut pixel_height = 0u32;
    let mut display_width = None;
    let mut display_height = None;
    while !r.eof() {
        let child = match r.read_header() {
            Ok(h) => h,
            Err(Error::UnexpectedEof(_)) => break,
            Err(e) => return Err(e),
        };
        match child.id.value {
            ids::PIXEL_WIDTH => pixel_width = r.read_uint(&child)? as u32,
            ids::PIXEL_HEIGHT => pixel_height = r.read_uint(&child)? as u32,
            ids::DISPLAY_WIDTH => display_width = Some(r.read_uint(&child)? as u32),
            ids::DISPLAY_HEIGHT => display_height = Some(r.read_uint(&child)? as u32),
            _ => {}
        }
        r.skip_payload(&child)?;
    }
    Ok(VideoParams {
        pixel_width,
        pixel_height,
        display_width,
        display_height,
    })
}

fn parse_audio(r: &mut Reader<'_>) -> Result<AudioParams> {
    let mut sampling_frequency = 8000.0;
    let mut output_sampling_frequency = None;
    let mut channels = 1u32;
    let mut bit_depth = None;
    while !r.eof() {
        let child = match r.read_header() {
            Ok(h) => h,
            Err(Error::UnexpectedEof(_)) => break,
            Err(e) => return Err(e),
        };
        match child.id.value {
            ids::SAMPLING_FREQUENCY => sampling_frequency = r.read_float(&child)?,
            ids::OUTPUT_SAMPLING_FREQUENCY => {
                output_sampling_frequency = Some(r.read_float(&child)?)
            }
            ids::CHANNELS => channels = r.read_uint(&child)? as u32,
            ids::BIT_DEPTH => bit_depth = Some(r.read_uint(&child)? as u32),
            _ => {}
        }
        r.skip_payload(&child)?;
    }
    Ok(AudioParams {
        sampling_frequency,
        output_sampling_frequency,
        channels,
        bit_depth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 1-byte EBML element: id(1B) + size(1B varint) + payload.
    fn elem1(id: u8, payload: &[u8]) -> Vec<u8> {
        assert!(payload.len() < 0x80);
        let mut v = vec![id, 0x80 | payload.len() as u8];
        v.extend_from_slice(payload);
        v
    }

    /// Build a master element with a 4-byte ID and a 1-byte known size.
    fn elem_id4(id: u32, payload: &[u8]) -> Vec<u8> {
        assert!(payload.len() < 0x80);
        let mut v = id.to_be_bytes().to_vec();
        v.push(0x80 | payload.len() as u8);
        v.extend_from_slice(payload);
        v
    }

    /// Build a 2-byte-ID element.
    fn elem_id2(id: u16, payload: &[u8]) -> Vec<u8> {
        assert!(payload.len() < 0x80);
        let mut v = id.to_be_bytes().to_vec();
        v.push(0x80 | payload.len() as u8);
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn parses_minimal_webm_doctype() {
        // EBML header containing DocType = "webm".
        let doc_type = elem_id2(0x4282, b"webm");
        let ebml = elem_id4(0x1A45DFA3, &doc_type);

        // Segment > Info { TimestampScale = 1_000_000 } + Tracks {} (empty).
        let ts_scale = {
            let mut v = vec![0x2A, 0xD7, 0xB1]; // TIMESTAMP_SCALE (3-byte VINT id)
            v.push(0x83); // size 3
            v.extend_from_slice(&[0x0F, 0x42, 0x40]); // 1_000_000
            v
        };
        let info = elem_id4(0x1549A966, &ts_scale);
        let tracks = elem_id4(0x1654AE6B, &[]);
        let mut segment_payload = Vec::new();
        segment_payload.extend_from_slice(&info);
        segment_payload.extend_from_slice(&tracks);
        let segment = elem_id4(0x18538067, &segment_payload);

        let mut file = Vec::new();
        file.extend_from_slice(&ebml);
        file.extend_from_slice(&segment);

        let d = Demuxer::parse(&file).unwrap();
        assert_eq!(d.doc_type, DocType::Webm);
        assert_eq!(d.timestamp_scale_ns, 1_000_000);
        assert!(d.tracks.is_empty());
    }

    #[test]
    fn parses_audio_track() {
        let doc_type = elem_id2(0x4282, b"matroska");
        let ebml = elem_id4(0x1A45DFA3, &doc_type);

        // TrackEntry: number=1, type=2(audio), codec=A_OPUS, audio{ sf=48000, channels=2 }
        let track_number = elem1(0xD7, &[0x01]);
        let track_type = elem1(0x83, &[0x02]);
        let codec_id = elem1(0x86, b"A_OPUS");
        let sf_bytes = 48000.0f32.to_be_bytes();
        let sf = elem1(0xB5, &sf_bytes);
        let channels = elem1(0x9F, &[0x02]);
        let mut audio_payload = Vec::new();
        audio_payload.extend_from_slice(&sf);
        audio_payload.extend_from_slice(&channels);
        let audio = elem1(0xE1, &audio_payload);

        let mut track_entry_payload = Vec::new();
        track_entry_payload.extend_from_slice(&track_number);
        track_entry_payload.extend_from_slice(&track_type);
        track_entry_payload.extend_from_slice(&codec_id);
        track_entry_payload.extend_from_slice(&audio);
        let track_entry = elem1(0xAE, &track_entry_payload);

        let tracks = elem_id4(0x1654AE6B, &track_entry);
        let segment = elem_id4(0x18538067, &tracks);

        let mut file = Vec::new();
        file.extend_from_slice(&ebml);
        file.extend_from_slice(&segment);

        let d = Demuxer::parse(&file).unwrap();
        assert_eq!(d.doc_type, DocType::Matroska);
        assert_eq!(d.tracks.len(), 1);
        let t = &d.tracks[0];
        assert_eq!(t.number, 1);
        assert_eq!(t.kind, TrackKind::Audio);
        assert_eq!(t.codec_id, "A_OPUS");
        let a = t.audio.as_ref().unwrap();
        assert_eq!(a.sampling_frequency as u32, 48000);
        assert_eq!(a.channels, 2);
    }
}
