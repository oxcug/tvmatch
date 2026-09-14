//! DocType-gated codec policy.
//!
//! WebM is structurally identical to Matroska — the only difference at
//! parse time is which codec IDs the container is allowed to carry.
//! `.webm` files MUST restrict to royalty-free codecs (VP8/VP9/AV1 +
//! Opus/Vorbis); `.mkv` accepts the full Matroska codec set.
//!
//! Per `https://www.webmproject.org/docs/container/`: anything outside
//! the WebM whitelist in a `webm` DocType is a spec violation, and we
//! surface it as `Unsupported` rather than attempting decode.

use crate::ebml::schema::codec_id;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocType {
    Matroska,
    Webm,
}

impl DocType {
    /// Parse the DocType string carried in the EBML header.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "matroska" => Some(Self::Matroska),
            "webm" => Some(Self::Webm),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Matroska => "matroska",
            Self::Webm => "webm",
        }
    }
}

/// Whether a `CodecID` string is allowed for the given container
/// profile. Unknown codec IDs return `false` — callers report
/// `Unsupported { what: codec_id }` so users can see what was rejected.
pub fn is_codec_allowed(doc_type: DocType, codec: &str) -> bool {
    match doc_type {
        DocType::Webm => matches!(
            codec,
            codec_id::V_AV1
                | codec_id::V_VP8
                | codec_id::V_VP9
                | codec_id::A_OPUS
                | codec_id::A_VORBIS
        ),
        DocType::Matroska => matches!(
            codec,
            codec_id::V_MPEG4_ISO_AVC
                | codec_id::V_MPEGH_ISO_HEVC
                | codec_id::V_AV1
                | codec_id::V_VP8
                | codec_id::V_VP9
                | codec_id::A_AAC
                | codec_id::A_OPUS
                | codec_id::A_VORBIS
                | codec_id::A_FLAC
                | codec_id::A_PCM_INT_LE
                | codec_id::A_PCM_INT_BE
                | codec_id::A_PCM_FLOAT_LE
                | codec_id::S_TEXT_UTF8
                | codec_id::S_TEXT_ASS
                | codec_id::S_TEXT_SSA
                | codec_id::S_TEXT_WEBVTT
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webm_rejects_h264_and_aac() {
        assert!(!is_codec_allowed(DocType::Webm, codec_id::V_MPEG4_ISO_AVC));
        assert!(!is_codec_allowed(DocType::Webm, codec_id::A_AAC));
    }

    #[test]
    fn webm_accepts_vp9_opus() {
        assert!(is_codec_allowed(DocType::Webm, codec_id::V_VP9));
        assert!(is_codec_allowed(DocType::Webm, codec_id::A_OPUS));
    }

    #[test]
    fn mkv_accepts_h264_and_subtitles() {
        assert!(is_codec_allowed(
            DocType::Matroska,
            codec_id::V_MPEG4_ISO_AVC
        ));
        assert!(is_codec_allowed(DocType::Matroska, codec_id::S_TEXT_UTF8));
    }

    #[test]
    fn unknown_codec_rejected_both_profiles() {
        assert!(!is_codec_allowed(DocType::Matroska, "V_THEORA"));
        assert!(!is_codec_allowed(DocType::Webm, "A_AC3"));
    }
}
