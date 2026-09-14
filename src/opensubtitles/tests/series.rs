use super::*;
#[test]
#[ignore = "opt-in local display-metadata enrichment; only features GETs, no subtitle acquisition"]
fn enrich_local_series_display_metadata_only() {
    let Ok(imdb) = std::env::var("TVMATCH_TEST_DISPLAY_IMDB") else {
        return;
    };
    let season = std::env::var("TVMATCH_TEST_DISPLAY_SEASON").unwrap();
    let scope = Scope::from_imdb(&imdb, &season, None).unwrap();
    struct MetadataOnly {
        online: Online,
        gets: usize,
    }
    impl Transport for MetadataOnly {
        fn api(&mut self, path: &str, body: Option<Value>) -> Result<Value> {
            if body.is_some() || !path.starts_with("features?feature_id=") || self.gets >= 1 {
                return Err(fail("display audit refuses non-metadata request"));
            }
            self.gets += 1;
            self.online.api(path, None)
        }
        fn content(&mut self, _: &str) -> Result<Vec<u8>> {
            Err(fail("display audit refuses subtitle downloads"))
        }
        fn authenticated(&self) -> bool {
            false
        }
    }
    let cache = Cache::default_private().unwrap();
    let m = cache
        .manifest(&scope)
        .unwrap()
        .expect("existing frozen scope required");
    cache.pin(&m).unwrap();
    let selected = m.selected.clone();
    let mut online = MetadataOnly {
        online: Online::from_env().unwrap(),
        gets: 0,
    };
    let d = super::super::series::ensure(&cache, &m, &mut online).unwrap();
    assert_eq!(cache.selected_references(&scope).unwrap(), selected);
    println!(
        "series display={}; metadata_gets={}; subtitle POSTs/downloads=0",
        d.prefix(None).unwrap(),
        online.gets
    );
}
fn fixture() -> Manifest {
    let mut m = manifest();
    m.scope = Scope::from_imdb("tt1695360", "1", None).unwrap();
    m.selected.truncate(1);
    m
}
fn response() -> Value {
    json!({"data":[{"id":"1","type":"tvshow","attributes":{"feature_id":1,"imdb_id":1695360,"title":"the legend of korra","original_title":"The Legend of Korra","year":"2012"}}]})
}
fn fetch(v: Value) -> Result<super::super::series::Display> {
    let mut t = Mock::new(None);
    t.responses.push_back(Ok(v));
    let d = super::super::series::fetch(&mut t, &fixture());
    assert_eq!((t.gets, t.posts, t.contents), (1, 0, 0));
    assert_eq!(t.paths, ["features?feature_id=1&type=tvshow"]);
    d
}
#[test]
fn original_title_and_year_are_preserved_not_title_cased_and_override_is_name_only() {
    for title in [
        "The Legend of Korra",
        "PSYCHO-PASS サイコパス",
        "CSI",
        "iZombie",
    ] {
        let mut v = response();
        v["data"][0]["attributes"]["original_title"] = json!(title);
        let d = fetch(v).unwrap();
        assert_eq!(d.prefix(None).unwrap(), format!("{title} (2012)"));
        assert_eq!(d.prefix(Some("My Name")).unwrap(), "My Name (2012)");
    }
    let mut v = response();
    v["data"][0]["attributes"]["year"] = json!(2012);
    assert_eq!(fetch(v).unwrap().year, 2012);
    let mut v = response();
    v["data"][0]["attributes"]["original_title"] = Value::Null;
    let d = fetch(v).unwrap();
    assert!(d.prefix(None).is_err());
    assert_eq!(d.prefix(Some("K")).unwrap(), "K (2012)");
    assert!(d.prefix(Some(&"x".repeat(201))).is_err());
    assert!(d.prefix(Some("bad\u{0007}")).is_err());
    assert_eq!(d.prefix(Some(&"x".repeat(200))).unwrap().len(), 207);
}
#[test]
fn display_metadata_rejects_wrong_identity_type_pagination_and_unknown_or_malformed_year() {
    for year in [
        Value::Null,
        json!(0),
        json!(999),
        json!(10000),
        json!(2012.5),
        json!("2012-2014"),
        json!("2012.0"),
        json!("20xx"),
    ] {
        let mut v = response();
        v["data"][0]["attributes"]["year"] = year;
        assert!(fetch(v).is_err());
    }
    for (field, value) in [
        ("imdb_id", json!(7)),
        ("feature_id", json!(2)),
        ("original_title", json!("bad\u{0007}")),
        ("original_title", json!("x".repeat(201))),
    ] {
        let mut v = response();
        v["data"][0]["attributes"][field] = value;
        assert!(fetch(v).is_err());
    }
    let mut v = response();
    v["data"][0]["type"] = json!("episode");
    assert!(fetch(v).is_err());
    let mut v = response();
    v["total_pages"] = json!(2);
    assert!(fetch(v).is_err());
}
#[test]
fn legacy_cache_display_enrichment_is_separate_and_overrides_never_refreeze_or_redownload() {
    let root = temp();
    let cache = Cache::open(&root).unwrap();
    let m = fixture();
    cache.freeze(&m).unwrap();
    cache.reserve(&m.selected[0]).unwrap();
    let raw = Mock::new(None).body;
    let partial = cache.stage(&m.selected[0], &raw).unwrap();
    let mut offline = Mock::new(None);
    acquire(&cache, &m, &mut offline).unwrap();
    let entry = partial.with_extension("");
    let provenance = fs::read(entry.join("provenance.json")).unwrap();
    let before = fs::read_dir(&root)
        .unwrap()
        .filter_map(|p| {
            let p = p.unwrap().path();
            p.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("request-")
                .then(|| (p.clone(), fs::read(p).unwrap()))
        })
        .collect::<Vec<_>>();
    assert!(references_for_rename(&cache, &m.scope, false, None).is_err());
    assert!(references(&cache, &m.scope, false).is_ok());
    let mut online = Mock::new(None);
    online.responses.push_back(Ok(response()));
    super::super::series::ensure(&cache, &m, &mut online).unwrap();
    super::super::series::ensure(&cache, &m, &mut online).unwrap();
    assert_eq!((online.gets, online.posts, online.contents), (1, 0, 0));
    let profile = fs::read(root.join("series-1.json")).unwrap();
    for name in [None, Some("CSI"), Some("Psycho-Pass")] {
        let (refs, prefix) = references_for_rename(&cache, &m.scope, false, name).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(
            prefix,
            format!("{} (2012)", name.unwrap_or("The Legend of Korra"))
        );
    }
    for (path, bytes) in before {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    assert_eq!(fs::read(root.join("series-1.json")).unwrap(), profile);
    assert_eq!(fs::read(entry.join("provenance.json")).unwrap(), provenance);
    assert_eq!(fs::read(entry.join("content.srt")).unwrap(), raw);
    let mut alias = m.clone();
    alias.scope = Scope::new("Original Show", "1", None).unwrap();
    cache.freeze(&alias).unwrap();
    assert_eq!(
        cache.series_prefix(&alias.scope, None).unwrap(),
        "The Legend of Korra (2012)"
    );
    assert_eq!(
        cache
            .series_prefix(&alias.scope, Some("Original Show"))
            .unwrap(),
        "Original Show (2012)"
    );
    let mut season = m.clone();
    season.scope.season = 2;
    season.selected[0].season = 2;
    cache.freeze(&season).unwrap();
    super::super::series::ensure(&cache, &season, &mut online).unwrap();
    assert_eq!(online.gets, 1);
    for (field, value) in [
        ("policy", json!("wrong")),
        ("show_id", json!(2)),
        ("imdb_id", json!(3)),
        ("year", json!(0)),
        ("original_title", json!("bad\u{0007}")),
        ("unexpected", json!(true)),
    ] {
        let mut v: Value = serde_json::from_slice(&profile).unwrap();
        v[field] = value;
        fs::write(root.join("series-1.json"), serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(references_for_rename(&cache, &m.scope, true, None).is_err());
    }
    fs::write(root.join("series-1.json"), profile).unwrap();
    drop(cache);
    let cache = Cache::open(&root).unwrap();
    assert_eq!(
        cache.series_prefix(&m.scope, None).unwrap(),
        "The Legend of Korra (2012)"
    );
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
