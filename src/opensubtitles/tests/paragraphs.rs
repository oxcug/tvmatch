use super::*;
use crate::opensubtitles::transcript as prepare_srt;
const RAW: &str = "1\n00:00:01,000 --> 00:00:03,000\nFirst synthetic line.\n\nSecond synthetic line.\n\n2\n00:00:05,000 --> 00:00:07,000\nNext synthetic cue.\n";
#[test]
fn framer_retains_paragraphs_provider_normalizes_and_public_srt_stays_strict() {
    assert!(Transcript::parse(RAW).is_err());
    let framed = crate::opensubtitles::records::parser::parse(RAW).unwrap();
    let first = framed[0].as_ref().unwrap();
    assert_eq!(first.caption_blank_lines, vec![4]);
    assert_eq!(
        first.cue.text,
        "First synthetic line.\n\nSecond synthetic line."
    );
    let prepared = prepare_srt(RAW.as_bytes()).unwrap();
    let p = prepared.caption_paragraphs.unwrap();
    assert_eq!(p.policy, "provider-caption-paragraphs-v1");
    assert_eq!(
        (p.original_cues, p.affected_cues, p.removed_blank_lines),
        (2, 1, 1)
    );
    assert_eq!(
        prepared.transcript.cues()[0].text,
        "First synthetic line.\nSecond synthetic line."
    );
    assert_eq!(
        (
            prepared.transcript.cues()[0].start_ms,
            prepared.transcript.cues()[0].end_ms
        ),
        (1000, 3000)
    );
    assert_eq!(prepared.transcript.cues()[1].text, "Next synthetic cue.");
    assert!(prepared.record_framing.is_none());
    assert!(prepared.missing_text.is_none());
}
#[test]
fn paragraph_spacing_preserves_nonblank_lines_and_composes_with_existing_policies() {
    for separator in ["\n\n", "\n \n\t\n", "\r\n\r\n"] {
        let raw = RAW.replace("line.\n\nSecond", &format!("line.{separator}Second"));
        let p = prepare_srt(raw.as_bytes()).unwrap();
        assert_eq!(
            p.transcript.cues()[0].text,
            "First synthetic line.\nSecond synthetic line."
        );
        assert_eq!(
            p.caption_paragraphs.unwrap().removed_blank_lines,
            if separator.contains('\t') { 2 } else { 1 }
        );
    }
    let raw = "1\n00:00:20,000 --> 00:00:22,000\nAlpha synthetic words.\n\nBeta synthetic words.\n\n2\n00:00:00,000 --> 00:00:00,000\nOmitted nonempty zero.\n\n3\n00:00:00,000 --> 00:00:01,000\n\n4\n00:00:01,000 --> 00:00:02,000\nGamma synthetic \u{009d} words.\n";
    let p = prepare_srt(raw.as_bytes()).unwrap();
    assert_eq!(p.transcript.cues().len(), 2);
    assert_eq!(
        p.transcript.cues()[1].text,
        "Alpha synthetic words.\nBeta synthetic words."
    );
    assert_eq!(p.caption_paragraphs.unwrap().original_cues, 4);
    assert!(p.caption_normalization.is_some());
    assert!(p.cue_ordering.is_some());
    assert_eq!(p.zero_duration.unwrap().skipped_cues, 1);
    assert_eq!(p.missing_text.unwrap().skipped_cues, 1);
}
#[test]
fn ambiguous_paragraphs_or_bad_headers_are_not_assigned_invented_timestamps() {
    for raw in [
        RAW.replace("\n2\n", "\n3\n"),
        RAW.replace("1\n00:", "1.0\n00:"),
        RAW.replacen("1\n", "", 1),
        RAW.replace("\n2\n", "\n"),
        RAW.split("\n2\n").next().unwrap().to_owned(),
        RAW.replace("First synthetic line.\n", ""),
        RAW.replace("Second synthetic line.", "309"),
        RAW.replace("Second synthetic line.", "1 --> 2"),
        RAW.replace("Second synthetic line.", "999999999999999999999"),
        RAW.replace(
            "00:00:05,000 --> 00:00:07,000",
            "00:00:07,000 --> 00:00:05,000",
        ),
        RAW.replace("Second synthetic line.", "bad\u{1}control"),
    ] {
        assert!(prepare_srt(raw.as_bytes()).is_err());
    }
    let oversized = RAW.replace("\n\nSecond", &format!("\n{}\nSecond", " ".repeat(4096)));
    assert!(prepare_srt(oversized.as_bytes()).is_err());
}
#[test]
fn ordinary_separators_keep_legacy_provenance_and_paragraph_layout_is_bound() {
    let plain = RAW.replace("\n\nSecond", "\nSecond");
    let p = prepare_srt(plain.as_bytes()).unwrap();
    assert!(p.caption_paragraphs.is_none());
    assert!(p.record_framing.is_none());
    let a = prepare_srt(RAW.as_bytes())
        .unwrap()
        .caption_paragraphs
        .unwrap();
    let b = prepare_srt(
        RAW.replace(
            "First synthetic line.\n",
            "Prefix line.\nFirst synthetic line.\n",
        )
        .as_bytes(),
    )
    .unwrap()
    .caption_paragraphs
    .unwrap();
    assert_ne!(a.layout_sha256, b.layout_sha256);
}
#[test]
fn paragraph_cache_policy_tampering_fails_offline_and_original_body_is_retained() {
    let root = temp();
    let cache = Cache::open(&root).unwrap();
    let mut m = manifest();
    m.scope = scope();
    m.selected.truncate(1);
    cache.freeze(&m).unwrap();
    let s = &m.selected[0];
    cache.reserve(s).unwrap();
    cache.publish(s, RAW.as_bytes()).unwrap();
    let dir = root.join(format!(
        "show-{}/season-{}/episode-{}/file-{}",
        s.show_id, s.season, s.episode, s.file_id
    ));
    let path = dir.join("provenance.json");
    let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        original["format"],
        "srt-utf8-raw-with-caption-paragraphs-v1"
    );
    assert_eq!(cache.references(&m).unwrap().len(), 1);
    for field in [
        "policy",
        "original_cues",
        "affected_cues",
        "removed_blank_lines",
        "layout_sha256",
        "unknown",
    ] {
        let mut changed = original.clone();
        changed["caption_paragraphs"][field] =
            if field.ends_with("cues") || field == "removed_blank_lines" {
                json!(99)
            } else {
                json!("wrong")
            };
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(cache.references(&m).is_err());
    }
    let mut removed = original.clone();
    removed
        .as_object_mut()
        .unwrap()
        .remove("caption_paragraphs");
    fs::write(&path, serde_json::to_vec(&removed).unwrap()).unwrap();
    assert!(cache.references(&m).is_err());
    fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(fs::read(dir.join("content.srt")).unwrap(), RAW.as_bytes());
    drop(cache);
    let cache = Cache::open(&root).unwrap();
    assert_eq!(references(&cache, &m.scope, false).unwrap().len(), 1);
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
