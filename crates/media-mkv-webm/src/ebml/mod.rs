//! EBML element layer per RFC 8794 plus the Matroska schema overlay.
//!
//! Two layers in this module:
//!
//! - [`varint`] + [`element`]: format-agnostic EBML primitives — VINT
//!   decode, element headers, typed payload readers.
//! - [`schema`]: Matroska/WebM element ID constants and the helpers
//!   that turn a raw `Reader` into a structured track / segment table.
//!
//! EBML primitives are separate from the Matroska schema layer.

pub mod element;
pub mod schema;
pub mod varint;
pub mod writer;

pub use element::{ElementHeader, Reader};
pub use schema::ids;
pub use varint::{Vint, read_vint_id, read_vint_size};
