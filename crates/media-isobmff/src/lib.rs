//! ISO Base Media File Format (ISOBMFF) box layer, originating in media-core.
//!
//! Specs:
//! - ISO/IEC 14496-12 — base container (`ftyp`, `moov`, `mdat`, `moof`, `traf`, …)
//! - ISO/IEC 23008-12 — HEIF box subset (`meta`, `iloc`, `iinf`, `iref`, `iprp`, …)
//!
//! Native encoders in [`boxes`] include the tkhd flags, dref self-contained flag,
//! esds expandable lengths and tfhd default-base-is-moof handling. `mp4-atom`
//! is a dev-only oracle; its bytewise workarounds are test-only helpers.

pub mod error;
pub use error::{IsobmffError, IsobmffResult};

// Native ISOBMFF box encoders. The canonical production path.
pub mod boxes;

#[cfg(feature = "fmp4")]
pub mod fmp4;

#[cfg(feature = "heif")]
pub mod heif;

// Read-side ISOBMFF parsing — walks moov/trak/mdia/minf/stbl to
// materialize a video track's sample table. Companion to [`fmp4`]
// (write-side fragments) and [`heif`] (HEIF parse).
pub mod parse;

// Bounded Read + Seek demuxing. Codec interpretation remains consumer-owned.
pub mod demux;

// Test-only corrections to mp4-atom v0.10 output for bit-exact comparisons
// against native writers. Production encoders do not run bytewise patches.
#[cfg(test)]
mod patches;

// Crate-level re-exports for native box construction.
pub use boxes::{
    Atom,
    // Codec sample entries.
    Audio,
    Avc1,
    Avc3,
    AvcC,
    Btrt,
    Codec,
    Dfla,
    Dinf,
    Dops,
    Dref,
    Edts,
    Elst,
    ElstEntry,
    // Primitives and traits.
    Encode,
    Esds,
    FixedPoint,
    Flac,
    FlacMetadataBlock,
    FourCC,
    // Core boxes (foundation + fragments).
    Ftyp,
    FullBox,
    Hdlr,
    Hev1,
    Hvc1,
    HvcC,
    // Init-segment boxes.
    Matrix,
    Mdat,
    Mdhd,
    Mdia,
    Mfhd,
    Minf,
    Moof,
    Moov,
    Mp4a,
    Mvex,
    Mvhd,
    Opus,
    Smhd,
    Stbl,
    Stco,
    Stsc,
    StscEntry,
    Stsd,
    Stsz,
    StszSamples,
    Stts,
    SttsEntry,
    Styp,
    Tfdt,
    Tfhd,
    Tkhd,
    Traf,
    Trak,
    Trex,
    Trun,
    TrunEntry,
    Url,
    Visual,
    Vmhd,
    // u24 helper — used by FLAC StreamInfo frame-size fields.
    u24,
};

/// Re-export the native `esds` descriptor module so the AAC builder
/// reaches for `esds::EsDescriptor` / `esds::DecoderConfig` etc.
/// without naming the parent `boxes` module.
pub mod esds {
    pub use crate::boxes::esds::*;
}
