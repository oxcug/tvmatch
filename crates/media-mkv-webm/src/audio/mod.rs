//! Audio decode dispatch.
//!
//! [`AudioDecoder`] is the seam between the container layer and any
//! particular codec backend. Today symphonia handles Vorbis/AAC/FLAC/
//! PCM; Opus (and anything else) plugs in by adding a second impl and
//! extending [`open_audio_decoder`] — the rest of the crate, and any
//! downstream consumer, never sees the backend type.
//!
//! The trait deliberately keeps no symphonia (or any other backend)
//! types in its signature, so `symphonia_decoder` is the only file
//! in the crate that imports the symphonia crate.

use crate::cluster::Frame;
use crate::demux::{TrackInfo, TrackKind};
use crate::ebml::schema::codec_id;
use crate::{Error, Result};

#[cfg(feature = "audio-symphonia")]
mod symphonia_decoder;

/// Decoded interleaved PCM trait. Implementations append samples to a
/// caller-provided buffer so per-frame allocations stay out of the
/// hot path.
pub trait AudioDecoder: Send {
    /// Native output sample rate in Hz.
    fn sample_rate(&self) -> u32;
    /// Channel count of the decoded stream.
    fn channels(&self) -> u32;
    /// Decode one container [`Frame`] and append interleaved f32
    /// samples to `out`. Returns the number of samples-per-channel
    /// written (so callers can compute duration if needed).
    fn decode(&mut self, frame: &Frame, out: &mut Vec<f32>) -> Result<usize>;
}

/// Open the right decoder backend for `track`.
///
/// Returns [`Error::Unsupported`] if `track` is not an audio track or
/// if its `CodecID` is not supported by a backend (including Opus).
pub fn open_audio_decoder(track: &TrackInfo) -> Result<Box<dyn AudioDecoder>> {
    if track.kind != TrackKind::Audio {
        return Err(Error::Unsupported {
            what: "non-audio track passed to open_audio_decoder",
        });
    }
    match track.codec_id.as_str() {
        codec_id::A_VORBIS
        | codec_id::A_AAC
        | codec_id::A_FLAC
        | codec_id::A_PCM_INT_LE
        | codec_id::A_PCM_INT_BE
        | codec_id::A_PCM_FLOAT_LE => {
            #[cfg(feature = "audio-symphonia")]
            {
                symphonia_decoder::open(track).map(|d| Box::new(d) as Box<dyn AudioDecoder>)
            }
            #[cfg(not(feature = "audio-symphonia"))]
            {
                Err(Error::Unsupported {
                    what: "audio-symphonia feature disabled",
                })
            }
        }
        codec_id::A_OPUS => Err(Error::Unsupported {
            what: "Opus decode",
        }),
        _ => Err(Error::Unsupported {
            what: "audio codec",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demux::{TrackInfo, TrackKind};

    fn track(codec_id: &str, kind: TrackKind) -> TrackInfo {
        TrackInfo {
            number: 1,
            uid: 1,
            kind,
            codec_id: codec_id.to_string(),
            codec_private: None,
            default_duration_ns: None,
            codec_delay_ns: 0,
            seek_pre_roll_ns: 0,
            language: None,
            name: None,
            flag_enabled: true,
            flag_default: true,
            flag_forced: false,
            video: None,
            audio: Some(crate::demux::AudioParams {
                sampling_frequency: 48000.0,
                output_sampling_frequency: None,
                channels: 2,
                bit_depth: Some(16),
            }),
        }
    }

    #[test]
    fn rejects_non_audio_track() {
        let t = track(codec_id::V_VP9, TrackKind::Video);
        assert!(matches!(
            open_audio_decoder(&t),
            Err(Error::Unsupported { .. })
        ));
    }

    #[test]
    fn rejects_unknown_codec() {
        let t = track("A_MADE_UP", TrackKind::Audio);
        assert!(matches!(
            open_audio_decoder(&t),
            Err(Error::Unsupported { .. })
        ));
    }

    #[test]
    fn opus_dispatch_is_unsupported() {
        let t = track(codec_id::A_OPUS, TrackKind::Audio);
        match open_audio_decoder(&t) {
            Err(Error::Unsupported { what }) => assert!(what.contains("Opus"), "got {what}"),
            Ok(_) => panic!("expected Unsupported(Opus), got Ok"),
            Err(e) => panic!("expected Unsupported(Opus), got Err({e})"),
        }
    }
}
