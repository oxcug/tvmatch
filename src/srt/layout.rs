//! Bounded lexical helpers. Rendering placement is validated, not interpreted.
use super::{MAX_SRT_BYTES, ParseError, ParseErrorKind};
use std::borrow::Cow;

pub(crate) struct Decoded<'a> {
    pub(crate) text: Cow<'a, str>,
    // Only provider provenance consumes the encoding label.
    #[cfg_attr(not(feature = "opensubtitles"), allow(dead_code))]
    pub(crate) encoding: &'static str,
}
/// UTF-8, or explicit UTF-16LE/BE BOM. Never guess a legacy encoding or replace
/// an invalid surrogate. Input and decoded UTF-8 bounds are independent.
pub(crate) fn decode(bytes: &[u8]) -> Result<Decoded<'_>, ParseError> {
    let error = |kind| ParseError { line: 1, kind };
    if bytes.len() > MAX_SRT_BYTES {
        return Err(error(ParseErrorKind::LimitExceeded));
    }
    let encoding = if bytes.starts_with(&[0xff, 0xfe]) {
        Some((true, "utf-16le-bom"))
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        Some((false, "utf-16be-bom"))
    } else {
        None
    };
    if let Some((little, encoding)) = encoding {
        if !(bytes.len() - 2).is_multiple_of(2) {
            return Err(error(ParseErrorKind::InvalidText));
        }
        let units = bytes[2..].chunks_exact(2).map(|pair| {
            if little {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        });
        // Retain a decoded BOM so the record/public parser removes exactly one.
        let mut text = String::from("\u{feff}");
        for ch in char::decode_utf16(units) {
            let ch = ch.map_err(|_| error(ParseErrorKind::InvalidText))?;
            if text.len() + ch.len_utf8() > MAX_SRT_BYTES {
                return Err(error(ParseErrorKind::LimitExceeded));
            }
            text.push(ch);
        }
        Ok(Decoded {
            text: Cow::Owned(text),
            encoding,
        })
    } else {
        let text = std::str::from_utf8(bytes).map_err(|_| error(ParseErrorKind::InvalidText))?;
        Ok(Decoded {
            text: Cow::Borrowed(text),
            encoding: "utf-8",
        })
    }
}

/// One four-byte endpoint per physical line, rather than one fat &str per line.
/// LF, CRLF and CR each end one line; a final terminator adds no phantom line.
pub(crate) struct Lines<'a> {
    text: &'a str,
    ends: Vec<u32>,
}
impl<'a> Lines<'a> {
    pub(crate) fn new(text: &'a str) -> Result<Self, ParseError> {
        if text.len() > MAX_SRT_BYTES {
            return Err(ParseError {
                line: 1,
                kind: ParseErrorKind::LimitExceeded,
            });
        }
        let bytes = text.as_bytes();
        let endpoint =
            |i: usize| bytes[i] == b'\n' || (bytes[i] == b'\r' && bytes.get(i + 1) != Some(&b'\n'));
        let count = (0..bytes.len()).filter(|&i| endpoint(i)).count()
            + usize::from(!bytes.is_empty() && !endpoint(bytes.len() - 1));
        let mut ends = Vec::with_capacity(count);
        for i in 0..bytes.len() {
            if endpoint(i) {
                ends.push((i + 1) as u32);
            }
        }
        if !bytes.is_empty() && !endpoint(bytes.len() - 1) {
            ends.push(bytes.len() as u32);
        }
        Ok(Self { text, ends })
    }
    pub(crate) fn len(&self) -> usize {
        self.ends.len()
    }
    pub(crate) fn get(&self, i: usize) -> Option<&'a str> {
        let end = *self.ends.get(i)? as usize;
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        let line = &self.text[start..end];
        let line = line.strip_suffix('\n').unwrap_or(line);
        Some(line.strip_suffix('\r').unwrap_or(line))
    }
    pub(crate) fn at(&self, i: usize) -> &'a str {
        self.get(i).expect("validated line index")
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = &'a str> + '_ {
        (0..self.len()).map(|i| self.at(i))
    }
    #[cfg(feature = "opensubtitles")]
    pub(crate) fn range(&self, start: usize, end: usize) -> impl Iterator<Item = &'a str> + '_ {
        (start..end).map(|i| self.at(i))
    }
    #[cfg(feature = "opensubtitles")]
    pub(crate) fn bare_cr_offsets(text: &str) -> impl Iterator<Item = usize> + '_ {
        text.bytes().enumerate().filter_map(|(i, b)| {
            (b == b'\r' && text.as_bytes().get(i + 1) != Some(&b'\n')).then_some(i)
        })
    }
    #[cfg(test)]
    pub(crate) fn index_bytes(&self) -> usize {
        self.ends.capacity() * std::mem::size_of::<u32>()
    }
}

pub(crate) struct TimingParts<'a> {
    pub(crate) start: &'a str,
    pub(crate) end: &'a str,
    // Validation always runs; only provider provenance retains the rectangle.
    #[cfg_attr(not(feature = "opensubtitles"), allow(dead_code))]
    pub(crate) placement: Option<[u32; 4]>,
}
/// Exact optional X1/X2/Y1/Y2 extension, in that order, unsigned u32 coordinates.
/// No partial settings, extras, guessed precision, or whitespace-trimmed captions.
pub(crate) fn timing_parts(line: &str) -> Option<TimingParts<'_>> {
    let (start, rest) = line.split_once("-->")?;
    let mut fields = rest.split_whitespace();
    let end = fields.next()?;
    let placement = if let Some(first) = fields.next() {
        let mut values = [0; 4];
        for (i, key) in ["X1:", "X2:", "Y1:", "Y2:"].into_iter().enumerate() {
            let field = if i == 0 { first } else { fields.next()? };
            let number = field.strip_prefix(key)?;
            if number.is_empty() || number.len() > 10 || !number.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            values[i] = number.parse().ok()?;
        }
        if fields.next().is_some() || values[0] > values[1] || values[2] > values[3] {
            return None;
        }
        Some(values)
    } else {
        None
    };
    Some(TimingParts {
        start: start.trim(),
        end,
        placement,
    })
}
