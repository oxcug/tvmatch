//! Hand-rolled error enum. No thiserror — keep the dep graph empty.

use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// Source ended mid-element. Carries the field that was being read.
    UnexpectedEof(&'static str),
    /// VINT width byte was 0x00 — reserved per RFC 8794.
    InvalidVint,
    /// Element size (or VINT) is larger than what the reader allows.
    SizeOverflow,
    /// EBML header missing or DocType not matroska/webm.
    NotMatroska,
    /// DocType + codec combination forbidden by the WebM profile.
    Unsupported { what: &'static str },
    /// Field outside the bounded streaming allowlist, identified for compatibility diagnosis.
    UnsupportedElement { parent: u64, element: u64 },
    /// Element payload didn't match its declared type (e.g. odd-length float).
    Malformed(&'static str),
    /// std::io::Error from a streaming reader.
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof(what) => write!(f, "unexpected EOF while reading {what}"),
            Self::InvalidVint => f.write_str("invalid EBML VINT (zero width byte)"),
            Self::SizeOverflow => f.write_str("element size exceeds reader bounds"),
            Self::NotMatroska => f.write_str("not a Matroska or WebM file"),
            Self::Unsupported { what } => write!(f, "unsupported: {what}"),
            Self::UnsupportedElement { parent, element } => write!(
                f,
                "unsupported: element 0x{element:X} in parent 0x{parent:X} is outside the bounded streaming allowlist"
            ),
            Self::Malformed(what) => write!(f, "malformed {what}"),
            Self::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
