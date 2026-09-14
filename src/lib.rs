//! Offline, exact-shingle transcript evidence. Scores are uncalibrated support counts.
//! Optional native MKV/MP4 subtitles are container evidence, never audio verification.
//! No inferred labels or external processes. The default CLI acquires missing references
//! through OpenSubtitles; the minimal matching library is offline.
pub mod media;
pub mod paths;
/// Strict public IMDb title identity; no numeric aliases.
pub fn imdb_number(id: &str) -> Result<u32, std::io::Error> {
    let digits = id.strip_prefix("tt").unwrap_or("");
    if digits.is_empty() || digits.len() > 10 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(std::io::Error::other(
            "IMDb ID must be tt followed by bounded decimal digits",
        ));
    }
    digits
        .parse::<u32>()
        .ok()
        .filter(|n| *n > 0 && *n <= i32::MAX as u32)
        .ok_or_else(|| std::io::Error::other("IMDb ID outside bounds"))
}

#[cfg(feature = "opensubtitles")]
pub mod opensubtitles;
pub mod srt;

use srt::Transcript;
use std::{collections::BTreeMap, error::Error, fmt};

pub const MAX_REFERENCES: usize = 32;
pub const MAX_WORDS: usize = 32_768;
pub const MAX_WORD_BYTES: usize = 128;
pub const MAX_INDEX_SHINGLES: usize = 100_000;
pub const MAX_PAIRS_PER_REFERENCE: usize = 128;
pub const MAX_TOTAL_PAIRS: usize = 512;
/// Explicit finite episode profile; default matching limits remain 128/512.
pub const EPISODE_PAIR_LIMITS: PairLimits = PairLimits {
    per_reference: 1024,
    total: 4096,
};
#[derive(Clone, Copy, Debug)]
pub struct PairLimits {
    pub per_reference: usize,
    pub total: usize,
}
impl Default for PairLimits {
    fn default() -> Self {
        Self {
            per_reference: MAX_PAIRS_PER_REFERENCE,
            total: MAX_TOTAL_PAIRS,
        }
    }
}
pub const MIN_SEPARATION_MS: u64 = 8_000;
pub const MAX_OFFSET_SPREAD_MS: i64 = 2_000;
pub const MIN_SUPPORT: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    InvalidMetadata(&'static str),
    DuplicateReferenceId(ReferenceId),
    LimitExceeded(&'static str),
    NoReferences,
}
impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl Error for EngineError {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReferenceId {
    namespace: String,
    value: String,
}
impl ReferenceId {
    pub fn new(namespace: &str, value: &str) -> Result<Self, EngineError> {
        if !valid_metadata(namespace, 64)
            || !namespace
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || !valid_metadata(value, 256)
        {
            return Err(EngineError::InvalidMetadata("reference ID"));
        }
        Ok(Self {
            namespace: namespace.into(),
            value: value.into(),
        })
    }
    pub fn namespace(&self) -> &str {
        &self.namespace
    }
    pub fn value(&self) -> &str {
        &self.value
    }
}
impl fmt::Display for ReferenceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.namespace, self.value)
    }
}

#[derive(Debug, Clone)]
pub struct ReferenceLabel {
    pub id: ReferenceId,
    pub display_name: String,
    /// User assertion of origin/rights/version, not verified by this engine.
    pub provenance: String,
}

#[derive(Debug, Clone)]
pub struct Reference {
    label: ReferenceLabel,
    transcript: Transcript,
}
impl Reference {
    pub fn new(
        id: ReferenceId,
        display_name: &str,
        provenance: &str,
        transcript: Transcript,
    ) -> Result<Self, EngineError> {
        if !valid_metadata(display_name, 1024) || !valid_metadata(provenance, 1024) {
            return Err(EngineError::InvalidMetadata("display name or provenance"));
        }
        Ok(Self {
            label: ReferenceLabel {
                id,
                display_name: display_name.into(),
                provenance: provenance.into(),
            },
            transcript,
        })
    }
}

fn valid_metadata(value: &str, max_bytes: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectionReason {
    NoDistinctiveOverlap,
    InsufficientSeparatedSupport,
    CompetingEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimestampEvidence {
    pub query_start_ms: u64,
    pub reference_start_ms: u64,
    pub distinct_shingles: usize,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub reference: ReferenceLabel,
    /// Uncalibrated count of independent, separated cue pairs; NOT a probability.
    pub score: usize,
    pub evidence: Vec<TimestampEvidence>,
    pub distinct_shingles: usize,
    /// Supported cues / all query cues (including unusable cues).
    pub query_cue_coverage: f64,
    /// Reference time minus query time, midpoint of observed offset range.
    pub offset_ms: i64,
    pub offset_spread_ms: u64,
    pub query_span_ms: u64,
    pub reference_span_ms: u64,
    pub rejection_reasons: Vec<RejectionReason>,
}

#[derive(Debug, Clone)]
pub enum MatchOutcome {
    Identified {
        best: Candidate,
        competing: Vec<Candidate>,
    },
    Ambiguous {
        candidates: Vec<Candidate>,
        reason: RejectionReason,
    },
    Unknown {
        candidates: Vec<Candidate>,
        reasons: Vec<RejectionReason>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub references: usize,
    pub distinctive_shingles: usize,
    pub shared_shingles_suppressed: usize,
    pub repeated_shingles_suppressed: usize,
}

#[derive(Debug, Clone, Copy)]
struct Posting {
    reference: usize,
    cue: usize,
}

/// An in-memory pack. Shingles shared by references or repeated within a reference
/// are unusable. Duplicate content under different IDs therefore cannot identify either.
pub struct Index {
    references: Vec<Reference>,
    postings: BTreeMap<String, Posting>,
    stats: IndexStats,
}

impl Index {
    pub fn build(references: Vec<Reference>) -> Result<Self, EngineError> {
        Self::build_with_reference_limit(references, MAX_REFERENCES)
    }
    /// Additive season-sized admission; global shingle suppression and all work budgets remain intact.
    pub fn build_with_reference_limit(
        mut references: Vec<Reference>,
        limit: usize,
    ) -> Result<Self, EngineError> {
        if limit == 0 || limit > 1000 {
            return Err(EngineError::LimitExceeded("invalid reference limit"));
        }
        if references.is_empty() {
            return Err(EngineError::NoReferences);
        }
        if references.len() > limit {
            return Err(EngineError::LimitExceeded("reference count"));
        }
        references.sort_by(|a, b| a.label.id.cmp(&b.label.id));
        for pair in references.windows(2) {
            if pair[0].label.id == pair[1].label.id {
                return Err(EngineError::DuplicateReferenceId(pair[0].label.id.clone()));
            }
        }
        let mut all: BTreeMap<String, (usize, Option<Posting>)> = BTreeMap::new();
        for (reference, item) in references.iter().enumerate() {
            for (shingle, cue) in shingles(&item.transcript)? {
                if let Some((owners, posting)) = all.get_mut(&shingle) {
                    *owners += 1;
                    *posting = None;
                } else {
                    if all.len() == MAX_INDEX_SHINGLES {
                        return Err(EngineError::LimitExceeded("index shingles"));
                    }
                    all.insert(shingle, (1, cue.map(|cue| Posting { reference, cue })));
                }
            }
        }
        let mut stats = IndexStats {
            references: references.len(),
            ..IndexStats::default()
        };
        let mut postings = BTreeMap::new();
        for (shingle, (owners, posting)) in all {
            if owners > 1 {
                stats.shared_shingles_suppressed += 1;
            } else if let Some(posting) = posting {
                postings.insert(shingle, posting);
            } else {
                stats.repeated_shingles_suppressed += 1;
            }
        }
        stats.distinctive_shingles = postings.len();
        Ok(Self {
            references,
            postings,
            stats,
        })
    }

    pub fn stats(&self) -> &IndexStats {
        &self.stats
    }

    pub fn match_query(&self, query: &Transcript) -> Result<MatchOutcome, EngineError> {
        self.match_query_with_limits(query, PairLimits::default())
    }

    /// Same evidence rules with caller-selected work bounds; no truncation or retry.
    pub fn match_query_with_limits(
        &self,
        query: &Transcript,
        limits: PairLimits,
    ) -> Result<MatchOutcome, EngineError> {
        if limits.per_reference == 0
            || limits.per_reference > EPISODE_PAIR_LIMITS.per_reference
            || limits.total == 0
            || limits.total > EPISODE_PAIR_LIMITS.total
        {
            return Err(EngineError::LimitExceeded("invalid candidate pair limits"));
        }
        let mut pairs = vec![BTreeMap::<(usize, usize), usize>::new(); self.references.len()];
        let mut total_pairs = 0;
        for (shingle, cue) in shingles(query)? {
            if let (Some(cue), Some(posting)) = (cue, self.postings.get(&shingle)) {
                let map = &mut pairs[posting.reference];
                let key = (cue, posting.cue);
                if !map.contains_key(&key) {
                    if map.len() == limits.per_reference || total_pairs == limits.total {
                        return Err(EngineError::LimitExceeded("query candidate cue pairs"));
                    }
                    total_pairs += 1;
                }
                *map.entry(key).or_default() += 1;
            }
        }
        let mut candidates = Vec::new();
        for (reference, pairs) in self.references.iter().zip(pairs) {
            // Multiple overlapping shingles in one cue are one anchor, not independent votes.
            let anchors: Vec<_> = pairs
                .into_iter()
                .filter(|(_, support)| *support >= 2)
                .map(|((q, r), support)| Anchor {
                    q: query.cues()[q].start_ms,
                    r: reference.transcript.cues()[r].start_ms,
                    support,
                })
                .collect();
            let aligned = align(&anchors);
            if aligned.is_empty() {
                continue;
            }
            let min_offset = aligned.iter().map(Anchor::offset).min().unwrap();
            let max_offset = aligned.iter().map(Anchor::offset).max().unwrap();
            let first = aligned.first().unwrap();
            let last = aligned.last().unwrap();
            let score = aligned.len();
            candidates.push(Candidate {
                reference: reference.label.clone(),
                score,
                distinct_shingles: aligned.iter().map(|a| a.support).sum(),
                query_cue_coverage: score as f64 / query.cues().len() as f64,
                offset_ms: min_offset + (max_offset - min_offset) / 2,
                offset_spread_ms: (max_offset - min_offset) as u64,
                query_span_ms: last.q - first.q,
                reference_span_ms: last.r - first.r,
                evidence: aligned
                    .iter()
                    .map(|a| TimestampEvidence {
                        query_start_ms: a.q,
                        reference_start_ms: a.r,
                        distinct_shingles: a.support,
                    })
                    .collect(),
                rejection_reasons: if score >= MIN_SUPPORT {
                    vec![]
                } else {
                    vec![RejectionReason::InsufficientSeparatedSupport]
                },
            });
        }
        candidates.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(a.reference.id.cmp(&b.reference.id))
        });
        let Some(best) = candidates.first() else {
            return Ok(MatchOutcome::Unknown {
                candidates,
                reasons: vec![RejectionReason::NoDistinctiveOverlap],
            });
        };
        if best.score < MIN_SUPPORT {
            return Ok(MatchOutcome::Unknown {
                candidates,
                reasons: vec![RejectionReason::InsufficientSeparatedSupport],
            });
        }
        if candidates.get(1).is_some_and(|runner| {
            runner.score >= MIN_SUPPORT
                && (runner.score + 1 >= best.score || runner.score * 5 >= best.score * 4)
        }) {
            return Ok(MatchOutcome::Ambiguous {
                candidates,
                reason: RejectionReason::CompetingEvidence,
            });
        }
        let best = candidates.remove(0);
        Ok(MatchOutcome::Identified {
            best,
            competing: candidates,
        })
    }
}

// A repeated shingle is represented by None even if both occurrences are in one cue.
fn shingles(transcript: &Transcript) -> Result<BTreeMap<String, Option<usize>>, EngineError> {
    let mut result = BTreeMap::new();
    let mut total_words = 0;
    for (cue, caption) in transcript.cues().iter().enumerate() {
        let mut words = Vec::new();
        for word in caption
            .text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
        {
            total_words += 1;
            if total_words > MAX_WORDS {
                return Err(EngineError::LimitExceeded("transcript words"));
            }
            // Unicode lowercase, not transliteration, stemming, NFC, or language segmentation.
            let word = word.to_lowercase();
            if word.len() > MAX_WORD_BYTES {
                return Err(EngineError::LimitExceeded("normalized word bytes"));
            }
            words.push(word);
        }
        for window in words.windows(3) {
            result
                .entry(window.join(" "))
                .and_modify(|value| *value = None)
                .or_insert(Some(cue));
        }
    }
    Ok(result)
}

#[derive(Debug, Clone, Copy)]
struct Anchor {
    q: u64,
    r: u64,
    support: usize,
}
impl Anchor {
    fn offset(&self) -> i64 {
        self.r as i64 - self.q as i64
    }
}

// Exhaust all observed offset-window starts, avoiding fixed-bin boundary failures.
// DP finds ordered, separated chains in each window. Pair limits bound worst-case work
// to at most caller total * per_reference^2 predecessor comparisons.
fn align(anchors: &[Anchor]) -> Vec<Anchor> {
    let mut offsets: Vec<_> = anchors.iter().map(Anchor::offset).collect();
    offsets.sort_unstable();
    offsets.dedup();
    let mut best = Vec::new();
    let mut best_rank = (0, 0);
    for lower in offsets {
        let window: Vec<_> = anchors
            .iter()
            .copied()
            .filter(|a| (lower..=lower + MAX_OFFSET_SPREAD_MS).contains(&a.offset()))
            .collect();
        let mut ranks = Vec::new();
        let mut previous = Vec::new();
        for (i, anchor) in window.iter().enumerate() {
            let mut rank = (1, anchor.support);
            let mut predecessor = None;
            for j in 0..i {
                if anchor.q >= window[j].q + MIN_SEPARATION_MS
                    && anchor.r >= window[j].r + MIN_SEPARATION_MS
                {
                    let (length, support) = ranks[j];
                    let proposed = (length + 1, support + anchor.support);
                    if proposed > rank {
                        rank = proposed;
                        predecessor = Some(j);
                    }
                }
            }
            ranks.push(rank);
            previous.push(predecessor);
            if rank > best_rank {
                best_rank = rank;
                best.clear();
                let mut current = Some(i);
                while let Some(j) = current {
                    best.push(window[j]);
                    current = previous[j];
                }
                best.reverse();
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_non_ascii_words_and_suppresses_repeated_shingles() {
        let transcript = Transcript::parse(
            "1\n00:00:00,000 --> 00:00:01,000\nÉTOILE, 東京 привет мир étoile 東京 привет\n",
        )
        .unwrap();
        let values = shingles(&transcript).unwrap();
        assert_eq!(values["étoile 東京 привет"], None);
        assert_eq!(values["東京 привет мир"], Some(0));
    }

    #[test]
    fn ids_and_metadata_are_explicit_and_bounded() {
        for (namespace, value) in [
            ("", "x"),
            ("a:b", "x"),
            ("a", ""),
            ("a", " x"),
            ("a", "x\n"),
        ] {
            assert!(ReferenceId::new(namespace, value).is_err());
        }
        let id = ReferenceId::new("demo", "edition:1").unwrap();
        assert_eq!(id.namespace(), "demo");
        assert_eq!(id.value(), "edition:1");
        assert_eq!(id.to_string(), "demo:edition:1");
    }
}
