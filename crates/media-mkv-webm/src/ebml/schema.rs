//! Matroska / WebM element IDs.
//!
//! IDs are quoted as the on-wire encoding (VINT representation with
//! the width marker retained) — exactly what `ebml::varint::read_vint_id`
//! returns in `Vint::value`. Sourced from
//! `https://www.matroska.org/technical/elements.html`.

/// Element IDs. Grouped by their parent in the spec for navigability.
pub mod ids {
    // === EBML header (top level) ============================================
    pub const EBML: u64 = 0x1A45DFA3;
    pub const EBML_VERSION: u64 = 0x4286;
    pub const EBML_READ_VERSION: u64 = 0x42F7;
    pub const EBML_MAX_ID_LENGTH: u64 = 0x42F2;
    pub const EBML_MAX_SIZE_LENGTH: u64 = 0x42F3;
    pub const DOC_TYPE: u64 = 0x4282;
    pub const DOC_TYPE_VERSION: u64 = 0x4287;
    pub const DOC_TYPE_READ_VERSION: u64 = 0x4285;

    // === Segment ============================================================
    pub const SEGMENT: u64 = 0x18538067;

    // SeekHead (jump table — optional, accelerates seeking)
    pub const SEEK_HEAD: u64 = 0x114D9B74;
    pub const SEEK: u64 = 0x4DBB;
    pub const SEEK_ID: u64 = 0x53AB;
    pub const SEEK_POSITION: u64 = 0x53AC;

    // Segment Info
    pub const INFO: u64 = 0x1549A966;
    pub const TIMESTAMP_SCALE: u64 = 0x2AD7B1; // a.k.a. TimecodeScale, default 1_000_000 (ns)
    pub const DURATION: u64 = 0x4489;
    pub const DATE_UTC: u64 = 0x4461;
    pub const TITLE: u64 = 0x7BA9;
    pub const MUXING_APP: u64 = 0x4D80;
    pub const WRITING_APP: u64 = 0x5741;
    pub const SEGMENT_UID: u64 = 0x73A4;

    // Tracks
    pub const TRACKS: u64 = 0x1654AE6B;
    pub const TRACK_ENTRY: u64 = 0xAE;
    pub const MAX_BLOCK_ADDITION_ID: u64 = 0x55EE;
    pub const BLOCK_ADDITION_MAPPING: u64 = 0x41E4;
    pub const BLOCK_ADD_ID_VALUE: u64 = 0x41F0;
    pub const BLOCK_ADD_ID_NAME: u64 = 0x41A4;
    pub const BLOCK_ADD_ID_TYPE: u64 = 0x41E7;
    pub const BLOCK_ADD_ID_EXTRA_DATA: u64 = 0x41ED;
    pub const TRACK_NUMBER: u64 = 0xD7;
    pub const TRACK_UID: u64 = 0x73C5;
    pub const TRACK_TYPE: u64 = 0x83;
    pub const FLAG_ENABLED: u64 = 0xB9;
    pub const FLAG_DEFAULT: u64 = 0x88;
    pub const FLAG_FORCED: u64 = 0x55AA;
    pub const FLAG_LACING: u64 = 0x9C;
    /// Historical unsigned frame-cache hint; not a decoding allocation limit.
    pub const MIN_CACHE: u64 = 0x6DE7;
    pub const DEFAULT_DURATION: u64 = 0x23E383;
    pub const NAME: u64 = 0x536E;
    pub const LANGUAGE: u64 = 0x22B59C;
    pub const LANGUAGE_IETF: u64 = 0x22B59D;
    pub const CODEC_ID: u64 = 0x86;
    pub const CODEC_PRIVATE: u64 = 0x63A2;
    pub const CODEC_NAME: u64 = 0x258688;
    pub const CODEC_DELAY: u64 = 0x56AA;
    pub const SEEK_PRE_ROLL: u64 = 0x56BB;

    // Video sub-element
    pub const VIDEO: u64 = 0xE0;
    pub const PIXEL_WIDTH: u64 = 0xB0;
    pub const PIXEL_HEIGHT: u64 = 0xBA;
    pub const DISPLAY_WIDTH: u64 = 0x54B0;
    pub const DISPLAY_HEIGHT: u64 = 0x54BA;
    pub const FRAME_RATE: u64 = 0x2383E3; // deprecated
    pub const FLAG_INTERLACED: u64 = 0x9A;

    // Audio sub-element
    pub const AUDIO: u64 = 0xE1;
    pub const SAMPLING_FREQUENCY: u64 = 0xB5;
    pub const OUTPUT_SAMPLING_FREQUENCY: u64 = 0x78B5;
    pub const CHANNELS: u64 = 0x9F;
    pub const BIT_DEPTH: u64 = 0x6264;

    // Cluster — frame-carrying section
    pub const CLUSTER: u64 = 0x1F43B675;
    pub const TIMESTAMP: u64 = 0xE7; // a.k.a. Timecode
    pub const SIMPLE_BLOCK: u64 = 0xA3;
    pub const BLOCK_GROUP: u64 = 0xA0;
    pub const BLOCK: u64 = 0xA1;
    pub const BLOCK_DURATION: u64 = 0x9B;
    pub const REFERENCE_BLOCK: u64 = 0xFB;

    // Cues — random-access index (optional)
    pub const CUES: u64 = 0x1C53BB6B;
    pub const CUE_POINT: u64 = 0xBB;
    pub const CUE_TIME: u64 = 0xB3;
    pub const CUE_TRACK_POSITIONS: u64 = 0xB7;
    pub const CUE_TRACK: u64 = 0xF7;
    pub const CUE_CLUSTER_POSITION: u64 = 0xF1;

    // Attachments / Chapters / Tags — surfaced as opaque blobs for MKV
    pub const ATTACHMENTS: u64 = 0x1941A469;
    pub const CHAPTERS: u64 = 0x1043A770;
    pub const TAGS: u64 = 0x1254C367;

    // Void / CRC32 — skipped during walks
    pub const VOID: u64 = 0xEC;
    pub const CRC32: u64 = 0xBF;
}

/// Matroska TrackType enum values from the spec.
pub mod track_type {
    pub const VIDEO: u64 = 1;
    pub const AUDIO: u64 = 2;
    pub const COMPLEX: u64 = 3;
    pub const LOGO: u64 = 0x10;
    pub const SUBTITLE: u64 = 0x11;
    pub const BUTTONS: u64 = 0x12;
    pub const CONTROL: u64 = 0x20;
    pub const METADATA: u64 = 0x21;
}

/// CodecID strings recognized by this crate. The full Matroska codec
/// table is much longer — anything not listed here is surfaced to the
/// caller but routed through the `Unsupported` policy in [`profile`](crate::profile).
pub mod codec_id {
    // Video
    pub const V_MPEG4_ISO_AVC: &str = "V_MPEG4/ISO/AVC"; // H.264
    pub const V_MPEGH_ISO_HEVC: &str = "V_MPEGH/ISO/HEVC"; // HEVC
    pub const V_AV1: &str = "V_AV1";
    pub const V_VP8: &str = "V_VP8";
    pub const V_VP9: &str = "V_VP9";

    // Audio
    pub const A_AAC: &str = "A_AAC";
    pub const A_OPUS: &str = "A_OPUS";
    pub const A_VORBIS: &str = "A_VORBIS";
    pub const A_FLAC: &str = "A_FLAC";
    pub const A_PCM_INT_LE: &str = "A_PCM/INT/LIT";
    pub const A_PCM_INT_BE: &str = "A_PCM/INT/BIG";
    pub const A_PCM_FLOAT_LE: &str = "A_PCM/FLOAT/IEEE";

    // Subtitles (MKV only)
    pub const S_TEXT_UTF8: &str = "S_TEXT/UTF8"; // SRT
    pub const S_TEXT_ASS: &str = "S_TEXT/ASS";
    pub const S_TEXT_SSA: &str = "S_TEXT/SSA";
    pub const S_TEXT_WEBVTT: &str = "S_TEXT/WEBVTT";
}
