use super::*;
const ZERO: &str = "1\n00:00:00,000 --> 00:00:00,000\nSkipped zero display.\n\n2\n00:00:01,000 --> 00:00:02,000\nRetained original caption.\n\n3\n99:59:59,999 --> 99:59:59,999\nSkipped maximum timestamp.\n";
#[test]
fn zero_duration_omission_preserves_raw_and_retained_semantics_not_public_parser_leniency() {
    assert_eq!(
        Transcript::parse(ZERO).unwrap_err().kind,
        crate::srt::ParseErrorKind::InvalidTiming
    );
    let prepared = transcript(ZERO.as_bytes()).unwrap();
    let cues = prepared.transcript.cues();
    assert_eq!(cues.len(), 1);
    assert_eq!(
        (cues[0].start_ms, cues[0].end_ms, cues[0].text.as_str()),
        (1000, 2000, "Retained original caption.")
    );
    let policy = prepared.zero_duration.unwrap();
    assert_eq!(policy.policy, "provider-skip-zero-duration-v1");
    assert_eq!(
        (
            policy.original_cues,
            policy.skipped_cues,
            policy.retained_cues
        ),
        (3, 2, 1)
    );
    assert_eq!(policy.selection_sha256.len(), 64);
    assert!(prepared.cue_numbering.is_none());
    assert!(prepared.cue_ordering.is_none());
    assert_eq!(
        transcript(ZERO.as_bytes()).unwrap().zero_duration,
        Some(policy)
    );
}
#[test]
fn gapped_indices_keep_every_existing_timed_caption_and_do_not_create_missing_cues() {
    let raw = "1\n00:00:01,000 --> 00:00:02,000\nFirst.\n\n3\n00:00:03,000 --> 00:00:04,000\nSecond.\n\n8\n00:00:05,000 --> 00:00:06,000\nThird.\n";
    assert_eq!(
        Transcript::parse(raw).unwrap_err().kind,
        crate::srt::ParseErrorKind::InvalidIndex
    );
    let prepared = transcript(raw.as_bytes()).unwrap();
    assert!(prepared.zero_duration.is_none());
    let policy = prepared.cue_numbering.unwrap();
    assert_eq!(policy.policy, "provider-gapped-cue-numbering-v1");
    assert_eq!((policy.original_cues, policy.renumbered_cues), (3, 2));
    assert_eq!(
        prepared
            .transcript
            .cues()
            .iter()
            .map(|c| (c.start_ms, c.end_ms, c.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (1000, 2000, "First."),
            (3000, 4000, "Second."),
            (5000, 6000, "Third.")
        ]
    );
    assert_eq!(
        transcript(raw.as_bytes()).unwrap().cue_numbering,
        Some(policy)
    );
    for bad in [
        raw.replacen("1\n", "2\n", 1),
        raw.replace("\n\n3\n", "\n\n1\n"),
        raw.replace("\n\n8\n", "\n\n2\n"),
        raw.replace("\n\n3\n", "\n\n4294967296\n"),
    ] {
        assert!(transcript(bad.as_bytes()).is_err());
    }
}
#[test]
fn numbering_omission_caption_and_ordering_policies_compose_without_pooling_or_deduplication() {
    let raw = "1\n00:00:00,000 --> 00:00:00,000\nSkipped.\n\n3\n00:00:05,000 --> 00:00:06,000\nLater\u{0092}caption.\n\n8\n00:00:01,000 --> 00:00:02,000\nRepeated exact caption.\n\n9\n00:00:01,000 --> 00:00:02,000\nRepeated exact caption.\n";
    let prepared = transcript(raw.as_bytes()).unwrap();
    assert_eq!(prepared.cue_numbering.unwrap().renumbered_cues, 3);
    assert_eq!(prepared.zero_duration.unwrap().skipped_cues, 1);
    assert_eq!(prepared.caption_normalization.unwrap().replacements, 1);
    assert!(prepared.cue_ordering.is_some());
    assert_eq!(
        prepared
            .transcript
            .cues()
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        [
            "Repeated exact caption.",
            "Repeated exact caption.",
            "Later\u{fffd}caption."
        ]
    );
}
#[test]
fn omission_cannot_hide_controls_bad_timestamps_negative_durations_or_ambiguous_structure() {
    for bad in [
        ZERO.replace("Skipped zero display.", "Forbidden\u{0000}caption."),
        ZERO.replace("\n\n2\n", "\n\u{000c}\n2\n"),
        ZERO.replace(
            "00:00:01,000 --> 00:00:02,000",
            "00:00:02,000 --> 00:00:01,000",
        ),
        ZERO.replace("00:00:01,000", "00:00:01.0000"),
        ZERO.replace("\n\n2\n", "\n7.5\n"),
        ZERO.replace(
            "Skipped zero display.",
            &"x".repeat(crate::srt::MAX_CUE_BYTES + 1),
        ),
        "1\n00:00:00,000 --> 00:00:00,000\nOnly zero cue.\n".to_owned(),
    ] {
        assert!(transcript(bad.as_bytes()).is_err());
    }
    let mut many = String::new();
    for n in 1..=crate::srt::MAX_CUES {
        many.push_str(&format!("{n}\n00:00:00,000 --> 00:00:00,000\nZero.\n\n"));
    }
    many.push_str(&format!(
        "{}\n00:00:01,000 --> 00:00:02,000\nPositive.\n",
        crate::srt::MAX_CUES + 1
    ));
    assert!(transcript(many.as_bytes()).is_err());
    assert!(transcript(&vec![b' '; crate::srt::MAX_SRT_BYTES + 1]).is_err());
}
#[test]
fn offline_record_recovery_preserves_raw_and_checks_every_policy_field_on_reopen() {
    let raw = ZERO
        .replace("\n\n2\n", "\n\n4\n")
        .replace("\n\n3\n", "\n\n7\n");
    let root = temp();
    let mut m = manifest();
    m.scope = scope();
    m.selected.truncate(1);
    let cache = Cache::open(&root).unwrap();
    cache.freeze(&m).unwrap();
    cache.reserve(&m.selected[0]).unwrap();
    let partial = cache.stage(&m.selected[0], raw.as_bytes()).unwrap();
    drop(cache);
    let cache = Cache::open(&root).unwrap();
    let mut offline = Mock::new(None);
    acquire(&cache, &m, &mut offline).unwrap();
    let entry = partial.with_extension("");
    let path = entry.join("provenance.json");
    let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(original["zero_duration"]["skipped_cues"], 2);
    assert_eq!(original["cue_numbering"]["renumbered_cues"], 2);
    assert_eq!(fs::read(entry.join("content.srt")).unwrap(), raw.as_bytes());
    for (policy, fields) in [
        (
            "zero_duration",
            vec![
                "policy",
                "original_cues",
                "skipped_cues",
                "retained_cues",
                "selection_sha256",
            ],
        ),
        (
            "cue_numbering",
            vec![
                "policy",
                "original_cues",
                "renumbered_cues",
                "indices_sha256",
            ],
        ),
    ] {
        for field in fields {
            let mut changed = original.clone();
            changed[policy][field] = if field == "policy" || field.ends_with("sha256") {
                json!("wrong")
            } else {
                json!(999)
            };
            fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
            assert!(cache.references(&m).is_err());
            assert!(acquire(&cache, &m, &mut offline).is_err());
            assert_eq!((offline.gets, offline.posts, offline.contents), (0, 0, 0));
        }
        let mut changed = original.clone();
        changed.as_object_mut().unwrap().remove(policy);
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(cache.references(&m).is_err());
        assert!(acquire(&cache, &m, &mut offline).is_err());
        assert_eq!((offline.gets, offline.posts, offline.contents), (0, 0, 0));
    }
    fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    drop(cache);
    let cache = Cache::open(&root).unwrap();
    assert_eq!(references(&cache, &m.scope, true).unwrap().len(), 1);
    assert_eq!((offline.gets, offline.posts, offline.contents), (0, 0, 0));
    assert_eq!(fs::read(entry.join("content.srt")).unwrap(), raw.as_bytes());
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
