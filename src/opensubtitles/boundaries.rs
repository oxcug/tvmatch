//! Legacy provenance for the previously supported embedded-unindexed grammar.
//! Framing now happens once in records::parser; this only preserves cache identity.
use super::records::parser::Record;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CueBoundaries {
    pub(super) policy: String,
    pub(super) indexed_cues: usize,
    pub(super) unindexed_cues: usize,
    pub(super) cue_count: usize,
    pub(super) mapping_sha256: String,
}
pub(super) fn legacy(records: &[Record]) -> Option<CueBoundaries> {
    let mut indexed = 0;
    let mut inserted = 0;
    for (i, r) in records.iter().enumerate() {
        if r.normalized || r.cue.end_ms <= r.cue.start_ms || r.cue.text.contains(" --> ") {
            return None;
        }
        match r.integer() {
            Some(n) if n == indexed + 1 && r.separated => {
                indexed = n;
            }
            None if r.label.is_none()
                && !r.separated
                && i > 0
                && i + 1 < records.len()
                && records[i - 1].integer() == Some(indexed)
                && records[i + 1].integer() == Some(indexed + 1)
                && records[i + 1].separated =>
            {
                if records[i - 1]
                    .cue
                    .text
                    .lines()
                    .last()?
                    .trim()
                    .bytes()
                    .all(|b| b.is_ascii_digit())
                {
                    return None;
                }
                inserted += 1;
            }
            _ => return None,
        }
    }
    if inserted == 0 {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update(b"tvmatch-provider-unindexed-cue-v1\0");
    hash.update((records.len() as u32).to_be_bytes());
    for r in records {
        hash.update(r.integer().unwrap_or(0).to_be_bytes());
    }
    Some(CueBoundaries {
        policy: "provider-unindexed-cue-v1".into(),
        indexed_cues: indexed as usize,
        unindexed_cues: inserted,
        cue_count: records.len(),
        mapping_sha256: format!("{:x}", hash.finalize()),
    })
}
