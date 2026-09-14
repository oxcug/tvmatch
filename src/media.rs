//! Optional native MKV/MP4 subtitle evidence, NOT audio-verified speech or identity.
//! The bounded subsets are documented in docs/MKV.md and docs/MP4.md.
#[cfg(feature = "media")]
mod mp4;
#[cfg(feature = "ocr")]
pub mod ocr;
#[cfg(feature = "media")]
pub mod pgs;
use crate::srt::Transcript;
#[cfg(feature = "media")]
use crate::srt::{Cue, MAX_CUE_BYTES, MAX_CUES, MAX_SRT_BYTES, MAX_TIMESTAMP_MS};
use std::{
    error::Error,
    fmt,
    io::{Read, Seek},
};

pub const MAX_TRACKS: usize = 64;

#[derive(Debug)]
pub enum MediaError {
    Unavailable,
    Container(String),
    #[cfg(feature = "media")]
    Pgs(media_mkv_webm::pgs::PgsError),
    LimitExceeded(&'static str),
    NoSubtitleTracks,
    AmbiguousSubtitleTracks(Vec<u64>),
    TrackNotFound(u64),
    UnsupportedTrack(u64),
    InvalidTrackMetadata,
    NoCaptions,
    InvalidUtf8,
    InvalidTimestamp,
    Transcript(crate::srt::ParseError),
}
impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => write!(
                f,
                "native container support unavailable; rebuild with --features media"
            ),
            other => write!(f, "media: {other:?}"),
        }
    }
}
impl Error for MediaError {}

/// Container declarations only; every field is UNVERIFIED and may be mismuxed.
#[derive(Debug, Clone)]
pub struct SubtitleTrack {
    pub number: u64,
    pub uid: u64,
    pub codec_id: String,
    pub language: Option<String>,
    pub name: Option<String>,
    pub enabled: bool,
    pub default: bool,
    pub forced: bool,
    pub default_duration_ns: Option<u64>,
    /// Direct text extraction/matching support, not bitmap decoding support.
    pub supported: bool,
}

#[derive(Debug, Clone)]
pub struct SubtitleTimestamp {
    pub start_ns: u64,
    /// MKV raw BlockDuration, or MP4 sample duration floored to ns. No duration inferred.
    pub block_duration_ns: Option<u64>,
    /// MKV declared end, or MP4 movie-timeline end after declared edit clipping.
    pub declared_end_ns: Option<u64>,
    /// Transcript end is start_ms + 1 solely to satisfy its positive-duration invariant
    /// if duration is absent, zero, or rounds below 1 ms. Matcher uses starts only.
    pub transcript_end_synthetic: bool,
}

#[derive(Debug, Clone)]
pub struct EmbeddedSubtitles {
    pub track: SubtitleTrack,
    pub transcript: Transcript,
    /// One-to-one with transcript cues. Starts in transcript are floored to milliseconds.
    pub timestamps: Vec<SubtitleTimestamp>,
}

/// Metadata-only probe. Does NOT validate the subsequent media walk/payloads.
pub fn probe_subtitle_tracks<R: Read + Seek>(reader: R) -> Result<Vec<SubtitleTrack>, MediaError> {
    #[cfg(feature = "media")]
    {
        let (reader, mkv) = container_reader(reader)?;
        if mkv {
            enabled::probe(reader)
        } else {
            mp4::probe(reader)
        }
    }
    #[cfg(not(feature = "media"))]
    {
        let _ = reader;
        Err(MediaError::Unavailable)
    }
}

/// Reads to the end of the bounded strict stream before returning any evidence.
/// Selection counts all subtitle tracks, not only supported/default/enabled ones.
pub fn extract_subtitles<R: Read + Seek>(
    reader: R,
    track_number: Option<u64>,
) -> Result<EmbeddedSubtitles, MediaError> {
    #[cfg(feature = "media")]
    {
        let (reader, mkv) = container_reader(reader)?;
        if mkv {
            enabled::extract(reader, track_number)
        } else {
            mp4::extract(reader, track_number)
        }
    }
    #[cfg(not(feature = "media"))]
    {
        let _ = (reader, track_number);
        Err(MediaError::Unavailable)
    }
}

#[cfg(feature = "media")]
fn container_reader<R: Read + Seek>(mut reader: R) -> Result<(R, bool), MediaError> {
    use std::io::SeekFrom;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| MediaError::Container("container seek failed".into()))?;
    let mut magic = [0; 4];
    reader
        .read_exact(&mut magic)
        .map_err(|_| MediaError::Container("short container header".into()))?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| MediaError::Container("container rewind failed".into()))?;
    Ok((reader, magic == [0x1a, 0x45, 0xdf, 0xa3]))
}
#[cfg(feature = "media")]
mod enabled {
    use super::*;
    use media_mkv_webm::{
        TrackKind,
        streaming::{StreamingDemuxer, StreamingLimits, open_streaming_with_limits},
    };
    use std::io::{self, Write};

    fn container(error: media_mkv_webm::Error) -> MediaError {
        MediaError::Container(error.to_string())
    }
    fn open<R: Read + Seek>(reader: R) -> Result<StreamingDemuxer<R>, MediaError> {
        open_streaming_with_limits(
            reader,
            StreamingLimits {
                skip_cues: true,
                ..Default::default()
            },
        )
        .map_err(container)
    }
    pub(super) fn tracks<R: Read + Seek>(
        stream: &StreamingDemuxer<R>,
    ) -> Result<Vec<SubtitleTrack>, MediaError> {
        if stream.demuxer.doc_type != media_mkv_webm::DocType::Matroska {
            return Err(MediaError::Container(
                "only Matroska DocType supported, not WebM/MP4".into(),
            ));
        }
        if stream.demuxer.tracks.len() > MAX_TRACKS {
            return Err(MediaError::LimitExceeded("tracks"));
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for t in &stream.demuxer.tracks {
            if t.number == 0
                || !seen.insert(t.number)
                || t.codec_id.len() > 1024
                || t.name.as_ref().is_some_and(|s| s.len() > 1024)
                || t.language.as_ref().is_some_and(|s| s.len() > 1024)
            {
                return Err(MediaError::InvalidTrackMetadata);
            }
            if t.kind == TrackKind::Subtitle {
                out.push(SubtitleTrack {
                    number: t.number,
                    uid: t.uid,
                    codec_id: t.codec_id.clone(),
                    language: t.language.clone(),
                    name: t.name.clone(),
                    enabled: t.flag_enabled,
                    default: t.flag_default,
                    forced: t.flag_forced,
                    default_duration_ns: t.default_duration_ns,
                    supported: t.codec_id == "S_TEXT/UTF8"
                        && t.codec_delay_ns == 0
                        && t.seek_pre_roll_ns == 0
                        && t.codec_private.as_ref().is_none_or(Vec::is_empty)
                        && t.audio.is_none()
                        && t.video.is_none(),
                });
            }
        }
        Ok(out)
    }
    pub(super) fn probe<R: Read + Seek>(reader: R) -> Result<Vec<SubtitleTrack>, MediaError> {
        tracks(&open(reader)?)
    }
    struct Packet {
        bytes: Vec<u8>,
        overflow: bool,
    }
    impl Write for Packet {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > MAX_CUE_BYTES - self.bytes.len() {
                self.overflow = true;
                return Err(io::Error::other(
                    "selected subtitle packet byte limit exceeded",
                ));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    pub(super) fn extract<R: Read + Seek>(
        reader: R,
        selector: Option<u64>,
    ) -> Result<EmbeddedSubtitles, MediaError> {
        let mut stream = open(reader)?;
        let candidates = tracks(&stream)?;
        let track = if let Some(number) = selector {
            candidates
                .into_iter()
                .find(|t| t.number == number)
                .ok_or_else(|| {
                    if stream.demuxer.tracks.iter().any(|t| t.number == number) {
                        MediaError::UnsupportedTrack(number)
                    } else {
                        MediaError::TrackNotFound(number)
                    }
                })?
        } else {
            match candidates.len() {
                0 => return Err(MediaError::NoSubtitleTracks),
                1 => candidates.into_iter().next().unwrap(),
                _ => {
                    return Err(MediaError::AmbiguousSubtitleTracks(
                        candidates.iter().map(|t| t.number).collect(),
                    ));
                }
            }
        };
        if !track.supported {
            return Err(MediaError::UnsupportedTrack(track.number));
        }
        stream.set_track_filter(Some(vec![track.number]));
        let mut cues = Vec::new();
        let mut timestamps: Vec<SubtitleTimestamp> = Vec::new();
        let mut total = 0usize;
        loop {
            let mut packet = Packet {
                bytes: Vec::new(),
                overflow: false,
            };
            let result = stream.next_frame_into(&mut packet);
            if packet.overflow {
                return Err(MediaError::LimitExceeded("selected packet bytes"));
            }
            let Some(h) = result.map_err(container)? else {
                break;
            };
            if cues.len() == MAX_CUES {
                return Err(MediaError::LimitExceeded("selected packets/cues"));
            }
            total += packet.bytes.len();
            if total > MAX_SRT_BYTES {
                return Err(MediaError::LimitExceeded("subtitle text bytes"));
            }
            let text = String::from_utf8(packet.bytes)
                .map_err(|_| MediaError::InvalidUtf8)?
                .replace("\r\n", "\n");
            // Subtitle order must hold at source precision, not merely after
            // rounding; unlike video PTS, selected caption starts may not regress.
            if timestamps
                .last()
                .is_some_and(|previous| h.timestamp_ns < previous.start_ns)
            {
                return Err(MediaError::InvalidTimestamp);
            }
            let start_ms = h.timestamp_ns / 1_000_000;
            if start_ms >= MAX_TIMESTAMP_MS {
                return Err(MediaError::InvalidTimestamp);
            }
            let duration = h.block_duration_ns.or(track.default_duration_ns);
            let declared_end_ns = duration
                .map(|d| {
                    h.timestamp_ns
                        .checked_add(d)
                        .ok_or(MediaError::InvalidTimestamp)
                })
                .transpose()?;
            let end_ms = declared_end_ns.map(|n| n / 1_000_000);
            if end_ms.is_some_and(|n| n > MAX_TIMESTAMP_MS) {
                return Err(MediaError::InvalidTimestamp);
            }
            let synthetic = end_ms.is_none_or(|end| end <= start_ms);
            cues.push(Cue {
                start_ms,
                end_ms: if synthetic {
                    start_ms + 1
                } else {
                    end_ms.unwrap()
                },
                text,
            });
            timestamps.push(SubtitleTimestamp {
                start_ns: h.timestamp_ns,
                block_duration_ns: h.block_duration_ns,
                declared_end_ns,
                transcript_end_synthetic: synthetic,
            });
        }
        if cues.is_empty() {
            return Err(MediaError::NoCaptions);
        }
        let transcript = Transcript::from_cues(cues).map_err(MediaError::Transcript)?;
        Ok(EmbeddedSubtitles {
            track,
            transcript,
            timestamps,
        })
    }
}
