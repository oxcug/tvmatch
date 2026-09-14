//! Run the local demux unit module in the application regression suite.
//! Production extraction tests separately exercise media_isobmff through the
//! actual library dependency.
#![cfg(feature = "media")]
pub use media_isobmff::{IsobmffError, IsobmffResult};
#[path = "../crates/media-isobmff/src/demux.rs"]
pub mod demux;
