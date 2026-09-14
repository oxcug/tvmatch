use super::*;
use crate::opensubtitles::transcript;
const RAW: &str = "1\n00:00:01,002 --> 00:00:03,004\n  Synthetic caption\t \nsecond line\n\n2\n00:00:04,000 --> 00:00:05,000\nLast caption";
#[test]
fn utf16_raw_body_receipt_encoding_and_empty_outcome_are_preserved() {
    for little in [true, false] {
        let encode = |text: &str| {
            let mut bytes = if little {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for unit in text.encode_utf16() {
                bytes.extend(if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            bytes
        };
        let raw = encode(RAW);
        let prepared = transcript(&raw).unwrap();
        assert_eq!(
            prepared.transcript.cues()[0].text,
            "  Synthetic caption\t \nsecond line"
        );
        let expected = if little {
            "utf-16le-bom"
        } else {
            "utf-16be-bom"
        };
        assert_eq!(prepared.source_layout.unwrap().encoding, expected);
        let root = temp();
        let cache = Cache::open(&root).unwrap();
        let mut m = manifest();
        m.scope = scope();
        m.selected.truncate(1);
        cache.freeze(&m).unwrap();
        let s = &m.selected[0];
        cache.reserve(s).unwrap();
        cache.publish(s, &raw).unwrap();
        assert_eq!(cache.references(&m).unwrap().len(), 1);
        let dir = root.join(format!(
            "show-{}/season-{}/episode-{}/file-{}",
            s.show_id, s.season, s.episode, s.file_id
        ));
        assert_eq!(fs::read(dir.join("content.srt")).unwrap(), raw);
        let path = dir.join("provenance.json");
        let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let mut changed = original.clone();
        changed["source_layout"]["encoding"] = json!("utf-8");
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(cache.references(&m).is_err());
        fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        let mut empty = s.clone();
        empty.file_id += 1000;
        let raw = encode("1\r00:00:01,000-->00:00:02,000\r\r");
        cache.reserve(&empty).unwrap();
        cache.stage(&empty, &raw).unwrap();
        assert_eq!(cache.empty_proof(&empty).unwrap().unwrap().records, 1);
        drop(cache);
        let cache = Cache::open(&root).unwrap();
        assert_eq!(references(&cache, &m.scope, false).unwrap().len(), 1);
        drop(cache);
        fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn provider_layout_corpus_has_exact_fields_and_versioned_new_syntax() {
    for ending in ["\n", "\r\n", "\r"] {
        for arrow in [" --> ", "-->", "\t-->\t"] {
            for decimal in [",", "."] {
                for positioned in [false, true] {
                    let raw = RAW
                        .replace(" --> ", arrow)
                        .replace(',', decimal)
                        .replace(
                            "03.004",
                            if positioned {
                                "03.004 X1:0 X2:720 Y1:10 Y2:576"
                            } else {
                                "03.004"
                            },
                        )
                        .replace(
                            "03,004",
                            if positioned {
                                "03,004 X1:0 X2:720 Y1:10 Y2:576"
                            } else {
                                "03,004"
                            },
                        )
                        .replace('\n', ending);
                    let p = transcript(raw.as_bytes()).unwrap();
                    assert_eq!(p.transcript.cues().len(), 2);
                    let first = &p.transcript.cues()[0];
                    assert_eq!(
                        (first.start_ms, first.end_ms, first.text.as_str()),
                        (1002, 3004, "  Synthetic caption\t \nsecond line")
                    );
                    assert_eq!(p.source_layout.is_some(), ending == "\r" || positioned);
                    if let Some(layout) = p.source_layout {
                        assert_eq!(layout.positioned_cues, usize::from(positioned));
                        assert_eq!(
                            layout.bare_cr_line_endings,
                            if ending == "\r" { 7 } else { 0 }
                        );
                    }
                }
            }
        }
    }
}
#[test]
fn layout_policies_compose_without_hiding_missing_text_or_invalid_records() {
    let raw = "1\r00:00:10.000-->00:00:11.000 X1:0 X2:10 Y1:0 Y2:10\rAlpha\r\rBeta\r\r2\r00:00:01,000 --> 00:00:01,000\rZero\r\r3\r00:00:02,000 --> 00:00:03,000\r\r4\r00:00:03,000 --> 00:00:04,000\rWords \u{009d}\r";
    let p = transcript(raw.as_bytes()).unwrap();
    assert_eq!(p.transcript.cues().len(), 2);
    assert!(p.source_layout.is_some());
    assert!(p.caption_paragraphs.is_some());
    assert!(p.zero_duration.is_some());
    assert!(p.missing_text.is_some());
    assert!(p.caption_normalization.is_some());
    assert!(p.cue_ordering.is_some());
    assert_eq!(p.transcript.cues()[1].text, "Alpha\nBeta");
    for bad in [
        raw.replace("X2:10", "X2:-1"),
        raw.replace("00:00:10.000", "00:60:10.000"),
        raw.replace("Beta", "Bad\u{000b}control"),
        raw.replace("4\r00:", "3\r00:"),
    ] {
        assert!(transcript(bad.as_bytes()).is_err());
    }
    for ending in ["\n", "\r"] {
        let raw = format!(
            "1{ending}00:00:00,000-->00:00:01,000{ending}{}",
            "a".repeat(4097)
        );
        assert!(transcript(raw.as_bytes()).is_err());
    }
}
#[test]
fn new_layout_policy_is_recomputed_on_reopen_and_all_fields_are_bound() {
    let root = temp();
    let cache = Cache::open(&root).unwrap();
    let mut m = manifest();
    m.scope = scope();
    m.selected.truncate(1);
    cache.freeze(&m).unwrap();
    let s = &m.selected[0];
    let raw = RAW
        .replace("03,004", "03,004 X1:0 X2:1 Y1:0 Y2:1")
        .replace('\n', "\r");
    cache.reserve(s).unwrap();
    cache.publish(s, raw.as_bytes()).unwrap();
    let dir = root.join(format!(
        "show-{}/season-{}/episode-{}/file-{}",
        s.show_id, s.season, s.episode, s.file_id
    ));
    let path = dir.join("provenance.json");
    let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        original["source_layout"]["policy"],
        "provider-srt-source-layout-v1"
    );
    assert_eq!(original["source_layout"]["positioned_cues"], 1);
    for field in [
        "policy",
        "encoding",
        "original_cues",
        "bare_cr_line_endings",
        "positioned_cues",
        "layout_sha256",
        "unknown",
    ] {
        let mut changed = original.clone();
        changed["source_layout"][field] =
            if field.ends_with("cues") || field == "bare_cr_line_endings" {
                json!(99)
            } else {
                json!("wrong")
            };
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(cache.references(&m).is_err());
    }
    let mut missing = original.clone();
    missing.as_object_mut().unwrap().remove("source_layout");
    fs::write(&path, serde_json::to_vec(&missing).unwrap()).unwrap();
    assert!(cache.references(&m).is_err());
    fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(fs::read(dir.join("content.srt")).unwrap(), raw.as_bytes());
    drop(cache);
    let cache = Cache::open(&root).unwrap();
    assert_eq!(references(&cache, &m.scope, false).unwrap().len(), 1);
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn provider_newline_heavy_and_oversized_caption_inputs_are_bounded() {
    for ending in ["\n", "\r"] {
        let raw = ending.repeat(crate::srt::MAX_SRT_BYTES);
        assert!(transcript(raw.as_bytes()).is_err());
        let raw = format!(
            "1{ending}00:00:00,000 --> 00:00:01,000{ending}{}",
            "a".repeat(crate::srt::MAX_CUE_BYTES + 1)
        );
        assert!(transcript(raw.as_bytes()).is_err());
    }
}
