use super::*;
const RAW: &str = "1\n00:00:01,000 --> 00:00:02,000\nFirst original caption.\n00:00:03,000 --> 00:00:04,000\nUnindexed original caption.\n\n2\n00:00:05,000 --> 00:00:06,000\nLast original caption.\n";
#[test]
fn unindexed_timed_cue_preserves_every_caption_timestamp_and_keeps_public_parser_strict() {
    assert_eq!(
        Transcript::parse(RAW).unwrap_err().kind,
        crate::srt::ParseErrorKind::MissingSeparator
    );
    let prepared = transcript(RAW.as_bytes()).unwrap();
    let cues = prepared.transcript.cues();
    assert_eq!(cues.len(), 3);
    assert_eq!(
        cues.iter()
            .map(|c| (c.start_ms, c.end_ms, c.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (1000, 2000, "First original caption."),
            (3000, 4000, "Unindexed original caption."),
            (5000, 6000, "Last original caption.")
        ]
    );
    let policy = prepared.cue_boundaries.unwrap();
    assert_eq!(policy.policy, "provider-unindexed-cue-v1");
    assert_eq!(
        (policy.indexed_cues, policy.unindexed_cues, policy.cue_count),
        (2, 1, 3)
    );
    assert_eq!(policy.mapping_sha256.len(), 64);
    assert!(prepared.cue_ordering.is_none());
    assert!(prepared.caption_normalization.is_none());
    assert_eq!(
        transcript(RAW.as_bytes()).unwrap().cue_boundaries,
        Some(policy)
    );
}
#[test]
fn boundary_recovery_rejects_ambiguous_headers_controls_and_invalid_timing() {
    for caption in ["First original caption.", "Unindexed original caption."] {
        let p = transcript(RAW.replace(caption, "").as_bytes()).unwrap();
        assert_eq!(p.missing_text.unwrap().skipped_cues, 1);
        assert_eq!(p.transcript.cues().len(), 2);
    }
    for bad in [
        RAW.replace("First original caption.", "First original caption.\n2"),
        RAW.replace("\n\n2\n", "\n\n1\n"),
        RAW.replace(
            "00:00:03,000 --> 00:00:04,000",
            "00:00:03,000 --> 00:00:02,000",
        ),
        RAW.replace("00:00:01,000 --> 00:00:02,000", "00:00:01,000 --> invalid"),
        RAW.replace("\n\n2\n", "\n\u{000c}\n2\n"),
        RAW.replace("First original caption.", "First\u{009c}caption"),
    ] {
        assert!(transcript(bad.as_bytes()).is_err());
    }
}
#[test]
fn new_c1_placeholder_is_explicit_limited_and_composes_with_ordering_and_boundaries() {
    let raw = RAW
        .replace(
            "First original caption.",
            "First\u{0092}original\u{009d}caption.",
        )
        .replace(
            "00:00:03,000 --> 00:00:04,000",
            "00:00:00,500 --> 00:00:00,900",
        );
    let prepared = transcript(raw.as_bytes()).unwrap();
    let policy = prepared.caption_normalization.unwrap();
    assert_eq!(policy.policy, "caption-c1-placeholders-v2");
    assert_eq!(policy.replacements, 2);
    assert_eq!(policy.codepoints.get("U+0092"), Some(&1));
    assert_eq!(policy.codepoints.get("U+009D"), Some(&1));
    assert_eq!(prepared.cue_boundaries.unwrap().unindexed_cues, 1);
    assert!(prepared.cue_ordering.is_some());
    assert_eq!(
        prepared.transcript.cues()[0].text,
        "Unindexed original caption."
    );
    assert_eq!(
        prepared.transcript.cues()[1].text,
        "First\u{fffd}original\u{fffd}caption."
    );
    for bad in [
        RAW.replace("First original caption.", " \u{0092} "),
        RAW.replace("First original caption.", "2\u{0092}"),
        RAW.replace("00:00:03,000", "00:00:03,00\u{0092}"),
        RAW.replacen("1\n", "1\u{0092}\n", 1),
    ] {
        assert!(transcript(bad.as_bytes()).is_err());
    }
}
#[test]
fn boundary_recovery_is_offline_and_all_derivative_metadata_is_reverified() {
    let root = temp();
    let cache = Cache::open(&root).unwrap();
    let mut m = manifest();
    m.scope = scope();
    m.selected.truncate(1);
    cache.freeze(&m).unwrap();
    cache.reserve(&m.selected[0]).unwrap();
    let raw = RAW.replace("Last original caption.", "Last\u{0092}original caption.");
    let partial = cache.stage(&m.selected[0], raw.as_bytes()).unwrap();
    drop(cache);
    let cache = Cache::open(&root).unwrap();
    let mut mock = Mock::new(None);
    acquire(&cache, &m, &mut mock).unwrap();
    assert_eq!((mock.posts, mock.gets, mock.contents), (0, 0, 0));
    let entry = partial.with_extension("");
    let path = entry.join("provenance.json");
    let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(original["cue_boundaries"]["unindexed_cues"], 1);
    assert_eq!(original["caption_normalization"]["codepoints"]["U+0092"], 1);
    assert_eq!(fs::read(entry.join("content.srt")).unwrap(), raw.as_bytes());
    assert_eq!(cache.references(&m).unwrap().len(), 1);
    for field in [
        "policy",
        "indexed_cues",
        "unindexed_cues",
        "cue_count",
        "mapping_sha256",
        "missing",
        "codepoints",
    ] {
        let mut changed = original.clone();
        match field {
            "missing" => {
                changed.as_object_mut().unwrap().remove("cue_boundaries");
                changed["format"] = json!("srt-utf8-raw-with-caption-derivative-v2");
            }
            "codepoints" => changed["caption_normalization"]["codepoints"]["U+0092"] = json!(2),
            "policy" | "mapping_sha256" => changed["cue_boundaries"][field] = json!("wrong"),
            _ => changed["cue_boundaries"][field] = json!(999),
        }
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(acquire(&cache, &m, &mut mock).is_err(), "{field}");
        assert_eq!((mock.posts, mock.gets, mock.contents), (0, 0, 0));
        assert_eq!(fs::read(entry.join("content.srt")).unwrap(), raw.as_bytes());
    }
    fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    drop(cache);
    let cache = Cache::open(&root).unwrap();
    assert_eq!(references(&cache, &m.scope, true).unwrap().len(), 1);
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
