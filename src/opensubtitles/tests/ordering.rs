use super::*;
const RAW: &str = "1\n00:00:01,000 --> 00:00:04,000\nLater synthetic caption.\n\n2\n00:00:00,973 --> 00:00:02,404\nEarlier synthetic caption.\n\n";
#[test]
fn provider_ordering_preserves_cues_and_public_parser_stays_strict() {
    let error = Transcript::parse(RAW).unwrap_err();
    assert_eq!(error.line, 6);
    assert_eq!(error.kind, crate::srt::ParseErrorKind::InvalidTiming);
    let prepared = transcript(RAW.as_bytes()).unwrap();
    assert!(prepared.caption_normalization.is_none());
    let cues = prepared.transcript.cues();
    assert_eq!(cues.len(), 2);
    assert_eq!(
        (cues[0].start_ms, cues[0].end_ms, cues[0].text.as_str()),
        (973, 2404, "Earlier synthetic caption.")
    );
    assert_eq!(
        (cues[1].start_ms, cues[1].end_ms, cues[1].text.as_str()),
        (1000, 4000, "Later synthetic caption.")
    );
    let policy = prepared.cue_ordering.unwrap();
    assert_eq!(policy.policy, "provider-stable-cue-order-v1");
    assert_eq!((policy.cue_count, policy.moved_cues), (2, 2));
    assert_eq!(policy.permutation_sha256.len(), 64);
    assert_eq!(
        transcript(RAW.as_bytes()).unwrap().cue_ordering,
        Some(policy)
    );
}
#[test]
fn ordering_is_stable_preserves_duplicate_occurrences_and_composes_caption_policy() {
    let raw = "\u{feff}1\r\n00:00:05,000 --> 00:00:06,000\r\nLate\u{009d}caption.\r\n\r\n2\r\n00:00:01,000 --> 00:00:02,000\r\nSame caption.\r\n\r\n3\r\n00:00:01,000 --> 00:00:02,000\r\nSame caption.\r\n\r\n4\r\n00:00:01,000 --> 00:00:03,000\r\nLast at equal start.\r\nSecond line.\r\n";
    let prepared = transcript(raw.as_bytes()).unwrap();
    assert_eq!(prepared.caption_normalization.unwrap().replacements, 1);
    assert_eq!(prepared.cue_ordering.unwrap().moved_cues, 4);
    let cues = prepared.transcript.cues();
    assert_eq!(cues.len(), 4);
    assert_eq!(
        cues.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        [
            "Same caption.",
            "Same caption.",
            "Last at equal start.\nSecond line.",
            "Late\u{fffd}caption."
        ]
    );
    assert_eq!(
        cues.iter().map(|c| c.end_ms).collect::<Vec<_>>(),
        [2000, 2000, 3000, 6000]
    );
    let already_sorted =
        "1\n00:00:01,000 --> 00:00:02,000\nFirst\n\n2\n00:00:01,000 --> 00:00:03,000\nSecond\n";
    assert!(
        transcript(already_sorted.as_bytes())
            .unwrap()
            .cue_ordering
            .is_none()
    );
}
#[test]
fn ordering_never_repairs_other_structure_or_bounds() {
    let empty = format!("{RAW}3\n00:00:06,000 --> 00:00:07,000\n\n");
    assert!(crate::opensubtitles::ordering::parse(&empty).is_err());
    assert_eq!(
        transcript(empty.as_bytes())
            .unwrap()
            .missing_text
            .unwrap()
            .skipped_cues,
        1
    );
    for suffix in [
        "3x\n00:00:06,000 --> 00:00:07,000\nWrong index\n",
        "3\n00:00:06,000 --> 00:00:05,000\nNegative duration\n",
        "3\n00:00:06,00 --> 00:00:07,000\nBad timestamp\n",
        "3\n00:00:06,000 --> 00:00:07,000\nForbidden\u{009c}\n",
        "3\n00:00:06,000 --> 00:00:07,000\n \u{009d} \n",
    ] {
        assert!(transcript(format!("{RAW}{suffix}").as_bytes()).is_err());
    }
    let zero_duration = format!("{RAW}3\n00:00:06,000 --> 00:00:06,000\nZero\n");
    assert!(
        crate::opensubtitles::ordering::parse(&format!(
            "{RAW}4\n00:00:06,000 --> 00:00:07,000\nGap\n"
        ))
        .is_err()
    );
    let error = crate::opensubtitles::ordering::parse(&zero_duration)
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("SRT line 10: InvalidTiming"), "{error}");
    let missing_separator = format!(
        "{}\n3\n00:00:06,000 --> 00:00:07,000\nThird\n",
        RAW.trim_end()
    );
    assert!(crate::opensubtitles::ordering::parse(&missing_separator).is_err());
    let prepared = transcript(missing_separator.as_bytes()).unwrap();
    assert!(prepared.record_framing.is_some());
    assert!(prepared.cue_ordering.is_some());
    assert!(transcript(&[255]).is_err());
    let oversized = format!(
        "{RAW}3\n00:00:06,000 --> 00:00:07,000\n{}\n",
        "x".repeat(crate::srt::MAX_CUE_BYTES + 1)
    );
    assert!(transcript(oversized.as_bytes()).is_err());
    let mut too_many = RAW.to_owned();
    for n in 3..=crate::srt::MAX_CUES + 1 {
        too_many.push_str(&format!(
            "{n}\n00:00:06,000 --> 00:00:07,000\nSynthetic\n\n"
        ));
    }
    assert!(transcript(too_many.as_bytes()).is_err());
    assert!(transcript(&vec![b' '; crate::srt::MAX_SRT_BYTES + 1]).is_err());
}
#[test]
fn ordered_staging_recovers_offline_keeps_raw_and_rechecks_provenance() {
    let root = temp();
    let mut m = manifest();
    m.scope = scope();
    m.selected.truncate(1);
    let cache = Cache::open(&root).unwrap();
    cache.freeze(&m).unwrap();
    cache.reserve(&m.selected[0]).unwrap();
    let partial = cache.stage(&m.selected[0], RAW.as_bytes()).unwrap();
    drop(cache);
    let cache = Cache::open(&root).unwrap();
    let mut offline = Mock::new(None);
    acquire(&cache, &m, &mut offline).unwrap();
    assert_eq!((offline.posts, offline.gets, offline.contents), (0, 0, 0));
    let entry = partial.with_extension("");
    assert_eq!(fs::read(entry.join("content.srt")).unwrap(), RAW.as_bytes());
    assert!(!partial.exists());
    assert!(!entry.join("download.json").exists());
    let path = entry.join("provenance.json");
    let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(original["format"], "srt-utf8-raw-with-cue-ordering-v1");
    assert_eq!(original["cue_ordering"]["moved_cues"], 2);
    assert_eq!(original["recovered_without_fetch_metadata"], true);
    assert!(original.get("caption_normalization").is_none());
    assert_eq!(cache.references(&m).unwrap().len(), 1);
    for field in [
        "policy",
        "cue_count",
        "moved_cues",
        "permutation_sha256",
        "missing",
    ] {
        let mut tampered = original.clone();
        if field == "missing" {
            tampered.as_object_mut().unwrap().remove("cue_ordering");
            tampered["format"] = json!("srt-utf8-original");
        } else {
            tampered["cue_ordering"][field] = match field {
                "policy" | "permutation_sha256" => json!("wrong"),
                _ => json!(999),
            };
        }
        fs::write(&path, serde_json::to_vec(&tampered).unwrap()).unwrap();
        assert!(cache.contains(&m.selected[0]).is_err(), "{field}");
        assert!(acquire(&cache, &m, &mut offline).is_err());
        assert_eq!((offline.posts, offline.gets, offline.contents), (0, 0, 0));
        assert_eq!(fs::read(entry.join("content.srt")).unwrap(), RAW.as_bytes());
    }
    fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    drop(cache);
    let cache = Cache::open(&root).unwrap();
    assert_eq!(references(&cache, &m.scope, true).unwrap().len(), 1);
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
