use tvmatch::{
    EPISODE_PAIR_LIMITS, Index, MatchOutcome, PairLimits, Reference, ReferenceId,
    srt::{Cue, Transcript},
};
fn transcript(count: usize) -> Transcript {
    Transcript::from_cues(
        (0..count)
            .map(|i| Cue {
                start_ms: i as u64 * 10_000,
                end_ms: i as u64 * 10_000 + 1000,
                text: format!("original{i} synthetic{i} dialogue{i} evidence{i}"),
            })
            .collect(),
    )
    .unwrap()
}
#[test]
fn default_cap_fails_configured_episode_matches_and_exhaustion_remains_error() {
    let q = transcript(129);
    let index = Index::build(vec![
        Reference::new(
            ReferenceId::new("original", "episode").unwrap(),
            "Original episode",
            "Original synthetic fixture",
            q.clone(),
        )
        .unwrap(),
    ])
    .unwrap();
    assert!(index.match_query(&q).is_err());
    let MatchOutcome::Identified { best, .. } = index
        .match_query_with_limits(&q, EPISODE_PAIR_LIMITS)
        .unwrap()
    else {
        panic!("expected independent ordered anchors")
    };
    assert_eq!(best.score, 129);
    for limits in [
        PairLimits {
            per_reference: 128,
            total: 4096,
        },
        PairLimits {
            per_reference: 1024,
            total: 128,
        },
        PairLimits {
            per_reference: 0,
            total: 1,
        },
        PairLimits {
            per_reference: 1025,
            total: 4096,
        },
        PairLimits {
            per_reference: 1024,
            total: 4097,
        },
    ] {
        assert!(index.match_query_with_limits(&q, limits).is_err());
    }
    let huge = transcript(1025);
    let index = Index::build(vec![
        Reference::new(
            ReferenceId::new("original", "large").unwrap(),
            "Original large episode",
            "Original synthetic fixture",
            huge.clone(),
        )
        .unwrap(),
    ])
    .unwrap();
    assert!(
        index
            .match_query_with_limits(&huge, EPISODE_PAIR_LIMITS)
            .is_err()
    );
}
