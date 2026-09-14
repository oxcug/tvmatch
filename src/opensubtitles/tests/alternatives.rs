use super::*;

fn alternative_detail() -> Value {
    let mut value = detail();
    let rows = value["data"][0]["attributes"]["seasons"][0]["episodes"]
        .as_array_mut()
        .unwrap();
    rows.push(json!({"episode_number":1,"feature_id":12,"title":"Alternative Episode"}));
    rows.push(rows[0].clone()); // Identical catalog row must not cost another download.
    rows.reverse(); // Stable output cannot depend on provider row order.
    value
}
fn alternative_page() -> Value {
    let mut other = subtitle(22, 32, false);
    other["attributes"]["feature_details"]["feature_id"] = json!(12);
    other["attributes"]["feature_details"]["title"] = json!("Alternative Episode");
    json!({"page":1,"total_pages":1,"total_count":2,"data":[other,subtitle(21,31,true)]})
}
fn alternatives() -> (Manifest, Mock) {
    let mut online = Mock::new(Some(20));
    online.responses.push_back(Ok(json!({"data":[show(1)]})));
    online.responses.push_back(Ok(alternative_detail()));
    online.responses.push_back(Ok(alternative_page()));
    let manifest = catalog::resolve(&mut online, &scope()).unwrap();
    assert_eq!(online.gets, 3); // One subtitle query for both identities.
    (manifest, online)
}
#[test]
fn exact_catalog_duplicates_download_once() {
    let mut online = metadata_mock();
    let mut value = detail();
    let rows = value["data"][0]["attributes"]["seasons"][0]["episodes"]
        .as_array_mut()
        .unwrap();
    rows.push(rows[0].clone());
    online.responses[1] = Ok(value);
    online.responses.push_back(Ok(
        json!({"page":1,"total_pages":1,"total_count":1,"data":[subtitle(21,31,true)]}),
    ));
    let m = catalog::resolve(&mut online, &scope()).unwrap();
    assert_eq!(m.selected.len(), 1);
    online.responses.push_back(Ok(
        json!({"remaining":19,"link":"https://www.opensubtitles.com/synthetic"}),
    ));
    let root = temp();
    let cache = Cache::open(&root).unwrap();
    cache.freeze(&m).unwrap();
    acquire(&cache, &m, &mut online).unwrap();
    assert_eq!(online.files, [31]);
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn competing_ids_cache_independently_and_reopen_without_http() {
    let (m, mut online) = alternatives();
    assert_eq!(
        m.selected
            .iter()
            .map(|s| (s.episode, s.episode_id, s.file_id))
            .collect::<Vec<_>>(),
        [(1, 11, 31), (1, 12, 32)]
    );
    let root = temp();
    let cache = Cache::open(&root).unwrap();
    cache.freeze(&m).unwrap();
    for n in [19, 18] {
        online.responses.push_back(Ok(
            json!({"remaining":n,"link":"https://www.opensubtitles.com/synthetic"}),
        ));
    }
    acquire(&cache, &m, &mut online).unwrap();
    assert_eq!(online.files, [31, 32]);
    let first = cache
        .selected(1, 1, 1, 11, "Original Episode")
        .unwrap()
        .unwrap();
    assert_eq!(first.file_id, 31);
    assert_eq!(
        cache
            .selected(1, 1, 1, 12, "Alternative Episode")
            .unwrap()
            .unwrap()
            .file_id,
        32
    );
    assert!(cache.selected(1, 1, 1, 11, "Alternative Episode").is_err());
    drop(cache);
    let cache = Cache::open(&root).unwrap();
    let refs = references(&cache, &m.scope, false).unwrap();
    assert_eq!(refs.len(), 2);
    assert_ne!(refs[0].label.id, refs[1].label.id);
    let mut offline = Mock::new(None);
    acquire(&cache, &m, &mut offline).unwrap();
    assert_eq!((offline.posts, offline.gets, offline.contents), (0, 0, 0));
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn alternatives_count_against_quota_and_preserve_existing_frozen_choice() {
    let (m, _) = alternatives();
    let root = temp();
    let cache = Cache::open(&root).unwrap();
    let mut short = Mock::new(Some(1));
    assert!(acquire(&cache, &m, &mut short).is_err());
    assert_eq!(short.posts, 0);
    let mut old = m.clone();
    old.scope = Scope::new("Original Show", "1", None).unwrap();
    old.selected.truncate(1);
    cache.freeze(&old).unwrap();
    cache.publish(&old.selected[0], &short.body).unwrap();
    let mut online = Mock::new(Some(20));
    online.responses.push_back(Ok(alternative_detail()));
    // Existing identity has no current search results; its frozen valid selection still wins.
    let mut page = alternative_page();
    page["data"].as_array_mut().unwrap().pop();
    page["total_count"] = json!(1);
    online.responses.push_back(Ok(page));
    let resolved = catalog::resolve_season(
        &mut online,
        &scope(),
        1,
        "Original Show".into(),
        Some(&cache),
    )
    .unwrap();
    assert_eq!(resolved.selected[0], old.selected[0]);
    online.responses.push_back(Ok(
        json!({"remaining":19,"link":"https://www.opensubtitles.com/synthetic"}),
    ));
    acquire(&cache, &resolved, &mut online).unwrap();
    assert_eq!(online.files, [32]);
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn same_identity_conflicts_and_foreign_subtitle_labels_still_fail_closed() {
    for field in ["title", "episode_number"] {
        let mut online = metadata_mock();
        let mut value = detail();
        let rows = value["data"][0]["attributes"]["seasons"][0]["episodes"]
            .as_array_mut()
            .unwrap();
        let mut other = rows[0].clone();
        other[field] = if field == "title" {
            json!("Conflicting title")
        } else {
            json!(2)
        };
        rows.push(other);
        online.responses[1] = Ok(value);
        let full = Scope::new("Original Show", "1", None).unwrap();
        let error = catalog::resolve(&mut online, &full)
            .unwrap_err()
            .to_string();
        assert!(error.contains("same episode identity"));
        assert_eq!(online.posts, 0);
    }
    let mut online = metadata_mock();
    online.responses[1] = Ok(alternative_detail());
    let mut page = alternative_page();
    page["data"][0]["attributes"]["feature_details"]["feature_id"] = json!(999);
    online.responses.push_back(Ok(page));
    assert!(catalog::resolve(&mut online, &scope()).is_err());
}
#[test]
fn independent_variants_identify_a_clear_winner_but_never_pool_support() {
    let (m, _) = alternatives();
    let root = temp();
    let cache = Cache::open(&root).unwrap();
    cache.freeze(&m).unwrap();
    let a = b"1\n00:00:00,000 --> 00:00:01,000\nAmber lanterns illuminate quiet gardens\n\n2\n00:00:10,000 --> 00:00:11,000\nSilver otters navigate winding rivers\n\n3\n00:00:20,000 --> 00:00:21,000\nVelvet clouds surround distant mountains\n";
    let b = b"1\n00:00:05,000 --> 00:00:06,000\nCopper falcons patrol ancient towers\n\n2\n00:00:15,000 --> 00:00:16,000\nGolden badgers uncover hidden caverns\n\n3\n00:00:25,000 --> 00:00:26,000\nScarlet boats explore frozen harbors\n";
    cache.publish(&m.selected[0], a).unwrap();
    cache.publish(&m.selected[1], b).unwrap();
    let refs = cache.references(&m).unwrap();
    let index = crate::Index::build(refs.clone()).unwrap();
    let first = transcript(a).unwrap().transcript;
    let second = transcript(b).unwrap().transcript;
    let crate::MatchOutcome::Identified { best, .. } = index.match_query(&first).unwrap() else {
        panic!("clear winner");
    };
    assert_eq!(best.reference.id.value(), "11");
    assert_eq!(best.reference.display_name, "S01E01 Original Episode");
    let mut cues = first.cues().to_vec();
    cues.extend_from_slice(second.cues());
    cues.sort_by_key(|c| c.start_ms);
    let mixed = Transcript::from_cues(cues.clone()).unwrap();
    assert!(matches!(
        index.match_query(&mixed).unwrap(),
        crate::MatchOutcome::Ambiguous { .. }
    ));
    // Two anchors for each distinct identity cannot become four anchors for a merged episode.
    cues.retain(|c| c.start_ms < 20_000);
    let weak = Transcript::from_cues(cues).unwrap();
    assert!(matches!(
        index.match_query(&weak).unwrap(),
        crate::MatchOutcome::Unknown { .. }
    ));
    let mut indistinguishable = refs;
    indistinguishable[1].transcript = first.clone();
    assert!(matches!(
        crate::Index::build(indistinguishable)
            .unwrap()
            .match_query(&first)
            .unwrap(),
        crate::MatchOutcome::Unknown { .. }
    ));
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn missing_alternative_is_recorded_when_another_identity_covers_the_episode() {
    let mut online = metadata_mock();
    online.responses[1] = Ok(alternative_detail());
    online.responses.push_back(Ok(
        json!({"page":1,"total_pages":1,"total_count":1,"data":[subtitle(21,31,true)]}),
    ));
    let manifest = catalog::resolve(&mut online, &scope()).unwrap();
    assert_eq!(manifest.selected.len(), 1);
    assert_eq!(manifest.selected[0].episode_id, 11);
    assert_eq!(manifest.unavailable.len(), 1);
    assert_eq!(manifest.unavailable[0].episode_id, 12);
    assert_eq!(
        manifest.unavailable[0].reason,
        UnavailableReason::NoEligibleEnglishReference
    );
    assert_eq!((online.posts, online.contents), (0, 0));
}
#[test]
fn malformed_alternative_manifests_still_reject_duplicate_ids_files_and_order() {
    let (m, _) = alternatives();
    for fault in ["id", "file", "order", "range"] {
        let mut bad = m.clone();
        match fault {
            "id" => bad.selected[1].episode_id = bad.selected[0].episode_id,
            "file" => bad.selected[1].file_id = bad.selected[0].file_id,
            "order" => bad.selected.reverse(),
            "range" => bad.scope = Scope::new("Original Show", "1", Some("1-2")).unwrap(),
            _ => unreachable!(),
        }
        assert!(bad.validate(&bad.scope).is_err(), "{fault}");
    }
}
