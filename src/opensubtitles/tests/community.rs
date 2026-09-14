use super::*;

#[test]
fn missing_text_is_a_record_api_error_but_provider_caller_can_skip_it() {
    let raw = "1\n00:00:01,000 --> 00:00:02,000\n\n2\n00:00:03,000 --> 00:00:04,000\nRetained caption.\n\n3\n00:00:05,000 --> 00:00:06,000\n";
    assert_eq!(
        Transcript::parse(raw).unwrap_err().kind,
        crate::srt::ParseErrorKind::MissingText
    );
    let outcomes = super::super::records::parser::parse(raw).unwrap();
    assert_eq!(outcomes.len(), 3);
    for i in [0, 2] {
        match &outcomes[i] {
            Err(super::super::records::parser::RecordError::MissingText(r)) => {
                assert!(r.cue.text.is_empty());
                assert_eq!(r.integer(), Some(i as u32 + 1));
                assert!(r.cue.end_ms > r.cue.start_ms);
            }
            _ => panic!("expected explicit MissingText outcome"),
        }
    }
    assert!(outcomes[1].is_ok());
    let p = transcript(raw.as_bytes()).unwrap();
    let m = p.missing_text.unwrap();
    assert_eq!(
        (m.original_cues, m.skipped_cues, m.retained_cues),
        (3, 2, 1)
    );
    assert_eq!(m.policy, "provider-skip-missing-text-v1");
    assert!(p.zero_duration.is_none());
    let c = &p.transcript.cues()[0];
    assert_eq!(
        (c.start_ms, c.end_ms, c.text.as_str()),
        (3000, 4000, "Retained caption.")
    );
    // Consecutive unindexed empty records and no-separator indexed records remain
    // explicit record errors, not success with fabricated empty captions.
    for raw in [
        "00:00:01,000 --> 00:00:02,000\n00:00:03,000 --> 00:00:04,000\nKept.\n",
        "1\n00:00:01,000 --> 00:00:02,000\n2\n00:00:03,000 --> 00:00:04,000\nKept.\n",
    ] {
        let p = transcript(raw.as_bytes()).unwrap();
        assert_eq!(p.missing_text.unwrap().skipped_cues, 1);
        assert_eq!(p.transcript.cues()[0].text, "Kept.");
    }
}

#[test]
fn omissions_compose_without_changing_retained_text_times_order_or_legacy_policies() {
    let raw = "1\n00:00:01,000 --> 00:00:02,000\n\n3\n00:00:00,000 --> 00:00:00,000\n\n8\n00:00:00,000 --> 00:00:00,000\nZero display.\n\n9\n00:00:10,000 --> 00:00:11,000\nLater\u{0092}caption.\n\n10\n00:00:03,000 --> 00:00:04,000\nRepeated caption.\n\n11\n00:00:03,000 --> 00:00:04,000\nRepeated caption.\n";
    let p = transcript(raw.as_bytes()).unwrap();
    let missing = p.missing_text.unwrap();
    let zero = p.zero_duration.unwrap();
    assert_eq!(
        (
            missing.original_cues,
            missing.skipped_cues,
            missing.retained_cues
        ),
        (6, 2, 4)
    );
    assert_eq!(
        (zero.original_cues, zero.skipped_cues, zero.retained_cues),
        (4, 1, 3)
    );
    assert_eq!(p.cue_numbering.unwrap().original_cues, 6);
    assert_eq!(p.caption_normalization.unwrap().replacements, 1);
    assert!(p.cue_ordering.is_some());
    assert_eq!(
        p.transcript
            .cues()
            .iter()
            .map(|c| (c.start_ms, c.end_ms, c.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (3000, 4000, "Repeated caption."),
            (3000, 4000, "Repeated caption."),
            (10000, 11000, "Later\u{fffd}caption.")
        ]
    );
}

#[test]
fn skipping_missing_text_never_hides_other_errors_or_evades_bounds() {
    let tail = "\n2\n00:00:03,000 --> 00:00:04,000\nKept.\n";
    for bad in [
        "1\n00:00:02,000 --> 00:00:01,000\n",
        "1\n00:00:01,000 --> 00:00:02.00\n",
        "1\n00:61:01,000 --> 00:61:02,000\n",
        "1\n00:00:01,000 --> 00:00:02,000\n\0\n",
        "1\n00:00:01,000 --> 00:00:02,000\n\u{0092}\n",
        "1\n00:00:01,000 --> 00:00:02,000\n\u{000b}\n",
    ] {
        assert!(transcript(format!("{bad}{tail}").as_bytes()).is_err());
    }
    assert!(transcript(b"1\n00:00:01,000 --> 00:00:02,000\n").is_err());
    assert!(transcript(b"1\n00:00:00,000 --> 00:00:00,000\n").is_err());
    assert!(transcript(b"1\n00:00:01,000 --> 00:00:02,000\n\n2\n00:00:03,000 --> 00:00:04,000\nKept.\n\n3\ninvalid timestamp\n").is_err());
    let mut many = String::new();
    for n in 1..=crate::srt::MAX_CUES + 1 {
        many.push_str(&format!("{n}\n00:00:01,000 --> 00:00:02,000\n\n"));
    }
    assert!(super::super::records::parser::parse(&many).is_err());
}

#[test]
fn missing_text_cache_provenance_is_recomputed_without_redownload() {
    let raw=b"1\n00:00:01,000 --> 00:00:02,000\n\n2\n00:00:03,000 --> 00:00:04,000\nRetained caption.\n";
    let root = temp();
    let mut m = manifest();
    m.scope = scope();
    m.selected.truncate(1);
    let cache = Cache::open(&root).unwrap();
    cache.freeze(&m).unwrap();
    cache.reserve(&m.selected[0]).unwrap();
    let partial = cache.stage(&m.selected[0], raw).unwrap();
    let mut offline = Mock::new(None);
    acquire(&cache, &m, &mut offline).unwrap();
    let entry = partial.with_extension("");
    let path = entry.join("provenance.json");
    let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(original["missing_text"]["skipped_cues"], 1);
    assert_eq!(
        original["format"],
        "srt-utf8-raw-with-missing-text-derivative-v1"
    );
    assert!(original.get("zero_duration").is_none());
    for field in [
        "policy",
        "original_cues",
        "skipped_cues",
        "retained_cues",
        "selection_sha256",
        "unknown_field",
    ] {
        let mut changed = original.clone();
        changed["missing_text"][field] = if field.ends_with("cues") {
            json!(999)
        } else {
            json!("tampered")
        };
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(cache.references(&m).is_err());
        assert!(acquire(&cache, &m, &mut offline).is_err());
    }
    let mut removed = original.clone();
    removed.as_object_mut().unwrap().remove("missing_text");
    removed["format"] = json!("srt-utf8-original");
    fs::write(&path, serde_json::to_vec(&removed).unwrap()).unwrap();
    assert!(cache.references(&m).is_err());
    assert!(acquire(&cache, &m, &mut offline).is_err());
    fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    drop(cache);
    let cache = Cache::open(&root).unwrap();
    assert_eq!(references(&cache, &m.scope, true).unwrap().len(), 1);
    assert_eq!((offline.gets, offline.posts, offline.contents), (0, 0, 0));
    assert_eq!(fs::read(entry.join("content.srt")).unwrap(), raw);
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn community_s2e14_html_escaped_title_agrees_without_changing_ids_or_frozen_labels() {
    for defect in [None, Some("title"), Some("identity")] {
        let mut online = Mock::new(None);
        online.responses.push_back(Ok(json!({"data":[{"id":"10193","attributes":{"feature_id":10193,"imdb_id":1439629,"title":"community","feature_type":"Tvshow"}}]})));
        online.responses.push_back(Ok(json!({"data":[{"id":"10193","attributes":{"feature_id":10193,"title":"community","seasons":[{"season_number":2,"episodes":[{"episode_number":14,"feature_id":153707,"title":"Advanced Dungeons & Dragons"}]}]}}]})));
        let mut row = subtitle(7429413, 8362199, true);
        let a = &mut row["attributes"];
        a["feature_details"] = json!({"parent_feature_id":10193,"feature_id":153707,"feature_type":"Episode","season_number":2,"episode_number":14,"title":"Advanced Dungeons &amp; Dragons"});
        a["release"] = json!("Original.S02E14");
        a["files"][0]["file_name"] = json!("Original.S02E14.srt");
        if defect == Some("title") {
            a["feature_details"]["title"] = json!("Different Episode");
        }
        if defect == Some("identity") {
            a["feature_details"]["feature_id"] = json!(153708);
        }
        online.responses.push_back(Ok(
            json!({"page":1,"total_pages":1,"total_count":1,"data":[row]}),
        ));
        let result = catalog::resolve(
            &mut online,
            &Scope::from_imdb("tt1439629", "2", Some("14")).unwrap(),
        );
        if let Some(defect) = defect {
            let error = result.unwrap_err().0;
            if defect == "title" {
                assert!(error.contains("S02E14"));
                assert!(error.contains("153707"));
                assert!(error.contains("7429413"));
            }
        } else {
            let m = result.unwrap();
            assert_eq!(m.selected[0].title, "Advanced Dungeons & Dragons");
            assert_eq!(
                (m.selected[0].episode_id, m.selected[0].file_id),
                (153707, 8362199)
            );
        }
        assert_eq!((online.gets, online.posts, online.contents), (3, 0, 0));
    }
}
