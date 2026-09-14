//! Provider-only cue ordering. The public SRT parser stays strict.
use super::{Result, fail};
use crate::srt::{Cue, MAX_CUES, MAX_SRT_BYTES, ParseErrorKind, Transcript};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CueOrdering {
    pub(super) policy: String,
    pub(super) cue_count: usize,
    pub(super) moved_cues: usize,
    /// SHA256(domain, cue count as u32 BE, original 1-based indices in derivative order as u32 BE).
    pub(super) permutation_sha256: String,
}
fn invalid(error: impl std::fmt::Display) -> super::Failure {
    fail(&format!("reference is not valid bounded SRT: {error}"))
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
fn same(a: &Cue, b: &Cue) -> bool {
    a.start_ms == b.start_ms && a.end_ms == b.end_ms && a.text == b.text
}
pub(super) fn parse(text: &str) -> Result<(Transcript, Option<CueOrdering>)> {
    match Transcript::parse(text) {
        Ok(parsed) => return Ok((parsed, None)),
        Err(error) if error.kind == ParseErrorKind::InvalidTiming => (),
        Err(error) => return Err(invalid(error)),
    }
    // Validate original indices globally and each entire cue with the strict parser.
    // This defers ONLY between-cue chronology; durations, controls, separators,
    // timestamps and content bounds cannot be repaired by sorting.
    let lines = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .lines()
        .collect::<Vec<_>>();
    let mut original = Vec::new();
    let mut offset = 0;
    for block in lines.split(|line| line.trim().is_empty()) {
        let start = offset;
        offset += block.len() + 1;
        if block.is_empty() {
            continue;
        }
        if original.len() == MAX_CUES {
            return Err(invalid("cue count exceeds bound"));
        }
        let index = block[0].trim();
        if index.is_empty()
            || !index.bytes().all(|b| b.is_ascii_digit())
            || index.parse::<usize>().ok() != Some(original.len() + 1)
        {
            return Err(invalid("invalid original cue index; ordering not applied"));
        }
        let single = format!("1\n{}\n", block[1..].join("\n"));
        let parsed = Transcript::parse(&single).map_err(|mut error| {
            error.line += start;
            invalid(error)
        })?;
        if parsed.cues().len() != 1 {
            return Err(invalid("cue boundary changed"));
        }
        original.push(parsed.cues()[0].clone());
    }
    if !original.windows(2).any(|w| w[0].start_ms > w[1].start_ms) {
        return Err(invalid("no decreasing starts eligible for cue ordering"));
    }
    let mut order = (0..original.len()).collect::<Vec<_>>();
    order.sort_by_key(|&i| original[i].start_ms); // Stable for equal-start records, including duplicates.
    let moved_cues = order
        .iter()
        .enumerate()
        .filter(|&(new, old)| new != *old)
        .count();
    let mut inverse = vec![usize::MAX; original.len()];
    let mut derivative = String::new();
    let mut digest = Sha256::new();
    digest.update(b"tvmatch-provider-stable-cue-order-v1\0");
    digest.update((original.len() as u32).to_be_bytes());
    for (new, &old) in order.iter().enumerate() {
        if inverse[old] != usize::MAX {
            return Err(invalid("ordering is not one-to-one"));
        }
        inverse[old] = new;
        digest.update(((old + 1) as u32).to_be_bytes());
        let cue = &original[old];
        writeln!(
            &mut derivative,
            "{}\n{} --> {}\n{}\n",
            new + 1,
            stamp(cue.start_ms),
            stamp(cue.end_ms),
            cue.text
        )
        .map_err(invalid)?;
        if derivative.len() > MAX_SRT_BYTES {
            return Err(invalid("ordered derivative exceeds byte bound"));
        }
    }
    let parsed = Transcript::parse(&derivative).map_err(invalid)?;
    if parsed.cues().len() != original.len()
        || original.iter().enumerate().any(|(old, cue)| {
            parsed
                .cues()
                .get(inverse[old])
                .is_none_or(|actual| !same(cue, actual))
        })
    {
        return Err(invalid("ordered derivative semantic roundtrip failed"));
    }
    Ok((
        parsed,
        Some(CueOrdering {
            policy: "provider-stable-cue-order-v1".into(),
            cue_count: original.len(),
            moved_cues,
            permutation_sha256: format!("{:x}", digest.finalize()),
        }),
    ))
}
