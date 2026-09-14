//! Parse provider records once, then derive numbering/omission/order separately.
//! Legacy policies remain byte-compatible; additional framing has its own ledger.
use super::{Result, boundaries, fail};
use crate::srt::{Cue, MAX_SRT_BYTES, Transcript};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write;
pub(super) mod parser;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceLayout {
    pub(super) policy: String,
    pub(super) encoding: String,
    pub(super) original_cues: usize,
    pub(super) bare_cr_line_endings: usize,
    pub(super) positioned_cues: usize,
    pub(super) layout_sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaptionParagraphs {
    pub(super) policy: String,
    pub(super) original_cues: usize,
    pub(super) affected_cues: usize,
    pub(super) removed_blank_lines: usize,
    pub(super) layout_sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MissingText {
    pub(super) policy: String,
    pub(super) original_cues: usize,
    pub(super) skipped_cues: usize,
    /// Number entering zero-duration omission.
    pub(super) retained_cues: usize,
    pub(super) selection_sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ZeroDuration {
    pub(super) policy: String,
    pub(super) original_cues: usize,
    pub(super) skipped_cues: usize,
    pub(super) retained_cues: usize,
    pub(super) selection_sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CueNumbering {
    pub(super) policy: String,
    pub(super) original_cues: usize,
    pub(super) renumbered_cues: usize,
    pub(super) indices_sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecordFraming {
    pub(super) policy: String,
    pub(super) original_cues: usize,
    pub(super) unindexed_cues: usize,
    pub(super) missing_separators: usize,
    pub(super) fractional_labels: usize,
    pub(super) normalized_timings: usize,
    /// Domain + count + per-record source spans and original label bytes. Raw
    /// bytes/receipt hash additionally bind all caption/timing spellings.
    pub(super) layout_sha256: String,
}
pub(super) struct Prepared {
    pub(super) text: String,
    pub(super) zero_duration: Option<ZeroDuration>,
    pub(super) missing_text: Option<MissingText>,
    pub(super) caption_paragraphs: Option<CaptionParagraphs>,
    pub(super) source_layout: Option<SourceLayout>,
    pub(super) cue_numbering: Option<CueNumbering>,
    pub(super) cue_boundaries: Option<boundaries::CueBoundaries>,
    pub(super) record_framing: Option<RecordFraming>,
    pub(super) caption_normalization: Option<super::CaptionNormalization>,
    retained: Vec<Cue>,
}
impl Prepared {
    pub(super) fn verify(&self, parsed: &Transcript) -> Result<()> {
        let mut expected = self.retained.iter().collect::<Vec<_>>();
        expected.sort_by_key(|cue| cue.start_ms);
        if parsed.cues().len() != expected.len()
            || parsed
                .cues()
                .iter()
                .zip(expected)
                .any(|(a, b)| a.start_ms != b.start_ms || a.end_ms != b.end_ms || a.text != b.text)
        {
            return Err(fail(
                "provider record derivative exact retained-cue roundtrip failed",
            ));
        }
        Ok(())
    }
}
fn stamp(ms: u64) -> String {
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}
pub(super) fn prepare(text: &str) -> Result<Prepared> {
    prepare_decoded(text, "utf-8")
}
pub(super) fn prepare_decoded(text: &str, encoding: &str) -> Result<Prepared> {
    let mut empty = std::collections::BTreeSet::new();
    let mut records = parser::parse(text)?
        .into_iter()
        .enumerate()
        .map(|(i, result)| match result {
            Ok(record) => record,
            Err(parser::RecordError::MissingText(record)) => {
                // The provider importer, not the parser, chooses this policy.
                empty.insert(i);
                *record
            }
        })
        .collect::<Vec<_>>();
    let bare_cr_line_endings = crate::srt::layout::Lines::bare_cr_offsets(text).count();
    let positioned_cues = records.iter().filter(|r| r.placement.is_some()).count();
    let source_layout = (encoding != "utf-8" || bare_cr_line_endings > 0 || positioned_cues > 0)
        .then(|| {
            let mut hash = Sha256::new();
            hash.update(b"tvmatch-provider-srt-source-layout-v1\0");
            hash.update((encoding.len() as u32).to_be_bytes());
            hash.update(encoding.as_bytes());
            for n in [records.len(), bare_cr_line_endings, positioned_cues] {
                hash.update((n as u32).to_be_bytes());
            }
            for offset in crate::srt::layout::Lines::bare_cr_offsets(text) {
                hash.update((offset as u32).to_be_bytes());
            }
            for (i, record) in records.iter().enumerate() {
                if let Some(position) = record.placement {
                    hash.update(((i + 1) as u32).to_be_bytes());
                    hash.update((record.timing_line as u32).to_be_bytes());
                    for coordinate in position {
                        hash.update(coordinate.to_be_bytes());
                    }
                }
            }
            SourceLayout {
                policy: "provider-srt-source-layout-v1".into(),
                encoding: encoding.into(),
                original_cues: records.len(),
                bare_cr_line_endings,
                positioned_cues,
                layout_sha256: format!("{:x}", hash.finalize()),
            }
        });
    // Normalize only classified caption lines, including records later omitted.
    // Structural roles come from the framer, not blank-line counts.
    let mut replacements = 0;
    let mut codepoints = std::collections::BTreeMap::new();
    for r in &mut records {
        for line in r.cue.text.lines() {
            if !line.contains(['\u{0092}', '\u{009d}']) {
                continue;
            }
            let without = line.replace(['\u{0092}', '\u{009d}'], "");
            if without.trim().is_empty()
                || (line.contains('\u{0092}')
                    && (line.contains("-->") || without.trim().bytes().all(|b| b.is_ascii_digit())))
            {
                return Err(fail(
                    "provider caption normalization: InvalidText; ambiguous placeholder",
                ));
            }
            for (c, name) in [('\u{0092}', "U+0092"), ('\u{009d}', "U+009D")] {
                let n = line.matches(c).count();
                if n > 0 {
                    *codepoints.entry(name.to_owned()).or_insert(0) += n;
                    replacements += n;
                }
            }
        }
        r.cue.text = r.cue.text.replace(['\u{0092}', '\u{009d}'], "\u{fffd}");
    }
    if text.len() + replacements > MAX_SRT_BYTES {
        return Err(fail("provider normalized input byte cap exceeded"));
    }
    let caption_normalization = (replacements > 0).then(|| {
        let policy = if codepoints.contains_key("U+0092") {
            "caption-c1-placeholders-v2"
        } else {
            codepoints.clear();
            "caption-u009d-to-ufffd-v1"
        };
        super::CaptionNormalization {
            policy: policy.into(),
            replacements,
            codepoints,
        }
    });
    let original_cues = records.len();
    let affected_cues = records
        .iter()
        .filter(|r| !r.caption_blank_lines.is_empty())
        .count();
    let caption_paragraphs = if affected_cues > 0 {
        let mut hash = Sha256::new();
        hash.update(b"tvmatch-provider-caption-paragraphs-v1\0");
        hash.update((original_cues as u32).to_be_bytes());
        let mut removed_blank_lines = 0;
        for (i, r) in records.iter_mut().enumerate() {
            if r.cue.text.len() > crate::srt::MAX_CUE_BYTES {
                return Err(fail("provider normalized caption byte cap exceeded"));
            }
            if r.caption_blank_lines.is_empty() {
                continue;
            }
            for n in [
                i + 1,
                r.header_line,
                r.timing_line,
                r.body_end,
                r.caption_blank_lines.len(),
            ] {
                hash.update((n as u32).to_be_bytes());
            }
            for line in &r.caption_blank_lines {
                hash.update((*line as u32).to_be_bytes());
            }
            let nonblank = r
                .cue
                .text
                .lines()
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>();
            let removed = r.cue.text.lines().count() - nonblank.len();
            if removed != r.caption_blank_lines.len() {
                return Err(fail("provider caption paragraph layout mismatch"));
            }
            removed_blank_lines += removed;
            // All nonblank caption lines stay byte-identical and in order.
            r.cue.text = nonblank.join("\n");
        }
        Some(CaptionParagraphs {
            policy: "provider-caption-paragraphs-v1".into(),
            original_cues,
            affected_cues,
            removed_blank_lines,
            layout_sha256: format!("{:x}", hash.finalize()),
        })
    } else {
        None
    };
    let cue_boundaries = boundaries::legacy(&records);
    let conventional = records
        .iter()
        .all(|r| r.integer().is_some() && r.separated && !r.normalized);
    let record_framing = (!conventional && cue_boundaries.is_none()).then(|| {
        let mut hash = Sha256::new();
        hash.update(b"tvmatch-provider-srt-record-framing-v2\0");
        hash.update((original_cues as u32).to_be_bytes());
        for r in &records {
            for n in [r.header_line, r.timing_line, r.body_end] {
                hash.update((n as u32).to_be_bytes());
            }
            let label = r.label_text.as_deref().unwrap_or("");
            hash.update((label.len() as u32).to_be_bytes());
            hash.update(label.as_bytes());
        }
        RecordFraming {
            policy: "provider-srt-record-framing-v2".into(),
            original_cues,
            unindexed_cues: records.iter().filter(|r| r.label.is_none()).count(),
            missing_separators: records.iter().filter(|r| !r.separated).count(),
            fractional_labels: records
                .iter()
                .filter(|r| r.label_text.as_ref().is_some_and(|s| s.contains('.')))
                .count(),
            normalized_timings: records.iter().filter(|r| r.normalized).count(),
            layout_sha256: format!("{:x}", hash.finalize()),
        }
    });
    let renumbered_cues = records
        .iter()
        .enumerate()
        .filter(|(i, r)| r.integer() != Some((*i + 1) as u32))
        .count();
    let cue_numbering = (conventional && renumbered_cues > 0).then(|| {
        let mut hash = Sha256::new();
        hash.update(b"tvmatch-provider-gapped-cue-numbering-v1\0");
        hash.update((original_cues as u32).to_be_bytes());
        for r in &records {
            hash.update(r.integer().unwrap().to_be_bytes());
        }
        CueNumbering {
            policy: "provider-gapped-cue-numbering-v1".into(),
            original_cues,
            renumbered_cues,
            indices_sha256: format!("{:x}", hash.finalize()),
        }
    });
    // Framing/numbering above describe every original record. MissingText is
    // then omitted explicitly; zero-duration provenance describes the remaining
    // nonempty records. Files without omissions keep their old provenance exactly.
    let mut missing_hash = Sha256::new();
    missing_hash.update(b"tvmatch-provider-skip-missing-text-v1\0");
    missing_hash.update((original_cues as u32).to_be_bytes());
    let records = records
        .into_iter()
        .enumerate()
        .filter_map(|(i, record)| {
            let keep = !empty.contains(&i);
            missing_hash.update(((i + 1) as u32).to_be_bytes());
            missing_hash.update([u8::from(keep)]);
            keep.then_some(record)
        })
        .collect::<Vec<_>>();
    let missing_text = (!empty.is_empty()).then(|| MissingText {
        policy: "provider-skip-missing-text-v1".into(),
        original_cues,
        skipped_cues: empty.len(),
        retained_cues: records.len(),
        selection_sha256: format!("{:x}", missing_hash.finalize()),
    });
    let original_cues = records.len();
    let mut derivative = String::new();
    let mut hash = Sha256::new();
    let mut retained = Vec::new();
    hash.update(b"tvmatch-provider-skip-zero-duration-v1\0");
    hash.update((original_cues as u32).to_be_bytes());
    for (i, record) in records.into_iter().enumerate() {
        let cue = record.cue;
        let keep = cue.end_ms > cue.start_ms;
        hash.update(((i + 1) as u32).to_be_bytes());
        hash.update([u8::from(keep)]);
        // Validate even discarded captions through the public constructor. A
        // synthetic duration is ONLY a validation probe, never retained evidence.
        let probe = Cue {
            start_ms: 0,
            end_ms: 1,
            text: cue.text.clone(),
        };
        Transcript::from_cues(vec![probe])
            .map_err(|_| fail("provider record import: InvalidText"))?;
        if !keep {
            continue;
        }
        writeln!(
            &mut derivative,
            "{}\n{} --> {}\n{}\n",
            retained.len() + 1,
            stamp(cue.start_ms),
            stamp(cue.end_ms),
            cue.text
        )
        .map_err(|_| fail("provider record derivative serialization failed"))?;
        if derivative.len() > MAX_SRT_BYTES {
            return Err(fail("provider record derivative byte cap exceeded"));
        }
        retained.push(cue);
    }
    if retained.is_empty() {
        return Err(fail(
            "no positive-duration reference cues remain after missing-text/zero-duration omission",
        ));
    }
    let retained_cues = retained.len();
    let skipped_cues = original_cues - retained_cues;
    let zero_duration = (skipped_cues > 0).then(|| ZeroDuration {
        policy: "provider-skip-zero-duration-v1".into(),
        original_cues,
        skipped_cues,
        retained_cues,
        selection_sha256: format!("{:x}", hash.finalize()),
    });
    Ok(Prepared {
        text: derivative,
        retained,
        zero_duration,
        missing_text,
        caption_paragraphs,
        source_layout,
        cue_numbering,
        cue_boundaries,
        record_framing,
        caption_normalization,
    })
}
