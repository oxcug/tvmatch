use std::{error::Error, fmt};

pub(crate) mod layout;

pub const MAX_SRT_BYTES: usize = 1_048_576;
pub const MAX_CUES: usize = 4096;
pub const MAX_CUE_BYTES: usize = 4096;
/// Same 00–99 hour range as the SRT parser; also bounds matcher arithmetic.
pub const MAX_TIMESTAMP_MS: u64 = 359_999_999;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseErrorKind {
    Empty,
    LimitExceeded,
    InvalidIndex,
    InvalidTimestamp,
    InvalidTiming,
    MissingText,
    MissingSeparator,
    InvalidText,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub kind: ParseErrorKind,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SRT line {}: {:?}", self.line, self.kind)
    }
}
impl Error for ParseError {}

#[derive(Debug, Clone)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Transcript {
    cues: Vec<Cue>,
}

impl Transcript {
    pub fn cues(&self) -> &[Cue] {
        &self.cues
    }

    /// Checked non-SRT input. Ends must be positive-duration and timestamps stay
    /// within the SRT range. Callers own any provenance for synthetic end times.
    /// `line` in returned errors is the one-based cue number for this constructor.
    pub fn from_cues(cues: Vec<Cue>) -> Result<Self, ParseError> {
        let fail = |line, kind| ParseError { line, kind };
        if cues.is_empty() {
            return Err(fail(1, ParseErrorKind::Empty));
        }
        if cues.len() > MAX_CUES {
            return Err(fail(1, ParseErrorKind::LimitExceeded));
        }
        let mut total = 0usize;
        for (i, cue) in cues.iter().enumerate() {
            if cue.end_ms <= cue.start_ms
                || cue.end_ms > MAX_TIMESTAMP_MS
                || (i > 0 && cues[i - 1].start_ms > cue.start_ms)
            {
                return Err(fail(i + 1, ParseErrorKind::InvalidTiming));
            }
            if cue
                .text
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
            {
                return Err(fail(i + 1, ParseErrorKind::InvalidText));
            }
            if cue.text.trim().is_empty() {
                return Err(fail(i + 1, ParseErrorKind::MissingText));
            }
            total = total.saturating_add(cue.text.len());
            if cue.text.len() > MAX_CUE_BYTES || total > MAX_SRT_BYTES {
                return Err(fail(i + 1, ParseErrorKind::LimitExceeded));
            }
        }
        Ok(Self { cues })
    }

    /// Decode bounded UTF-8 or BOM-marked UTF-16LE/BE without encoding guesses.
    /// All parsed-text evidence constraints remain those of `parse`.
    pub fn parse_bytes(input: &[u8]) -> Result<Self, ParseError> {
        let decoded = layout::decode(input)?;
        Self::parse(&decoded.text)
    }

    /// Bounded UTF-8 SRT: sequential numeric indices and nondecreasing starts.
    /// LF/CRLF/CR, arrow whitespace and comma/dot three-digit milliseconds are
    /// accepted. Optional X1/X2/Y1/Y2 placement is validated but not projected;
    /// overlapping captions are allowed and markup remains literal text.
    pub fn parse(input: &str) -> Result<Self, ParseError> {
        let fail = |line, kind| ParseError { line, kind };
        if input.len() > MAX_SRT_BYTES {
            return Err(fail(1, ParseErrorKind::LimitExceeded));
        }
        let input = input.strip_prefix('\u{feff}').unwrap_or(input);
        let lines = layout::Lines::new(input)?;
        // Validate before trimming: form feed and vertical tab are whitespace,
        // but must not silently become cue separators or timestamp padding.
        for (index, line) in lines.iter().enumerate() {
            if line.chars().any(|c| c.is_control() && c != '\t') {
                return Err(fail(index + 1, ParseErrorKind::InvalidText));
            }
        }
        let mut position = 0;
        let mut cues: Vec<Cue> = Vec::new();
        while position < lines.len() {
            if lines.at(position).trim().is_empty() {
                position += 1;
                continue;
            }
            if cues.len() == MAX_CUES {
                return Err(fail(position + 1, ParseErrorKind::LimitExceeded));
            }
            let index = lines.at(position).trim();
            if index.is_empty()
                || !index.bytes().all(|b| b.is_ascii_digit())
                || index.parse::<usize>().ok() != Some(cues.len() + 1)
            {
                return Err(fail(position + 1, ParseErrorKind::InvalidIndex));
            }
            position += 1;
            let timing = lines
                .get(position)
                .ok_or_else(|| fail(position + 1, ParseErrorKind::InvalidTimestamp))?;
            let parts = layout::timing_parts(timing)
                .ok_or_else(|| fail(position + 1, ParseErrorKind::InvalidTimestamp))?;
            let start_ms = compatible_timestamp(parts.start)
                .ok_or_else(|| fail(position + 1, ParseErrorKind::InvalidTimestamp))?;
            let end_ms = compatible_timestamp(parts.end)
                .ok_or_else(|| fail(position + 1, ParseErrorKind::InvalidTimestamp))?;
            if end_ms <= start_ms || cues.last().is_some_and(|cue| cue.start_ms > start_ms) {
                return Err(fail(position + 1, ParseErrorKind::InvalidTiming));
            }
            position += 1;
            let mut text = String::new();
            while position < lines.len() && !lines.at(position).trim().is_empty() {
                let line = lines.at(position);
                // Do not silently absorb a second cue's timing line as dialogue when
                // its required blank separator is missing.
                if layout::timing_parts(line).is_some_and(|p| {
                    compatible_timestamp(p.start).is_some() && compatible_timestamp(p.end).is_some()
                }) {
                    return Err(fail(position + 1, ParseErrorKind::MissingSeparator));
                }
                let separator = usize::from(!text.is_empty());
                if text.len() + separator + line.len() > MAX_CUE_BYTES {
                    return Err(fail(position + 1, ParseErrorKind::LimitExceeded));
                }
                if separator != 0 {
                    text.push('\n');
                }
                text.push_str(line);
                position += 1;
            }
            if text.is_empty() {
                return Err(fail(position + 1, ParseErrorKind::MissingText));
            }
            cues.push(Cue {
                start_ms,
                end_ms,
                text,
            });
        }
        if cues.is_empty() {
            return Err(fail(1, ParseErrorKind::Empty));
        }
        Ok(Self { cues })
    }
}

// Keep canonical spelling detection separate: provider provenance depends on it.
pub(crate) fn timestamp(value: &str) -> Option<u64> {
    timestamp_impl(value, false)
}
fn compatible_timestamp(value: &str) -> Option<u64> {
    if value.as_bytes().get(8) == Some(&b'.') {
        timestamp_impl(value, true)
    } else {
        timestamp(value)
    }
}
fn timestamp_impl(value: &str, dot: bool) -> Option<u64> {
    let bytes = value.as_bytes();
    if bytes.len() != 12
        || bytes[2] != b':'
        || bytes[5] != b':'
        || (bytes[8] != b',' && !(dot && bytes[8] == b'.'))
        || bytes
            .iter()
            .enumerate()
            .any(|(i, b)| !matches!(i, 2 | 5 | 8) && !b.is_ascii_digit())
    {
        return None;
    }
    let pair = |i: usize| u64::from(bytes[i] - b'0') * 10 + u64::from(bytes[i + 1] - b'0');
    let (hours, minutes, seconds) = (pair(0), pair(3), pair(6));
    if minutes >= 60 || seconds >= 60 {
        return None;
    }
    let millis = pair(9) * 10 + u64::from(bytes[11] - b'0');
    Some(((hours * 60 + minutes) * 60 + seconds) * 1000 + millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_api_decodes_only_explicit_boms_without_replacement_or_size_bypass() {
        let source = "1\r00:00:01,000-->00:00:02,000\r  東京 😀  ";
        for little in [true, false] {
            let mut bytes = if little {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for unit in source.encode_utf16() {
                bytes.extend(if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            let parsed = Transcript::parse_bytes(&bytes).unwrap();
            assert_eq!(parsed.cues[0].text, "  東京 😀  ");
            assert_eq!(parsed.cues[0].start_ms, 1000);
            bytes.push(0);
            assert_eq!(
                Transcript::parse_bytes(&bytes).unwrap_err().kind,
                ParseErrorKind::InvalidText
            );
        }
        for bytes in [
            vec![0xff],
            vec![0xff, 0xfe, 0, 0xd8],
            vec![0xfe, 0xff, 0xdc, 0],
            vec![0xff, 0xfe, 0xff, 0xfe],
        ] {
            assert!(Transcript::parse_bytes(&bytes).is_err());
        }
        // Decoding BMP characters can expand UTF-16 into UTF-8 beyond the source cap.
        let mut expanded = vec![0xff, 0xfe];
        for _ in 0..MAX_SRT_BYTES / 3 {
            expanded.extend([0, 0x4e]);
        }
        assert!(expanded.len() < MAX_SRT_BYTES);
        assert_eq!(
            Transcript::parse_bytes(&expanded).unwrap_err().kind,
            ParseErrorKind::LimitExceeded
        );
        assert_eq!(
            Transcript::parse_bytes(&vec![b'a'; MAX_SRT_BYTES + 1])
                .unwrap_err()
                .kind,
            ParseErrorKind::LimitExceeded
        );
    }
    #[test]
    fn compatible_layout_corpus_preserves_every_text_and_time_field() {
        for newline in ["\n", "\r\n", "\r"] {
            for arrow in [" --> ", "-->", "\t-->\t", "  -->  "] {
                for decimal in [",", "."] {
                    for settings in ["", " X1:0 X2:720 Y1:10 Y2:576"] {
                        let source = format!(
                            "\u{feff}1{newline}00:00:01{decimal}002{arrow}00:00:03{decimal}004{settings}{newline}  Étoile 東京\t {newline}<i>second line</i>{newline}{newline}2{newline}00:00:02{decimal}000{arrow}00:00:04{decimal}000{newline}EOF text"
                        );
                        let p = Transcript::parse(&source).unwrap();
                        assert_eq!(p.cues.len(), 2);
                        assert_eq!(
                            (
                                p.cues[0].start_ms,
                                p.cues[0].end_ms,
                                p.cues[0].text.as_str()
                            ),
                            (1002, 3004, "  Étoile 東京\t \n<i>second line</i>")
                        );
                        assert_eq!(
                            (
                                p.cues[1].start_ms,
                                p.cues[1].end_ms,
                                p.cues[1].text.as_str()
                            ),
                            (2000, 4000, "EOF text")
                        );
                    }
                }
            }
        }
        assert!(
            timestamp("00:00:01.002").is_none(),
            "legacy canonical detection must not change"
        );
    }
    #[test]
    fn placement_and_clock_extensions_fail_closed_on_partial_or_malformed_fields() {
        for suffix in [
            " X1:2",
            " X1:0 X2:1 Y1:0",
            " X1:2 X2:1 Y1:0 Y2:1",
            " X1:0 X2:1 Y1:2 Y2:1",
            " X1:-1 X2:1 Y1:0 Y2:1",
            " X1:0 X2:4294967296 Y1:0 Y2:1",
            " X1:0 X2:1 Y1:0 Y2:1 junk",
            " X1:0 X2:1 Y1:0 Y1:1",
            " align:start",
        ] {
            let raw = format!("1\n00:00:01,000-->00:00:02,000{suffix}\ntext");
            assert_eq!(
                Transcript::parse(&raw).unwrap_err().kind,
                ParseErrorKind::InvalidTimestamp
            );
        }
        for invalid in [
            "00:60:00.000",
            "00:00:60.000",
            "00:00:01.1",
            "00:00:01.0000",
            "100:00:00.000",
        ] {
            assert_eq!(
                Transcript::parse(&format!("1\n{invalid}-->00:00:02,000\ntext"))
                    .unwrap_err()
                    .kind,
                ParseErrorKind::InvalidTimestamp
            );
        }
        assert_eq!(Transcript::parse("1\n00:00:00,000-->00:00:01,000\nfirst\n2\n00:00:02.000-->00:00:03.000 X1:0 X2:1 Y1:0 Y2:1\nsecond").unwrap_err().kind,ParseErrorKind::MissingSeparator);
    }
    #[test]
    fn compact_line_index_matches_endings_and_bounds_newline_heavy_inputs() {
        for (source, expected) in [
            ("", vec![]),
            ("\n", vec![""]),
            ("a\r\nb\rc\n", vec!["a", "b", "c"]),
            ("\r\n\r\r\n", vec!["", "", ""]),
            ("é\r末", vec!["é", "末"]),
        ] {
            let lines = layout::Lines::new(source).unwrap();
            assert_eq!(lines.iter().collect::<Vec<_>>(), expected);
        }
        for ending in ["\n", "\r", "\r\n"] {
            let source = ending.repeat(MAX_SRT_BYTES / ending.len());
            let lines = layout::Lines::new(&source).unwrap();
            assert_eq!(lines.len(), source.len() / ending.len());
            assert!(lines.index_bytes() <= 4 * MAX_SRT_BYTES);
            assert_eq!(
                Transcript::parse(&source).unwrap_err().kind,
                ParseErrorKind::Empty
            );
        }
        let error =
            Transcript::parse("1\r00:00:00,000-->00:00:01,000\rtext\r\u{000b}").unwrap_err();
        assert_eq!((error.line, error.kind), (4, ParseErrorKind::InvalidText));
    }
    #[test]
    fn unicode_crlf_bom_multiline_and_overlap() {
        let parsed = Transcript::parse("\u{feff}1\r\n00:00:01,002 --> 00:00:03,004\r\nÉtoile 東京\r\nвторая строка\r\n\r\n2\r\n00:00:02,000 --> 00:00:04,000\r\nFin\r\n").unwrap();
        assert_eq!(parsed.cues()[0].start_ms, 1002);
        assert_eq!(parsed.cues()[0].end_ms, 3004);
        assert_eq!(parsed.cues()[0].text, "Étoile 東京\nвторая строка");
        assert_eq!(parsed.cues().len(), 2);
    }

    #[test]
    fn malformed_and_huge_timestamps_are_typed_errors() {
        for value in [
            "00:60:00,000",
            "00:00:60,000",
            "-1:00:00,000",
            "99999999999999999999:00:00,000",
            "é0:00:00,000",
            "00:00:00,00",
            "00:00:00,000 X1:2",
        ] {
            let srt = format!("1\n{value} --> 00:01:00,000\ntext\n");
            let error = Transcript::parse(&srt).unwrap_err();
            assert_eq!(error.kind, ParseErrorKind::InvalidTimestamp, "{value}");
            assert_eq!(error.line, 2);
        }
    }

    #[test]
    fn rejects_empty_bad_indices_missing_text_and_backwards_time() {
        for input in ["", "\u{feff}\r\n \r\n"] {
            assert_eq!(
                Transcript::parse(input).unwrap_err().kind,
                ParseErrorKind::Empty
            );
        }
        for (input, kind) in [
            (
                "2\n00:00:01,000 --> 00:00:02,000\nword",
                ParseErrorKind::InvalidIndex,
            ),
            (
                "1\n00:00:01,000 --> 00:00:01,000\nword",
                ParseErrorKind::InvalidTiming,
            ),
            (
                "1\n00:00:02,000 --> 00:00:01,000\nword",
                ParseErrorKind::InvalidTiming,
            ),
            (
                "1\n00:00:01,000 --> 00:00:02,000\n",
                ParseErrorKind::MissingText,
            ),
            (
                "1\n00:00:01,000 --> 00:00:02,000\nword\0",
                ParseErrorKind::InvalidText,
            ),
            ("1\n", ParseErrorKind::InvalidTimestamp),
            (
                "1\n00:00:02,000 --> 00:00:04,000\none\n\n2\n00:00:01,000 --> 00:00:03,000\ntwo",
                ParseErrorKind::InvalidTiming,
            ),
        ] {
            assert_eq!(Transcript::parse(input).unwrap_err().kind, kind);
        }
    }

    #[test]
    fn rejects_controls_before_whitespace_trimming() {
        for control in ['\u{000c}', '\u{000b}'] {
            for (input, expected_line) in [
                (
                    format!("1\n00:00:00,000 --> 00:00:01,000\nhello\n{control}\n"),
                    4,
                ),
                (
                    format!("{control}\n1\n00:00:00,000 --> 00:00:01,000\nhello\n"),
                    1,
                ),
                (
                    format!("1\n{control}00:00:00,000 --> 00:00:01,000\nhello\n"),
                    2,
                ),
                (format!("1\n00:00:00,000 --> 00:00:01,000\n{control}\n"), 3),
            ] {
                let error = Transcript::parse(&input).unwrap_err();
                assert_eq!(error.kind, ParseErrorKind::InvalidText);
                assert_eq!(error.line, expected_line);
            }
        }
    }

    #[test]
    fn accepts_tabs_as_text_and_separators_with_crlf() {
        let parsed = Transcript::parse(
            "\t\r\n1\r\n00:00:00,000 --> 00:00:01,000\r\nhello\tworld\r\n\t\r\n2\r\n00:00:02,000 --> 00:00:03,000\r\nnext line\r\n",
        ).unwrap();
        assert_eq!(parsed.cues().len(), 2);
        assert_eq!(parsed.cues()[0].text, "hello\tworld");
    }

    #[test]
    fn rejects_missing_blank_cue_separator() {
        let input = "1\n00:00:00,000 --> 00:00:01,000\none\n2\n00:00:02,000 --> 00:00:03,000\ntwo";
        let error = Transcript::parse(input).unwrap_err();
        assert_eq!(error.kind, ParseErrorKind::MissingSeparator);
        assert_eq!(error.line, 5);
    }

    #[test]
    fn bounds_bytes_and_caption_count() {
        assert_eq!(
            Transcript::parse(&" ".repeat(MAX_SRT_BYTES + 1))
                .unwrap_err()
                .kind,
            ParseErrorKind::LimitExceeded
        );
        let large = format!(
            "1\n00:00:00,000 --> 00:00:01,000\n{}",
            "a".repeat(MAX_CUE_BYTES + 1)
        );
        assert_eq!(
            Transcript::parse(&large).unwrap_err().kind,
            ParseErrorKind::LimitExceeded
        );
        let many: String = (1..=MAX_CUES + 1)
            .map(|i| format!("{i}\n00:00:00,000 --> 00:00:01,000\nword\n\n"))
            .collect();
        assert_eq!(
            Transcript::parse(&many).unwrap_err().kind,
            ParseErrorKind::LimitExceeded
        );
    }
}
