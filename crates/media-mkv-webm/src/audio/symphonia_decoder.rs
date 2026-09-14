//! Symphonia adapter — the ONLY place symphonia types appear in this
//! crate. Anything outside `audio` consumes the [`AudioDecoder`] trait.
//!
//! Symphonia 0.5.x provides the supported audio backends. It has no Opus
//! decoder; Opus packets remain unsupported by this dispatch.

use symphonia::core::audio::{Channels, SampleBuffer};
use symphonia::core::codecs::{
    CODEC_TYPE_AAC, CODEC_TYPE_FLAC, CODEC_TYPE_PCM_F32LE, CODEC_TYPE_PCM_S16BE,
    CODEC_TYPE_PCM_S16LE, CODEC_TYPE_PCM_S24BE, CODEC_TYPE_PCM_S24LE, CODEC_TYPE_PCM_S32BE,
    CODEC_TYPE_PCM_S32LE, CODEC_TYPE_VORBIS, CodecParameters, CodecType, Decoder, DecoderOptions,
};
use symphonia::core::formats::Packet;
use symphonia::default::get_codecs;

use crate::audio::AudioDecoder;
use crate::cluster::Frame;
use crate::demux::TrackInfo;
use crate::ebml::schema::codec_id;
use crate::{Error, Result};

/// Pick the symphonia [`CodecType`] for a Matroska CodecID + bit depth.
///
/// PCM dispatch uses BitDepth from the audio sub-element to choose
/// between S16/S24/S32/F32 LE/BE variants. Bit depth ≤ 16 maps to
/// S16, ≤ 24 → S24, ≤ 32 → S32 (or F32 for the `A_PCM/FLOAT/IEEE`
/// branch).
fn map_codec_type(id: &str, bit_depth: Option<u32>) -> Option<CodecType> {
    match id {
        codec_id::A_VORBIS => Some(CODEC_TYPE_VORBIS),
        codec_id::A_AAC => Some(CODEC_TYPE_AAC),
        codec_id::A_FLAC => Some(CODEC_TYPE_FLAC),
        codec_id::A_PCM_INT_LE => Some(match bit_depth.unwrap_or(16) {
            0..=16 => CODEC_TYPE_PCM_S16LE,
            17..=24 => CODEC_TYPE_PCM_S24LE,
            _ => CODEC_TYPE_PCM_S32LE,
        }),
        codec_id::A_PCM_INT_BE => Some(match bit_depth.unwrap_or(16) {
            0..=16 => CODEC_TYPE_PCM_S16BE,
            17..=24 => CODEC_TYPE_PCM_S24BE,
            _ => CODEC_TYPE_PCM_S32BE,
        }),
        codec_id::A_PCM_FLOAT_LE => Some(match bit_depth.unwrap_or(32) {
            0..=32 => CODEC_TYPE_PCM_F32LE,
            _ => CODEC_TYPE_PCM_F32LE, // F64 lands here too; symphonia 0.5 has no Matroska binding for F64.
        }),
        // Big-endian IEEE float exists in symphonia but Matroska doesn't define a CodecID for it.
        _ => None,
    }
}

/// Build a [`Channels`] bitflag with the N lowest "standard" speaker
/// bits set. Matroska doesn't carry an explicit speaker mapping for
/// most codecs, so we approximate: 1 → mono (FRONT_CENTRE), 2 →
/// stereo, ≥3 → first N flags (good enough to let symphonia decode;
/// downstream resampling is the place to do real layout mapping).
fn channels_for(n: u32) -> Channels {
    match n {
        0 | 1 => Channels::FRONT_CENTRE,
        2 => Channels::FRONT_LEFT | Channels::FRONT_RIGHT,
        _ => {
            let n = n.min(32);
            Channels::from_bits_truncate((1u32 << n) - 1)
        }
    }
}

pub(super) struct SymphoniaAudio {
    decoder: Box<dyn Decoder>,
    sample_rate: u32,
    channels: u32,
    /// Re-used SampleBuffer to avoid allocating per packet. Lazily
    /// initialised on the first decode because we don't know the
    /// frame `Duration` (samples-per-channel-per-packet) until we
    /// see the first decoded AudioBufferRef.
    sbuf: Option<SampleBuffer<f32>>,
}

pub(super) fn open(track: &TrackInfo) -> Result<SymphoniaAudio> {
    let audio = track
        .audio
        .as_ref()
        .ok_or(Error::Malformed("audio track without Audio sub-element"))?;
    let codec_type =
        map_codec_type(&track.codec_id, audio.bit_depth).ok_or(Error::Unsupported {
            what: "audio codec → symphonia mapping",
        })?;

    let sample_rate = audio
        .output_sampling_frequency
        .unwrap_or(audio.sampling_frequency)
        .round() as u32;
    let channels = audio.channels.max(1);

    let mut params = CodecParameters::new();
    params.for_codec(codec_type);
    params.sample_rate = Some(sample_rate);
    params.channels = Some(channels_for(channels));
    if let Some(bd) = audio.bit_depth {
        params.bits_per_sample = Some(bd);
    }
    if let Some(cp) = &track.codec_private {
        params.extra_data = Some(cp.clone().into_boxed_slice());
    }

    let decoder = get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(|e| Error::Malformed(symphonia_err_static(e)))?;

    Ok(SymphoniaAudio {
        decoder,
        sample_rate,
        channels,
        sbuf: None,
    })
}

impl AudioDecoder for SymphoniaAudio {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn channels(&self) -> u32 {
        self.channels
    }
    fn decode(&mut self, frame: &Frame, out: &mut Vec<f32>) -> Result<usize> {
        // Packet ts/dur are bookkeeping for symphonia's seek path; we
        // don't drive seeking through symphonia, so 0 is fine. The
        // track_id passed to the Packet is opaque to the decoder.
        let packet = Packet::new_from_slice(frame.track as u32, 0, 0, frame.data);
        let buf = self
            .decoder
            .decode(&packet)
            .map_err(|e| Error::Malformed(symphonia_err_static(e)))?;

        let spec = *buf.spec();
        let frames = buf.frames() as u64;
        if frames == 0 {
            return Ok(0);
        }

        // Initialise / resize the reusable sample buffer.
        let need_new = match &self.sbuf {
            None => true,
            Some(b) => (b.capacity() as u64) < frames,
        };
        if need_new {
            self.sbuf = Some(SampleBuffer::<f32>::new(frames, spec));
        }
        let sbuf = self.sbuf.as_mut().unwrap();
        sbuf.copy_interleaved_ref(buf);
        out.extend_from_slice(sbuf.samples());
        Ok(frames as usize)
    }
}

/// Symphonia errors carry a `&'static str` description; this strips
/// the dynamic info and projects it through our hand-rolled
/// `Error::Malformed(&'static str)` slot. Good enough for now —
/// callers see the symphonia category and can grep upstream for the
/// exact diagnostic.
fn symphonia_err_static(e: symphonia::core::errors::Error) -> &'static str {
    use symphonia::core::errors::Error as SE;
    match e {
        SE::IoError(_) => "symphonia: io",
        SE::DecodeError(_) => "symphonia: decode",
        SE::SeekError(_) => "symphonia: seek",
        SE::Unsupported(_) => "symphonia: unsupported",
        SE::LimitError(_) => "symphonia: limit",
        SE::ResetRequired => "symphonia: reset required",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_bit_depth_dispatch() {
        assert_eq!(
            map_codec_type("A_PCM/INT/LIT", Some(16)),
            Some(CODEC_TYPE_PCM_S16LE)
        );
        assert_eq!(
            map_codec_type("A_PCM/INT/LIT", Some(24)),
            Some(CODEC_TYPE_PCM_S24LE)
        );
        assert_eq!(
            map_codec_type("A_PCM/INT/LIT", Some(32)),
            Some(CODEC_TYPE_PCM_S32LE)
        );
        assert_eq!(
            map_codec_type("A_PCM/INT/BIG", Some(16)),
            Some(CODEC_TYPE_PCM_S16BE)
        );
        assert_eq!(
            map_codec_type("A_PCM/FLOAT/IEEE", Some(32)),
            Some(CODEC_TYPE_PCM_F32LE)
        );
        assert_eq!(map_codec_type("A_OPUS", None), None);
    }

    #[test]
    fn channels_for_layouts() {
        assert_eq!(channels_for(0), Channels::FRONT_CENTRE);
        assert_eq!(channels_for(1), Channels::FRONT_CENTRE);
        assert_eq!(
            channels_for(2),
            Channels::FRONT_LEFT | Channels::FRONT_RIGHT
        );
        assert_eq!(channels_for(6).bits().count_ones(), 6);
    }
}
