//! MP4 subtitle adapter. Container boxes, tables, edits and payload IO live in
//! media-core/media-isobmff; only tx3g semantics and tvmatch limits live here.
#[cfg(test)]
mod tests;
use super::{EmbeddedSubtitles, MediaError, SubtitleTimestamp, SubtitleTrack};
use crate::srt::{Cue, MAX_CUE_BYTES, MAX_CUES, MAX_SRT_BYTES, MAX_TIMESTAMP_MS, Transcript};
use media_isobmff::demux::{BoxBudget, Demuxer, Limits, Track};
use std::io::{Read, Seek};
type Result<T> = std::result::Result<T, MediaError>;
fn container(e: media_isobmff::IsobmffError) -> MediaError {
    MediaError::Container(format!("MP4: {e}"))
}
fn bad(s: &str) -> MediaError {
    MediaError::Container(format!("MP4 tx3g: {s}"))
}
fn u16be(b: &[u8], i: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(
        b.get(i..i + 2)
            .ok_or_else(|| bad("short text field"))?
            .try_into()
            .unwrap(),
    ))
}
fn open<R: Read + Seek>(reader: R) -> Result<Demuxer<R>> {
    Demuxer::with_limits(
        reader,
        Limits {
            read_bytes: 16 * 1024 * 1024,
            sample_bytes: 64 * 1024,
        },
    )
    .map_err(container)
}
fn tracks(source: &[Track], budget: &mut BoxBudget) -> Result<Vec<SubtitleTrack>> {
    let mut result = Vec::new();
    for t in source {
        if !matches!(&t.handler, b"text" | b"sbtl" | b"subt" | b"clcp") {
            continue;
        }
        let d = t
            .descriptions
            .first()
            .ok_or(MediaError::InvalidTrackMetadata)?;
        let mut supported =
            t.unsupported_reason.is_none() && t.descriptions.len() == 1 && d.format == *b"tx3g";
        let mut forced = false;
        if d.format == *b"tx3g" {
            let config = &d.configuration;
            if config.len() < 30 {
                return Err(bad("short sample description"));
            }
            forced = u32::from_be_bytes(config[..4].try_into().unwrap()) & 0xc0000000 != 0;
            if budget
                .read(&config[30..])
                .map_err(container)?
                .iter()
                .any(|a| &a.kind == b"sinf")
            {
                supported = false;
            }
        }
        result.push(SubtitleTrack {
            number: u64::from(t.id),
            uid: u64::from(t.id),
            codec_id: String::from_utf8_lossy(&d.format).into_owned(),
            language: t.language.clone(),
            name: None,
            enabled: t.enabled,
            default: false,
            forced,
            default_duration_ns: None,
            supported,
        });
    }
    Ok(result)
}
pub(super) fn probe<R: Read + Seek>(reader: R) -> Result<Vec<SubtitleTrack>> {
    tracks(open(reader)?.tracks(), &mut BoxBudget::default())
}
fn text(b: &[u8], track: u64, budget: &mut BoxBudget) -> Result<String> {
    let n = u16be(b, 0)? as usize;
    let raw = b.get(2..2 + n).ok_or_else(|| bad("truncated text"))?;
    let decoded = if raw.starts_with(&[0xfe, 0xff]) || raw.starts_with(&[0xff, 0xfe]) {
        if raw.len() % 2 != 0 {
            return Err(bad("odd UTF-16 text"));
        }
        let little = raw[0] == 0xff;
        let words = raw[2..]
            .chunks_exact(2)
            .map(|b| {
                if little {
                    u16::from_le_bytes([b[0], b[1]])
                } else {
                    u16::from_be_bytes([b[0], b[1]])
                }
            })
            .collect::<Vec<_>>();
        String::from_utf16(&words).map_err(|_| bad("invalid UTF-16 text"))?
    } else {
        let s = std::str::from_utf8(raw).map_err(|_| MediaError::InvalidUtf8)?;
        s.strip_prefix('\u{feff}').unwrap_or(s).to_owned()
    };
    for a in budget.read(&b[2 + n..]).map_err(container)? {
        match &a.kind {
            b"styl" => {
                let count = u16be(a.data, 0)? as usize;
                if a.data.len() != 2 + count * 12 {
                    return Err(bad("invalid text style records"));
                }
            }
            b"tbox" if a.data.len() == 8 => {}
            _ => return Err(MediaError::UnsupportedTrack(track)),
        }
    }
    let decoded = decoded.replace("\r\n", "\n").replace('\r', "\n");
    if decoded.len() > MAX_CUE_BYTES {
        return Err(MediaError::LimitExceeded("MP4 cue bytes"));
    }
    Ok(decoded)
}
pub(super) fn extract<R: Read + Seek>(
    reader: R,
    selector: Option<u64>,
) -> Result<EmbeddedSubtitles> {
    let file = open(reader)?;
    let mut budget = BoxBudget::default();
    let mut candidates = tracks(file.tracks(), &mut budget)?;
    let index = match selector {
        Some(id) => candidates
            .iter()
            .position(|t| t.number == id)
            .ok_or_else(|| {
                if file.tracks().iter().any(|t| u64::from(t.id) == id) {
                    MediaError::UnsupportedTrack(id)
                } else {
                    MediaError::TrackNotFound(id)
                }
            })?,
        None => match candidates.len() {
            0 => return Err(MediaError::NoSubtitleTracks),
            1 => 0,
            _ => {
                return Err(MediaError::AmbiguousSubtitleTracks(
                    candidates.iter().map(|t| t.number).collect(),
                ));
            }
        },
    };
    let track = candidates.remove(index);
    if !track.supported || track.forced {
        return Err(MediaError::UnsupportedTrack(track.number));
    }
    let mut samples = file.into_samples(track.number as u32).map_err(container)?;
    let mut cues = Vec::new();
    let mut timestamps = Vec::new();
    let mut total = 0;
    while let Some(sample) = samples.next_sample().map_err(container)? {
        let text = text(&sample.data, track.number, &mut budget)?;
        total += text.len();
        if total > MAX_SRT_BYTES {
            return Err(MediaError::LimitExceeded("MP4 subtitle text bytes"));
        }
        // Validate even clears, zero-duration and edit-excluded samples.
        if text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        {
            return Err(bad("invalid subtitle control"));
        }
        let Some((start, end)) = sample.presentation_ns else {
            continue;
        };
        if end / 1_000_000 > MAX_TIMESTAMP_MS || start / 1_000_000 >= MAX_TIMESTAMP_MS {
            return Err(MediaError::InvalidTimestamp);
        }
        if text.trim().is_empty() || end <= start {
            continue;
        }
        if cues.len() == MAX_CUES {
            return Err(MediaError::LimitExceeded("MP4 cues"));
        }
        let start_ms = start / 1_000_000;
        let synthetic = end / 1_000_000 <= start_ms;
        cues.push(Cue {
            start_ms,
            end_ms: if synthetic {
                start_ms + 1
            } else {
                end / 1_000_000
            },
            text,
        });
        timestamps.push(SubtitleTimestamp {
            start_ns: start,
            block_duration_ns: Some(sample.source_duration_ns),
            declared_end_ns: Some(end),
            transcript_end_synthetic: synthetic,
        });
    }
    if cues.is_empty() {
        return Err(MediaError::NoCaptions);
    }
    Ok(EmbeddedSubtitles {
        track,
        transcript: Transcript::from_cues(cues).map_err(MediaError::Transcript)?,
        timestamps,
    })
}
