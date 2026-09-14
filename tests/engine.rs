use tvmatch::{
    EngineError, Index, MAX_PAIRS_PER_REFERENCE, MAX_REFERENCES, MAX_WORDS, MatchOutcome,
    Reference, ReferenceId, RejectionReason, srt::Transcript,
};

const A: [&str; 4] = [
    "The copper telescope is growing tiny paper feathers",
    "Please keep those moonlight jars beneath the staircase",
    "Our patient comet has finally learned to whistle",
    "Tomorrow we shall knit a scarf for Saturn",
];
const B: [&str; 3] = [
    "A violet turnip has borrowed my waterproof notebook",
    "These sleepy radishes prefer their lullabies in emerald",
    "We planted seven umbrellas beside the marmalade fountain",
];
fn timestamp(ms: u64) -> String {
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}
fn transcript(lines: &[(&str, u64)]) -> Transcript {
    let text: String = lines
        .iter()
        .enumerate()
        .map(|(i, (text, time))| {
            format!(
                "{}\n{} --> {}\n{text}\n\n",
                i + 1,
                timestamp(*time),
                timestamp(time + 2000)
            )
        })
        .collect();
    Transcript::parse(&text).unwrap()
}
fn regular(lines: &[&str], offset: u64) -> Transcript {
    transcript(
        &lines
            .iter()
            .enumerate()
            .map(|(i, text)| (*text, offset + i as u64 * 12_000))
            .collect::<Vec<_>>(),
    )
}
fn reference(id: &str, transcript: Transcript) -> Reference {
    Reference::new(
        ReferenceId::new("synthetic", id).unwrap(),
        "Unverified display label",
        "Original synthetic test dialogue, v1",
        transcript,
    )
    .unwrap()
}
fn index() -> Index {
    Index::build(vec![
        reference("a", regular(&A, 60_000)),
        reference("b", regular(&B, 120_000)),
    ])
    .unwrap()
}
fn identified(outcome: MatchOutcome) -> tvmatch::Candidate {
    match outcome {
        MatchOutcome::Identified { best, .. } => best,
        other => panic!("expected Identified, got {other:?}"),
    }
}
fn unknown(outcome: MatchOutcome) {
    assert!(
        matches!(outcome, MatchOutcome::Unknown { .. }),
        "{outcome:?}"
    );
}

#[test]
fn positive_offset_alignment_exposes_provenance_support_and_timestamps() {
    let best = identified(index().match_query(&regular(&A[..3], 0)).unwrap());
    assert_eq!(best.reference.id.value(), "a");
    assert_eq!(
        best.reference.provenance,
        "Original synthetic test dialogue, v1"
    );
    assert_eq!(best.score, 3);
    assert_eq!(best.query_cue_coverage, 1.0);
    assert_eq!(best.offset_ms, 60_000);
    assert_eq!(best.offset_spread_ms, 0);
    assert_eq!(best.query_span_ms, 24_000);
    assert_eq!(best.reference_span_ms, 24_000);
    assert_eq!(best.evidence[1].query_start_ms, 12_000);
    assert_eq!(best.evidence[1].reference_start_ms, 72_000);
    assert!(best.distinct_shingles >= 6);
    assert!(best.rejection_reasons.is_empty());
}

#[test]
fn negative_offset_and_bin_boundary_jitter() {
    let query = transcript(&[(A[0], 120_999), (A[1], 133_001), (A[2], 144_500)]);
    let best = identified(index().match_query(&query).unwrap());
    assert_eq!(best.offset_ms, -60_751);
    assert_eq!(best.offset_spread_ms, 501);
    let query = transcript(&[(A[0], 59_999), (A[1], 72_001), (A[2], 84_000)]);
    let best = identified(index().match_query(&query).unwrap());
    assert_eq!(best.offset_ms, 0);
    assert_eq!(best.offset_spread_ms, 2);
}

#[test]
fn accepts_jitter_at_limit_but_rejects_inconsistent_offsets_and_rate_change() {
    let best = identified(
        index()
            .match_query(&transcript(&[(A[0], 0), (A[1], 13_000), (A[2], 26_000)]))
            .unwrap(),
    );
    assert_eq!(best.offset_spread_ms, 2000);
    unknown(
        index()
            .match_query(&transcript(&[(A[0], 0), (A[1], 15_000), (A[2], 30_000)]))
            .unwrap(),
    );
}

#[test]
fn unrelated_and_short_input_are_unknown() {
    unknown(
        index()
            .match_query(&regular(
                &[
                    "My bicycle bell tastes faintly of cinnamon",
                    "Someone folded this river into a blue envelope",
                    "Let us invite the quiet thunder to breakfast",
                ],
                0,
            ))
            .unwrap(),
    );
    unknown(
        index()
            .match_query(&regular(&["hello", "a b", "..."], 0))
            .unwrap(),
    );
    unknown(index().match_query(&regular(&A[..2], 0)).unwrap());
}

#[test]
fn shared_intro_only_is_never_strong_evidence() {
    let shared = [
        "Welcome aboard the little clockwork station",
        "Our opening lantern shines above every doorway",
        "Remember that tomorrow brings another curious journey",
    ];
    let mut left = shared.to_vec();
    left.extend(A);
    let mut right = shared.to_vec();
    right.extend(B);
    let pack = Index::build(vec![
        reference("a", regular(&left, 0)),
        reference("b", regular(&right, 0)),
    ])
    .unwrap();
    assert!(pack.stats().shared_shingles_suppressed > 0);
    unknown(pack.match_query(&regular(&shared, 0)).unwrap());
    // A shared intro cannot top up two real anchors to the required three.
    let query = [shared[0], A[0], A[1]];
    unknown(pack.match_query(&regular(&query, 0)).unwrap());
}

#[test]
fn identical_labeled_references_have_no_distinguishing_evidence() {
    let pack = Index::build(vec![
        reference("a", regular(&A, 0)),
        reference("duplicate", regular(&A, 0)),
    ])
    .unwrap();
    assert_eq!(pack.stats().distinctive_shingles, 0);
    unknown(pack.match_query(&regular(&A, 0)).unwrap());
}

#[test]
fn near_identical_references_with_only_one_unique_cue_remain_unidentifiable() {
    // Near-identical references still need enough independently distinctive cues.
    // Extra text in one upload does not supply three independent ordered anchors.
    let shared = [
        "The silver lantern waits beneath the bridge",
        "A distant messenger carries our sealed envelope",
        "We should return before the morning bells",
    ];
    let mut extended = shared.to_vec();
    extended.push("Only this final additional sentence distinguishes the upload");
    let index = Index::build(vec![
        reference("e1", regular(&shared, 0)),
        reference("e3", regular(&extended, 0)),
    ])
    .unwrap();
    unknown(index.match_query(&regular(&shared, 0)).unwrap());
    unknown(index.match_query(&regular(&extended, 0)).unwrap());
}
#[test]
fn repeated_phrase_cannot_manufacture_independent_support() {
    let repeated = [A[0]; 5];
    let pack = Index::build(vec![reference("repetition", regular(&repeated, 60_000))]).unwrap();
    assert!(pack.stats().repeated_shingles_suppressed > 0);
    unknown(pack.match_query(&regular(&repeated, 0)).unwrap());
    unknown(index().match_query(&regular(&repeated, 0)).unwrap());
    let within_cue = format!("{} {} {}", A[0], A[0], A[0]);
    unknown(index().match_query(&regular(&[&within_cue], 0)).unwrap());
}

#[test]
fn reversed_dialogue_adversarial_regression() {
    let reversed: Vec<_> = A.iter().rev().copied().collect();
    unknown(index().match_query(&regular(&reversed, 0)).unwrap());
    // Even compressed reversals inside the offset tolerance must fail ordered support.
    let pack = Index::build(vec![reference(
        "a",
        transcript(&[(A[0], 0), (A[1], 500), (A[2], 1000)]),
    )])
    .unwrap();
    unknown(
        pack.match_query(&transcript(&[(A[2], 0), (A[1], 500), (A[0], 1000)]))
            .unwrap(),
    );
}

#[test]
fn one_long_caption_or_adjacent_captions_are_not_independent() {
    let long = A.join(" ");
    let pack = Index::build(vec![reference("a", regular(&[&long], 60_000))]).unwrap();
    unknown(pack.match_query(&regular(&[&long], 0)).unwrap());
    let close = transcript(&[(A[0], 0), (A[1], 1000), (A[2], 2000)]);
    let pack = Index::build(vec![reference("a", close.clone())]).unwrap();
    unknown(pack.match_query(&close).unwrap());
}

#[test]
fn a_lexical_error_leaves_other_exact_shingles_usable() {
    let damaged = [
        "The silver telescope is growing tiny paper feathers",
        "Please store those moonlight jars beneath the staircase",
        "Our sleepy comet has finally learned to whistle",
    ];
    assert_eq!(
        identified(index().match_query(&regular(&damaged, 0)).unwrap()).score,
        3
    );
}

#[test]
fn unicode_case_punctuation_and_multiline_normalization_match() {
    let words = [
        "Étoile brillante garde notre étrange petit jardin",
        "東京 の 小鳥 は 静かな 青い 帽子 を 運ぶ",
        "Тихая луна хранит наши медные бумажные лодки",
    ];
    let pack = Index::build(vec![reference("unicode", regular(&words, 60_000))]).unwrap();
    let upper: Vec<_> = words
        .iter()
        .map(|line| line.to_uppercase().replace(' ', ",\n"))
        .collect();
    let query = regular(&upper.iter().map(String::as_str).collect::<Vec<_>>(), 0);
    assert_eq!(identified(pack.match_query(&query).unwrap()).score, 3);
}

#[test]
fn exact_ties_are_ambiguous_and_reference_order_is_deterministic() {
    let mut both = A[..3].to_vec();
    both.extend(B);
    let query = regular(&both, 0);
    for references in [
        vec![
            reference("a", regular(&A, 60_000)),
            reference("b", regular(&B, 120_000)),
        ],
        vec![
            reference("b", regular(&B, 120_000)),
            reference("a", regular(&A, 60_000)),
        ],
    ] {
        match Index::build(references)
            .unwrap()
            .match_query(&query)
            .unwrap()
        {
            MatchOutcome::Ambiguous { candidates, reason } => {
                assert_eq!(reason, RejectionReason::CompetingEvidence);
                assert_eq!(candidates[0].reference.id.value(), "a");
                assert_eq!(candidates[1].reference.id.value(), "b");
                assert_eq!(candidates[0].score, candidates[1].score);
                assert_eq!(candidates[0].query_cue_coverage, 0.5);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }
}

#[test]
fn near_ties_are_ambiguous_not_arbitrarily_selected() {
    let mut both = A.to_vec();
    both.extend(B);
    assert!(matches!(
        index().match_query(&regular(&both, 0)).unwrap(),
        MatchOutcome::Ambiguous { .. }
    ));
}

#[test]
fn reference_metadata_and_count_limits() {
    assert!(matches!(
        Index::build(vec![]),
        Err(EngineError::NoReferences)
    ));
    assert!(matches!(
        Index::build(vec![
            reference("same", regular(&A, 0)),
            reference("same", regular(&B, 0))
        ]),
        Err(EngineError::DuplicateReferenceId(_))
    ));
    let too_many = (0..MAX_REFERENCES + 1)
        .map(|i| reference(&i.to_string(), regular(&A, 0)))
        .collect();
    assert!(matches!(
        Index::build(too_many),
        Err(EngineError::LimitExceeded("reference count"))
    ));
    assert!(
        Reference::new(
            ReferenceId::new("test", "1").unwrap(),
            "name",
            "",
            regular(&A, 0)
        )
        .is_err()
    );
}

#[test]
fn word_and_pair_resource_limits_are_explicit_errors() {
    let long = "x".repeat(129);
    assert!(matches!(
        index().match_query(&regular(&[&long], 0)),
        Err(EngineError::LimitExceeded("normalized word bytes"))
    ));
    let text = "a ".repeat(400);
    let many_words = vec![text.as_str(); MAX_WORDS / 400 + 1];
    assert!(matches!(
        index().match_query(&regular(&many_words, 0)),
        Err(EngineError::LimitExceeded("transcript words"))
    ));
    let lines: Vec<_> = (0..MAX_PAIRS_PER_REFERENCE + 1)
        .map(|i| format!("first{i} second{i} third{i} fourth{i}"))
        .collect();
    let lines: Vec<_> = lines.iter().map(String::as_str).collect();
    let query = regular(&lines, 0);
    let pack = Index::build(vec![reference("many", query.clone())]).unwrap();
    assert!(matches!(
        pack.match_query(&query),
        Err(EngineError::LimitExceeded("query candidate cue pairs"))
    ));
}

#[test]
fn total_pair_limit_is_not_silent_truncation() {
    let mut all_lines = Vec::new();
    let mut references = Vec::new();
    for r in 0..5 {
        let lines: Vec<_> = (0..110)
            .map(|i| format!("first{r}x{i} second{r}x{i} third{r}x{i} fourth{r}x{i}"))
            .collect();
        references.push(reference(
            &r.to_string(),
            regular(&lines.iter().map(String::as_str).collect::<Vec<_>>(), 0),
        ));
        all_lines.extend(lines);
    }
    let pack = Index::build(references).unwrap();
    let query = regular(&all_lines.iter().map(String::as_str).collect::<Vec<_>>(), 0);
    assert!(matches!(
        pack.match_query(&query),
        Err(EngineError::LimitExceeded("query candidate cue pairs"))
    ));
}

#[test]
fn index_shingle_limit_is_enforced() {
    let mut references = Vec::new();
    for r in 0..4 {
        let lines: Vec<_> = (0..160)
            .map(|cue| {
                (0..200)
                    .map(|word| format!("r{r}c{cue}w{word}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        references.push(reference(
            &r.to_string(),
            regular(&lines.iter().map(String::as_str).collect::<Vec<_>>(), 0),
        ));
    }
    assert!(matches!(
        Index::build(references),
        Err(EngineError::LimitExceeded("index shingles"))
    ));
}
