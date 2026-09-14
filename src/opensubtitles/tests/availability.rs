use super::*;
#[test]
#[ignore = "opt-in provider metadata audit; never downloads subtitles or opens the cache"]
fn inspect_live_catalog() {
    let Ok(imdb) = std::env::var("TVMATCH_TEST_CATALOG_IMDB") else {
        return;
    };
    let season = std::env::var("TVMATCH_TEST_CATALOG_SEASON").unwrap();
    struct MetadataOnly {
        online: Online,
        gets: usize,
    }
    impl Transport for MetadataOnly {
        fn api(&mut self, path: &str, body: Option<Value>) -> Result<Value> {
            if body.is_some()
                || self.gets >= 40
                || !(path.starts_with("features?") || path.starts_with("subtitles?"))
            {
                return Err(fail("metadata audit refuses non-catalog request"));
            }
            self.gets += 1;
            self.online.api(path, None)
        }
        fn content(&mut self, _: &str) -> Result<Vec<u8>> {
            Err(fail("metadata audit refuses content downloads"))
        }
        fn authenticated(&self) -> bool {
            false
        }
    }
    let mut online = MetadataOnly {
        online: Online::from_env().unwrap(),
        gets: 0,
    };
    let scope = Scope::from_imdb(&imdb, &season, None).unwrap();
    let m = catalog::resolve(&mut online, &scope).unwrap();
    println!(
        "metadata_gets={} selected_references={} covered_episode_numbers={} unavailable_id_count={}; no cache access or downloads",
        online.gets,
        m.selected.len(),
        m.selected
            .iter()
            .map(|s| s.episode)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        m.unavailable.len()
    );
    m.report_unavailable();
}
fn duplicate_title_detail() -> Value {
    let mut value = detail();
    let rows = value["data"][0]["attributes"]["seasons"][0]["episodes"]
        .as_array_mut()
        .unwrap();
    rows.push(rows[0].clone());
    rows.push(json!({"episode_number":1,"feature_id":12,"title":"Original Episode"}));
    value
}
fn page(available: bool) -> Value {
    let mut rows = vec![subtitle(21, 31, true)];
    if available {
        let mut second = subtitle(22, 32, false);
        second["attributes"]["feature_details"]["feature_id"] = json!(12);
        rows.push(second);
    }
    json!({"page":1,"total_pages":1,"total_count":rows.len(),"data":rows})
}
#[test]
fn repeated_titles_are_not_merged_and_only_exact_repeated_rows_collapse() {
    for both in [false, true] {
        let mut online = metadata_mock();
        online.responses[1] = Ok(duplicate_title_detail());
        online.responses.push_back(Ok(page(both)));
        let m = catalog::resolve(&mut online, &scope()).unwrap();
        assert_eq!(
            m.selected.iter().map(|s| s.episode_id).collect::<Vec<_>>(),
            if both { vec![11, 12] } else { vec![11] }
        );
        assert_eq!(m.unavailable.len(), usize::from(!both));
        assert_eq!((online.gets, online.posts, online.contents), (3, 0, 0));
        if both {
            assert_eq!(m.selected[0].title, m.selected[1].title);
            let root = temp();
            let cache = Cache::open(&root).unwrap();
            cache.freeze(&m).unwrap();
            for s in &m.selected {
                cache.publish(s, &online.body).unwrap();
            }
            let refs = cache.references(&m).unwrap();
            assert_ne!(refs[0].label.id, refs[1].label.id);
            drop(cache);
            fs::remove_dir_all(root).unwrap();
        }
    }
}
#[test]
fn no_available_identity_for_an_episode_or_incomplete_search_still_blocks_before_download() {
    for response in [
        Ok(json!({"page":1,"total_pages":0,"total_count":0,"data":[]})),
        Err(fail("synthetic metadata failure")),
        Ok(json!({"page":1,"total_pages":1,"total_count":2,"data":[subtitle(21,31,true)]})),
    ] {
        let mut online = metadata_mock();
        online.responses[1] = Ok(duplicate_title_detail());
        online.responses.push_back(response);
        assert!(catalog::resolve(&mut online, &scope()).is_err());
        assert_eq!((online.posts, online.contents), (0, 0));
    }
    // Covering one number cannot silently remove a different uncovered number.
    let mut online = metadata_mock();
    let mut data = detail();
    data["data"][0]["attributes"]["seasons"][0]["episodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"episode_number":2,"feature_id":12,"title":"Second episode"}));
    online.responses[1] = Ok(data);
    online.responses.push_back(Ok(page(false)));
    online.responses.push_back(Ok(
        json!({"page":1,"total_pages":0,"total_count":0,"data":[]}),
    ));
    assert!(
        catalog::resolve(
            &mut online,
            &Scope::new("Original Show", "1", None).unwrap()
        )
        .is_err()
    );
    assert_eq!(online.posts, 0);
}
#[test]
fn unavailable_snapshot_roundtrips_offline_filters_with_alias_ranges_and_validates() {
    let root = temp();
    let cache = Cache::open(&root).unwrap();
    let mut m = manifest();
    m.scope = Scope::new("Original Show", "1", None).unwrap();
    m.selected.truncate(2);
    m.unavailable = vec![
        UnavailableVariant {
            episode: 1,
            episode_id: 101,
            title: "Original Episode 1".into(),
            reason: UnavailableReason::NoEligibleEnglishReference,
        },
        UnavailableVariant {
            episode: 2,
            episode_id: 102,
            title: "Original Episode 2".into(),
            reason: UnavailableReason::NoEligibleEnglishReference,
        },
    ];
    cache.freeze(&m).unwrap();
    let mock = Mock::new(None);
    for s in &m.selected {
        cache.publish(s, &mock.body).unwrap();
    }
    let range = Scope::from_imdb("tt1234567", "1", Some("2")).unwrap();
    let alias = cache.alias(&range, 1, "Original Show").unwrap().unwrap();
    assert_eq!(alias.unavailable.len(), 1);
    assert_eq!(alias.unavailable[0].episode_id, 102);
    cache.freeze(&alias).unwrap();
    drop(cache);
    let cache = Cache::open(&root).unwrap();
    assert_eq!(references(&cache, &range, true).unwrap().len(), 1);
    assert_eq!(
        cache.manifest(&range).unwrap().unwrap().unavailable,
        alias.unavailable
    );
    for fault in [
        "selected-id",
        "duplicate-id",
        "uncovered",
        "order",
        "title",
        "count",
    ] {
        let mut bad = m.clone();
        match fault {
            "selected-id" => bad.unavailable[0].episode_id = bad.selected[0].episode_id,
            "duplicate-id" => bad.unavailable[1].episode_id = bad.unavailable[0].episode_id,
            "uncovered" => {
                bad.selected.pop();
            }
            "order" => bad.unavailable.reverse(),
            "title" => bad.unavailable[0].title = "bad\nlabel".into(),
            "count" => {
                bad.unavailable = (200..1200)
                    .map(|id| UnavailableVariant {
                        episode_id: id,
                        ..m.unavailable[0].clone()
                    })
                    .collect()
            }
            _ => unreachable!(),
        }
        assert!(bad.validate(&bad.scope).is_err(), "{fault}");
    }
    let mut bad = serde_json::to_value(&m).unwrap();
    bad["unavailable"][0]["reason"] = json!("unknown_policy");
    assert!(serde_json::from_value::<Manifest>(bad).is_err());
    let legacy = serde_json::to_value(manifest()).unwrap();
    assert!(legacy.get("unavailable").is_none());
    assert!(
        serde_json::from_value::<Manifest>(legacy)
            .unwrap()
            .unavailable
            .is_empty()
    );
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn frozen_reference_is_never_dropped_for_current_unavailability_or_retained_parse_error() {
    let mut online = metadata_mock();
    online.responses[1] = Ok(duplicate_title_detail());
    online.responses.push_back(Ok(page(true)));
    let m = catalog::resolve(&mut online, &scope()).unwrap();
    let root = temp();
    let cache = Cache::open(&root).unwrap();
    cache.freeze(&m).unwrap();
    cache.reserve(&m.selected[0]).unwrap();
    let partial = cache
        .stage(&m.selected[0], b"invalid retained body")
        .unwrap();
    cache.publish(&m.selected[1], &online.body).unwrap();
    let mut current = Mock::new(None);
    current.responses.push_back(Ok(duplicate_title_detail()));
    let again = catalog::resolve_season(
        &mut current,
        &m.scope,
        1,
        "Original Show".into(),
        Some(&cache),
    )
    .unwrap();
    assert_eq!(again.selected, m.selected);
    assert!(again.unavailable.is_empty());
    assert_eq!(current.gets, 1);
    assert!(acquire(&cache, &again, &mut current).is_err());
    assert_eq!((current.posts, current.contents), (0, 0));
    assert_eq!(
        fs::read(partial.join("content.srt")).unwrap(),
        b"invalid retained body"
    );
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn korra_s1_stray_numbers_are_not_silently_dropped_but_explicit_range_excludes_them() {
    for range in [None, Some("1-12")] {
        let mut online = metadata_mock();
        let mut metadata = detail();
        let mut rows = (1..=12)
            .map(|n| json!({"episode_number":n,"feature_id":100+n,"title":format!("Fixture {n}")}))
            .collect::<Vec<_>>();
        rows.extend([
            json!({"episode_number":13,"feature_id":311986,"title":"Rebel Spirit"}),
            json!({"episode_number":16,"feature_id":311987,"title":"Civil Wars (2)"}),
        ]);
        metadata["data"][0]["attributes"]["seasons"][0]["episodes"] = json!(rows);
        online.responses[1] = Ok(metadata);
        for n in 1..=12 {
            let mut sub = subtitle(200 + n, 300 + n, false);
            let a = &mut sub["attributes"];
            a["feature_details"]["feature_id"] = json!(100 + n);
            a["feature_details"]["episode_number"] = json!(n);
            a["feature_details"]["title"] = json!(format!("Fixture {n}"));
            a["release"] = json!(format!("Show.S01E{n:02}"));
            a["files"][0]["file_name"] = a["release"].clone();
            online.responses.push_back(Ok(
                json!({"page":1,"total_pages":1,"total_count":1,"data":[sub]}),
            ));
        }
        if range.is_none() {
            online.responses.push_back(Ok(
                json!({"page":1,"total_pages":0,"total_count":0,"data":[]}),
            ));
        }
        let result = catalog::resolve(
            &mut online,
            &Scope::new("Original Show", "1", range).unwrap(),
        );
        if range.is_some() {
            assert_eq!(result.unwrap().selected.len(), 12);
            assert_eq!(online.gets, 14);
        } else {
            assert!(result.unwrap_err().0.contains("S01E13"));
            assert_eq!(online.gets, 15);
        }
        assert_eq!((online.posts, online.contents), (0, 0));
    }
}
#[test]
fn korra_s2_e3_available_identity_is_selected_for_whole_season_and_explicit_range() {
    for range in [None, Some("1-14")] {
        let mut online = Mock::new(None);
        online.responses.push_back(Ok(json!({"data":[{"id":"11896","attributes":{"feature_id":11896,"imdb_id":1695360,"title":"the legend of korra","feature_type":"Tvshow"}}]})));
        let ids = [
            312001, 312003, 514477, 312007, 312004, 311994, 312006, 312008, 312014, 312010, 312016,
            312013, 312009, 312012,
        ];
        let mut rows=ids.iter().enumerate().map(|(i,id)|json!({"episode_number":i+1,"feature_id":id,"title":if i==2{"Civil Wars (1)".to_owned()}else{format!("Synthetic episode {}",i+1)}})).collect::<Vec<_>>();
        rows.push(json!({"episode_number":3,"feature_id":311988,"title":"Civil Wars (1)"}));
        rows.push(rows[2].clone());
        online.responses.push_back(Ok(json!({"data":[{"id":"11896","attributes":{"feature_id":11896,"title":"the legend of korra","seasons":[{"season_number":2,"episodes":rows}]}}]})));
        for (i, id) in ids.iter().enumerate() {
            let mut sub = subtitle(1000 + i as u64, 2000 + i as u64, false);
            let a = &mut sub["attributes"];
            a["feature_details"] = json!({"parent_feature_id":11896,"feature_id":id,"feature_type":"Episode","season_number":2,"episode_number":i+1,"title":if i==2{"Civil Wars (1)".to_owned()}else{format!("Synthetic episode {}",i+1)}});
            a["release"] = json!(format!("Korra.S02E{:02}", i + 1));
            a["files"][0]["file_name"] = a["release"].clone();
            online.responses.push_back(Ok(
                json!({"page":1,"total_pages":1,"total_count":1,"data":[sub]}),
            ));
        }
        let m = catalog::resolve(
            &mut online,
            &Scope::from_imdb("tt1695360", "2", range).unwrap(),
        )
        .unwrap();
        assert_eq!(m.selected.len(), 14);
        assert_eq!(m.selected[2].episode_id, 514477);
        assert_eq!(m.unavailable.len(), 1);
        assert_eq!(m.unavailable[0].episode_id, 311988);
        assert_eq!((online.gets, online.posts, online.contents), (16, 0, 0));
    }
}
