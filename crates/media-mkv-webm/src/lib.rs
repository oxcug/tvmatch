//! Matroska + WebM container family.
//!
//! User-facing entry point for `.mkv`, `.mka`, `.mks`, `.webm`. WebM is
//! a strict codec-restricted profile of Matroska — same EBML base,
//! same segment/cluster/block walk — so both share one demuxer. The muxer
//! enforces the WebM codec whitelist. Readers report DocType without that
//! enforcement; callers can apply the [`profile`] policy helper.
//!
//! Container parse (EBML element walk + Matroska schema) lives in
//! [`ebml`]. Optional audio decoding uses Symphonia; video decoding is caller-owned.
//! This crate originates in media-core.

pub mod audio;
pub mod cluster;
pub mod demux;
pub mod ebml;
pub mod mux;
pub mod pgs;
pub mod profile;
pub mod streaming;

mod error;
pub use error::{Error, Result};

pub use audio::{AudioDecoder, open_audio_decoder};
pub use cluster::{Frame, Frames};
pub use demux::{AudioParams, CuePoint, Demuxer, TrackInfo, TrackKind, VideoParams};
pub use mux::{Muxer, TrackDescriptor};
pub use profile::DocType;
pub use streaming::{
    OwnedFrame, StreamingDemuxer, StreamingLimits, open_streaming, open_streaming_with_limits,
};
