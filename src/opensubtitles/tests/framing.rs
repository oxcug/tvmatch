use super::*;
// Original synthetic text combining independently validated framing defects.
const MIXED: &str = "1\n00:00:20,000 --> 00:00:21,000\nLater\u{0092}caption.\n1.5\n00:00:01,000 --> 00:00:002,000\nEarlier caption.\n00:00:03,000 --> 00:00:03,000\nZero caption, fully validated.\n\n2\n00:00:04,000-->00:00:05.000\nRepeated caption.\n\n4\n00:00:04,000 --> 00:00:05,000\nRepeated caption.\n";
#[test]
fn mixed_framing_numbering_padding_omission_caption_and_ordering_preserve_semantics() {
    assert!(Transcript::parse(MIXED).is_err());
    let p = transcript(MIXED.as_bytes()).unwrap();
    assert_eq!(
        p.transcript
            .cues()
            .iter()
            .map(|c| (c.start_ms, c.end_ms, c.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (1000, 2000, "Earlier caption."),
            (4000, 5000, "Repeated caption."),
            (4000, 5000, "Repeated caption."),
            (20000, 21000, "Later\u{fffd}caption.")
        ]
    );
    let f = p.record_framing.unwrap();
    assert_eq!(f.policy, "provider-srt-record-framing-v2");
    assert_eq!(
        (
            f.original_cues,
            f.unindexed_cues,
            f.missing_separators,
            f.fractional_labels,
            f.normalized_timings
        ),
        (5, 1, 2, 1, 2)
    );
    assert_eq!(p.zero_duration.unwrap().skipped_cues, 1);
    assert_eq!(p.caption_normalization.unwrap().replacements, 1);
    assert!(p.cue_ordering.is_some());
    assert!(p.cue_boundaries.is_none());
}
#[test]
fn record_repairs_compose_across_64_layout_combinations() {
    for mask in 0..64 {
        let sep = if mask & 1 != 0 { "" } else { "\n" };
        let index = if mask & 2 != 0 { "" } else { "2\n" };
        let last = if index.is_empty() { 2 } else { 3 } + if mask & 8 != 0 { 3 } else { 0 };
        let end = if mask & 4 != 0 {
            "00:00:01,000"
        } else {
            "00:00:02,000"
        };
        let start = if mask & 32 != 0 {
            "0:0:001.000"
        } else {
            "00:00:01,000"
        };
        let text = if mask & 16 != 0 {
            "Second\u{0092}caption."
        } else {
            "Second caption."
        };
        let raw = format!(
            "1\n00:00:05,000 --> 00:00:06,000\nFirst caption.\n{sep}{index}{start} --> {end}\n{text}\n\n{last}\n00:00:03,000 --> 00:00:04,000\nThird caption.\n"
        );
        let p = transcript(raw.as_bytes()).unwrap();
        let cues = p.transcript.cues();
        assert_eq!(cues.len(), if mask & 4 != 0 { 2 } else { 3 });
        if mask & 4 == 0 {
            assert_eq!((cues[0].start_ms, cues[0].end_ms), (1000, 2000));
            assert_eq!(cues[0].text, text.replace('\u{0092}', "\u{fffd}"));
        }
        assert_eq!(cues[cues.len() - 2].text, "Third caption.");
        assert_eq!(cues[cues.len() - 1].text, "First caption.");
        assert!(p.cue_ordering.is_some());
    }
}
#[test]
fn timestamp_spelling_is_numeric_not_guessed_precision_or_overflow() {
    for t in ["0:0:1.000", "000000:00:001,000", "00:00:01,000"] {
        let p =
            transcript(format!("1\n{t}-->00:00:02,000\nOriginal caption.\n").as_bytes()).unwrap();
        assert_eq!(p.transcript.cues()[0].start_ms, 1000);
    }
    for t in [
        "00:00:001,00",
        "00:00:001,0000",
        "00:00:060,000",
        "100:00:01,000",
        "0000000:00:01,000",
        "-00:00:01,000",
        "00:00:01e0,000",
    ] {
        assert!(
            transcript(format!("1\n{t} --> 00:00:02,000\nOriginal caption.\n").as_bytes()).is_err()
        );
    }
    for body in ["19.5", "123", "go --> home"] {
        let p =
            transcript(format!("1\n00:00:01,000 --> 00:00:02,000\n{body}\n").as_bytes()).unwrap();
        assert_eq!(p.transcript.cues()[0].text, body);
    }
}
#[test]
fn explicit_timing_records_can_be_unnumbered_repeated_and_end_at_eof() {
    let raw = "00:00:01,000 --> 00:00:02,000\nFirst\u{009d}caption.\n00:00:03,000 --> 00:00:04,000\nSecond caption.\n00:00:05,000 --> 00:00:06,000\nThird caption.";
    let p = transcript(raw.as_bytes()).unwrap();
    assert_eq!(p.transcript.cues().len(), 3);
    assert_eq!(p.record_framing.unwrap().unindexed_cues, 3);
    assert_eq!(p.caption_normalization.unwrap().replacements, 1);
    for raw in [
        MIXED.replace("1.5", "7.5"),
        MIXED.replace("1.5", "1.5\u{009d}"),
        MIXED.replace("1.5", "1.5\u{0092}"),
        MIXED.replace("1.5", "1.1234567890"),
        MIXED.replace("1.5", "4294967296"),
        MIXED.replace(
            "Zero caption, fully validated.",
            "Forbidden\u{000c}caption.",
        ),
        MIXED.replace(
            "00:00:03,000 --> 00:00:03,000",
            "-00:00:03,000 --> -00:00:03,000",
        ),
    ] {
        assert!(transcript(raw.as_bytes()).is_err());
    }
}
#[test]
fn framing_provenance_is_recomputed_offline_and_never_rewrites_raw_or_refetches() {
    let root = temp();
    let mut m = manifest();
    m.scope = scope();
    m.selected.truncate(1);
    let cache = Cache::open(&root).unwrap();
    cache.freeze(&m).unwrap();
    cache.reserve(&m.selected[0]).unwrap();
    let partial = cache.stage(&m.selected[0], MIXED.as_bytes()).unwrap();
    let mut offline = Mock::new(None);
    acquire(&cache, &m, &mut offline).unwrap();
    let entry = partial.with_extension("");
    let path = entry.join("provenance.json");
    let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(original["format"], "srt-utf8-raw-with-record-framing-v2");
    for field in [
        "policy",
        "original_cues",
        "unindexed_cues",
        "missing_separators",
        "fractional_labels",
        "normalized_timings",
        "layout_sha256",
        "remove",
        "unexpected",
    ] {
        let mut changed = original.clone();
        if field == "remove" {
            changed.as_object_mut().unwrap().remove("record_framing");
        } else {
            changed["record_framing"][field] = if field == "policy" || field.ends_with("sha256") {
                json!("wrong")
            } else {
                json!(999)
            };
        }
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(cache.references(&m).is_err());
        assert!(acquire(&cache, &m, &mut offline).is_err());
        assert_eq!((offline.gets, offline.posts, offline.contents), (0, 0, 0));
    }
    fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    drop(cache);
    let cache = Cache::open(&root).unwrap();
    assert_eq!(references(&cache, &m.scope, true).unwrap().len(), 1);
    assert_eq!(
        fs::read(entry.join("content.srt")).unwrap(),
        MIXED.as_bytes()
    );
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
