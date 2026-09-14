//! One bounded framing pass. Recognize timing records before applying numbering,
//! omission or ordering. Every nonblank line belongs to a header or caption.
use super::{Result, fail};
use crate::srt::{Cue, MAX_CUE_BYTES, MAX_CUES, MAX_SRT_BYTES, timestamp};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::opensubtitles) struct Label {
    pub whole: u32,
    fraction: u32,
}
fn label(s: &str) -> Option<Label> {
    let (whole, fraction) = s.trim().split_once('.').unwrap_or((s.trim(), ""));
    if whole.is_empty()
        || whole.len() > 10
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > 9
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || (s.contains('.') && fraction.is_empty())
    {
        return None;
    }
    Some(Label {
        whole: whole.parse().ok()?,
        fraction: if fraction.is_empty() {
            0
        } else {
            fraction.parse::<u32>().ok()? * 10u32.pow((9 - fraction.len()) as u32)
        },
    })
}
#[derive(Clone, Copy)]
struct Timing {
    start: u64,
    end: u64,
    normalized: bool,
    placement: Option<[u32; 4]>,
}
fn clock(s: &str) -> Option<u64> {
    let (h, rest) = s.split_once(':')?;
    let (m, rest) = rest.split_once(':')?;
    let (sec, ms) = rest.split_once(',').or_else(|| rest.split_once('.'))?;
    // Integer clock fields have fixed units; padding is insignificant. Fractional
    // seconds still require exactly three digits: never guess missing precision.
    if [h, m, sec]
        .iter()
        .any(|v| v.is_empty() || v.len() > 6 || !v.bytes().all(|b| b.is_ascii_digit()))
        || ms.len() != 3
        || !ms.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let (h, m, sec, ms) = (
        h.parse::<u64>().ok()?,
        m.parse::<u64>().ok()?,
        sec.parse::<u64>().ok()?,
        ms.parse::<u64>().ok()?,
    );
    if h > 99 || m > 59 || sec > 59 {
        return None;
    }
    Some(((h * 60 + m) * 60 + sec) * 1000 + ms)
}
fn timing(s: &str, line: usize) -> Result<Option<Timing>> {
    let Some((a, b)) = s.split_once("-->") else {
        return Ok(None);
    };
    let (a, b) = (a.trim(), b.trim());
    let parts = crate::srt::layout::timing_parts(s);
    let values = parts
        .as_ref()
        .and_then(|p| clock(p.start).zip(clock(p.end)));
    if values.is_none() {
        // Do not silently retain malformed clock-shaped headers as caption text.
        if [a, b].iter().any(|s| {
            s.contains(':')
                && s.trim_start_matches(['-', '+'])
                    .starts_with(|c: char| c.is_ascii_digit())
        }) {
            return Err(fail(&format!(
                "provider record import: SRT line {line}: InvalidTimestamp"
            )));
        }
        return Ok(None);
    }
    let (start, end) = values.unwrap();
    if end < start {
        return Err(fail(&format!(
            "provider record import: SRT line {line}: InvalidTiming; negative duration is not skipped"
        )));
    }
    let normalized = !s.split_once(" --> ").is_some_and(|(a, b)| {
        timestamp(a.trim()) == Some(start) && timestamp(b.trim()) == Some(end)
    });
    Ok(Some(Timing {
        start,
        end,
        normalized,
        placement: parts.unwrap().placement,
    }))
}
#[derive(Debug)]
pub(in crate::opensubtitles) struct Record {
    pub cue: Cue,
    pub label: Option<Label>,
    pub label_text: Option<String>,
    pub header_line: usize,
    pub timing_line: usize,
    pub body_end: usize,
    pub separated: bool,
    pub normalized: bool,
    /// Original one-based physical lines inside the caption, not cue separators.
    /// The framer retains them in cue.text; only the provider may normalize them.
    pub caption_blank_lines: Vec<usize>,
    pub placement: Option<[u32; 4]>,
}
impl Record {
    pub fn integer(&self) -> Option<u32> {
        self.label_text
            .as_ref()
            .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))
            .and(self.label.map(|l| l.whole))
    }
}
/// A recoverable record-level error, not permission to discard malformed input.
/// Metadata is retained so callers can audit an explicitly chosen omission.
#[derive(Debug)]
pub(in crate::opensubtitles) enum RecordError {
    MissingText(Box<Record>),
}
impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingText(_) => f.write_str("provider record import: MissingText"),
        }
    }
}
impl std::error::Error for RecordError {}
pub(in crate::opensubtitles) type RecordResult = std::result::Result<Record, RecordError>;

pub(in crate::opensubtitles) fn parse(text: &str) -> Result<Vec<RecordResult>> {
    if text.len() > MAX_SRT_BYTES {
        return Err(fail("provider record import byte cap exceeded"));
    }
    let lines = crate::srt::layout::Lines::new(text.strip_prefix('\u{feff}').unwrap_or(text))
        .map_err(|e| fail(&format!("provider record import: {e}")))?;
    let mut times = BTreeMap::new();
    for (i, line) in lines.iter().enumerate() {
        if line
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\t' | '\u{0092}' | '\u{009d}'))
        {
            return Err(fail(&format!(
                "provider record import: SRT line {}: InvalidText",
                i + 1
            )));
        }
        if let Some(t) = timing(line, i + 1)? {
            if times.len() == MAX_CUES {
                return Err(fail("provider record import cue cap exceeded"));
            }
            times.insert(i, t);
        }
    }
    // Reliable right anchors are explicit labels at a conventional block start.
    let anchors = times
        .keys()
        .filter_map(|i| {
            if *i >= 1 && (*i == 1 || lines.at(*i - 2).trim().is_empty()) {
                label(lines.at(*i - 1)).map(|l| (*i, l))
            } else {
                None
            }
        })
        .collect::<BTreeMap<_, _>>();
    let mut records = Vec::new();
    let mut pos = 0;
    let mut previous: Option<Label> = None;
    while pos < lines.len() {
        if lines.at(pos).trim().is_empty() {
            pos += 1;
            continue;
        }
        let header = pos;
        let separated = pos == 0 || lines.at(pos - 1).trim().is_empty();
        let (index, index_text) = if times.contains_key(&pos) {
            (None, None)
        } else {
            let index = label(lines.at(pos)).ok_or_else(|| {
                fail(&format!(
                    "provider record import: SRT line {}: InvalidIndex",
                    pos + 1
                ))
            })?;
            if previous.is_none_or(|p| index <= p)
                && (previous.is_some()
                    || index
                        != (Label {
                            whole: 1,
                            fraction: 0,
                        }))
            {
                return Err(fail(&format!(
                    "provider record import: SRT line {}: InvalidIndex; explicit labels must start at 1 and increase strictly",
                    pos + 1
                )));
            }
            if !separated {
                let integer_successor = previous.is_some_and(|p| {
                    p.fraction == 0
                        && index.fraction == 0
                        && p.whole.checked_add(1) == Some(index.whole)
                });
                let fractional_between = previous
                    .is_some_and(|p| p.whole == index.whole && p < index && index.fraction > 0)
                    && anchors.range((pos + 2)..).next().is_some_and(|(_, next)| {
                        next.fraction == 0 && index.whole.checked_add(1) == Some(next.whole)
                    });
                if !integer_successor && !fractional_between {
                    return Err(fail(
                        "provider record import: ambiguous numeric caption/cue boundary",
                    ));
                }
            }
            let original = lines.at(pos).trim().to_owned();
            pos += 1;
            previous = Some(index);
            (Some(index), Some(original))
        };
        let time = *times.get(&pos).ok_or_else(|| {
            fail(&format!(
                "provider record import: SRT line {}: InvalidTimestamp",
                pos + 1
            ))
        })?;
        let time_line = pos;
        pos += 1;
        let body_start = pos;
        let mut caption_blank_lines = Vec::new();
        while pos < lines.len() {
            if lines.at(pos).trim().is_empty() {
                // An internal paragraph is only framed across the first blank
                // when both cue headers are conventional consecutive integers,
                // with no intervening timing record. No orphan prefix/EOF repair.
                let right = times.range(pos..).next().and_then(|(t, _)| {
                    let next = anchors.get(t)?;
                    let current = index?;
                    (separated
                        && current.fraction == 0
                        && next.fraction == 0
                        && index_text
                            .as_ref()
                            .is_some_and(|s| s.bytes().all(|b| b.is_ascii_digit()))
                        && lines.at(t - 1).trim().bytes().all(|b| b.is_ascii_digit())
                        && current.whole.checked_add(1) == Some(next.whole))
                    .then_some(t - 1)
                });
                if pos > body_start
                    && let Some(right) = right
                {
                    let end = (pos..right)
                        .rev()
                        .find(|i| !lines.at(*i).trim().is_empty())
                        .map(|i| i + 1);
                    if let Some(end) = end {
                        // Enforce the body cap before collecting blank positions.
                        let mut size = 0;
                        for (n, line) in lines.range(body_start, end).enumerate() {
                            size += line.len() + usize::from(n != 0);
                            if size > MAX_CUE_BYTES {
                                return Err(fail(
                                    "provider record import caption byte cap exceeded",
                                ));
                            }
                        }
                        // Numeric/index-looking fragments or arrow lines remain
                        // ambiguous; do not turn malformed headers into dialogue.
                        if lines.range(pos, end).any(|s| {
                            !s.trim().is_empty()
                                && (s.contains("-->")
                                    || s.trim().chars().all(|c| {
                                        c.is_ascii_digit()
                                            || matches!(
                                                c,
                                                '.' | ',' | '+' | '-' | '\u{0092}' | '\u{009d}'
                                            )
                                    }))
                        }) {
                            return Err(fail(
                                "provider record import: ambiguous caption paragraph/header",
                            ));
                        }
                        caption_blank_lines.extend(
                            (pos..end)
                                .filter(|i| lines.at(*i).trim().is_empty())
                                .map(|i| i + 1),
                        );
                        pos = end;
                    }
                }
                break;
            }
            if times.contains_key(&pos)
                || (times.contains_key(&(pos + 1)) && label(lines.at(pos)).is_some())
            {
                break;
            }
            // An overflowed/malformed numeric label must not become invented words.
            if times.contains_key(&(pos + 1))
                && lines
                    .at(pos)
                    .trim()
                    .chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '.' | '\u{0092}' | '\u{009d}'))
            {
                return Err(fail(
                    "provider record import: ambiguous numeric caption/cue boundary",
                ));
            }
            pos += 1;
        }
        let missing_text = pos == body_start;
        let mut body = String::new();
        for (i, line) in lines.range(body_start, pos).enumerate() {
            let separator = usize::from(i != 0);
            if body.len() + separator + line.len() > MAX_CUE_BYTES {
                return Err(fail("provider record import caption byte cap exceeded"));
            }
            if separator != 0 {
                body.push('\n');
            }
            body.push_str(line);
        }
        let record = Record {
            cue: Cue {
                start_ms: time.start,
                end_ms: time.end,
                text: body,
            },
            label: index,
            label_text: index_text,
            header_line: header + 1,
            timing_line: time_line + 1,
            body_end: pos,
            separated,
            normalized: time.normalized,
            caption_blank_lines,
            placement: time.placement,
        };
        records.push(if missing_text {
            Err(RecordError::MissingText(Box::new(record)))
        } else {
            Ok(record)
        });
    }
    Ok(records)
}
