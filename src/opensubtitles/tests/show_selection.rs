use super::super::show_selection::{Choice, Options};
use super::*;

#[derive(Default)]
struct Picker {
    offers: Vec<ShowOffer>,
    selected: usize,
    answer: Option<usize>,
    error: bool,
}
impl ShowInteraction for Picker {
    fn offer(&mut self, offer: &ShowOffer) -> Result<()> {
        self.offers.push(offer.clone());
        Ok(())
    }
    fn select(&mut self, _: &ShowOffer) -> Result<Option<usize>> {
        self.selected += 1;
        if self.error {
            Err(fail("synthetic UI error"))
        } else {
            Ok(self.answer)
        }
    }
}
fn row(id: u64, title: &str) -> Value {
    json!({"id":id.to_string(),"attributes":{"feature_id":id,"title":title,"feature_type":"Tvshow","year":"2012","imdb_id":123}})
}
fn page(number: u64, pages: u64, count: u64, rows: Vec<Value>) -> Value {
    json!({"page":number,"total_pages":pages,"total_count":count,"data":rows})
}
fn resolve(
    mock: &mut Mock,
    scope: &Scope,
    picker: &mut Picker,
    dry_run: bool,
) -> Result<super::super::show_selection::Resolution> {
    catalog::resolve_show_with_selection(
        mock,
        scope,
        Some(&mut Options {
            dry_run,
            interaction: picker,
        }),
    )
}
#[test]
fn complete_search_keeps_unique_exact_fast_path_even_after_first_page() {
    let mut mock = Mock::new(None);
    mock.responses.extend([
        Ok(page(
            1,
            2,
            3,
            vec![row(2, "Other Show"), row(3, "Another Show")],
        )),
        Ok(page(2, 2, 3, vec![row(1, "original show")])),
    ]);
    let mut picker = Picker::default();
    let result = resolve(&mut mock, &scope(), &mut picker, true).unwrap();
    assert_eq!((result.id, result.title.as_str()), (1, "original show"));
    assert!(result.choice.is_none());
    assert!(picker.offers.is_empty());
    assert_eq!((mock.gets, mock.posts, mock.contents), (2, 0, 0));
    assert_eq!(
        mock.paths,
        [
            "features?query=Original+Show&type=tvshow",
            "features?query=Original+Show&type=tvshow&page=2"
        ]
    );
}
#[test]
fn same_name_on_later_page_requires_numbered_identity_not_title_dedup() {
    let mut mock = Mock::new(None);
    let mut second = row(2, "Original Show");
    second["attributes"]["year"] = json!(2020);
    second["attributes"]["imdb_id"] = json!(456);
    mock.responses.extend([
        Ok(page(1, 2, 2, vec![row(1, "Original Show")])),
        Ok(page(2, 2, 2, vec![second])),
    ]);
    let mut picker = Picker {
        answer: Some(2),
        ..Picker::default()
    };
    let result = resolve(&mut mock, &scope(), &mut picker, false).unwrap();
    assert_eq!(result.id, 2);
    assert_eq!(picker.selected, 1);
    assert_eq!(picker.offers[0].candidates.len(), 2);
    assert_eq!(picker.offers[0].candidates[1].year, Some(2020));
    assert_eq!(picker.offers[0].candidates[1].imdb_id, Some(456));
    assert!(!picker.offers[0].truncated);
}
#[test]
fn no_exact_is_never_automatic_but_explicit_choice_binds_nonexact_query() {
    for answer in [None, Some(0), Some(2), Some(usize::MAX), Some(1)] {
        let mut mock = Mock::new(None);
        mock.responses
            .push_back(Ok(json!({"data":[row(1, "Other Show")]})));
        let mut picker = Picker {
            answer,
            ..Picker::default()
        };
        let result = resolve(&mut mock, &scope(), &mut picker, false);
        assert_eq!(result.is_ok(), answer == Some(1));
        if let Ok(resolved) = result {
            assert!(resolved.choice.is_some());
        }
        assert_eq!(picker.selected, 1);
        assert_eq!((mock.posts, mock.contents), (0, 0));
    }
    let mut mock = Mock::new(None);
    mock.responses
        .push_back(Ok(json!({"data":[row(1, "Other Show")]})));
    assert!(catalog::resolve_show(&mut mock, &scope()).is_err());
    assert!(mock.paths[0].contains("query_match=exact"));
}
#[test]
fn dry_run_empty_search_and_ui_error_stop_before_catalog_or_download() {
    for (rows, dry_run, error) in [
        (vec![row(1, "Other Show")], true, false),
        (vec![], false, false),
        (vec![row(1, "Other Show")], false, true),
    ] {
        let root = temp();
        let cache = Cache::open(&root).unwrap();
        let mut mock = Mock::new(None);
        mock.responses.push_back(Ok(json!({"data":rows})));
        let mut picker = Picker {
            answer: Some(1),
            error,
            ..Picker::default()
        };
        assert!(
            resolve_manifest(
                &cache,
                &scope(),
                &mut mock,
                Some(&mut Options {
                    dry_run,
                    interaction: &mut picker
                })
            )
            .is_err()
        );
        assert_eq!(picker.offers.len(), 1);
        assert_eq!(picker.selected, usize::from(error));
        assert!(cache.manifest(&scope()).unwrap().is_none());
        assert_eq!((mock.gets, mock.posts, mock.contents), (1, 0, 0));
        drop(cache);
        fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn search_request_result_and_display_caps_never_manufacture_uniqueness() {
    for mode in ["pages", "results", "display", "count", "unknown_pages"] {
        let mut mock = Mock::new(None);
        match mode {
            "pages" => {
                for n in 1..=10 {
                    mock.responses.push_back(Ok(page(
                        n,
                        11,
                        11,
                        vec![row(n, if n == 1 { "Original Show" } else { "Other" })],
                    )));
                }
            }
            "results" => mock.responses.push_back(Ok(page(
                1,
                2,
                1001,
                (1..=1000)
                    .map(|n| row(n, if n == 1 { "Original Show" } else { "Other" }))
                    .collect(),
            ))),
            "display" => mock.responses.push_back(Ok(page(
                1,
                1,
                6,
                (1..=6).map(|n| row(n, "Original Show")).collect(),
            ))),
            "count" => mock
                .responses
                .push_back(Ok(json!({"total_count":2,"data":[row(1,"Original Show")]}))),
            _ => mock
                .responses
                .push_back(Ok(json!({"page":1,"data":[row(1,"Original Show")]}))),
        }
        let mut picker = Picker {
            answer: Some(1),
            ..Picker::default()
        };
        let result = resolve(&mut mock, &scope(), &mut picker, false).unwrap();
        assert_eq!(result.id, 1);
        assert_eq!(picker.selected, 1, "{mode}");
        assert!(picker.offers[0].truncated);
        assert!(picker.offers[0].candidates.len() <= 5);
        assert_eq!(mock.gets, if mode == "pages" { 10 } else { 1 });
        assert_eq!((mock.posts, mock.contents), (0, 0));
    }
}
#[test]
fn invalid_search_identity_metadata_and_pagination_fail_before_offering() {
    let base = row(1, "Original Show");
    let mut cases = Vec::new();
    for (field, value) in [
        ("feature_id", json!(2)),
        ("feature_type", json!("Movie")),
        ("title", json!("bad\u{0007}")),
        ("title", json!("x".repeat(201))),
        ("year", json!("2012-2014")),
        ("imdb_id", json!(0)),
    ] {
        let mut bad = base.clone();
        bad["attributes"][field] = value;
        cases.push(vec![json!({"data":[bad]})]);
    }
    for second in [
        page(1, 2, 2, vec![row(2, "Original Show")]),
        page(2, 3, 2, vec![row(2, "Original Show")]),
        page(2, 2, 3, vec![row(2, "Original Show")]),
        page(2, 2, 2, vec![base.clone()]),
    ] {
        cases.push(vec![page(1, 2, 2, vec![base.clone()]), second]);
    }
    cases.push(vec![json!({"total_pages":2,"data":[base.clone()]})]);
    cases.push(vec![page(1, 2, 2, vec![])]);
    cases.push(vec![page(1, 1, 0, vec![base.clone()])]);
    for responses in cases {
        let mut mock = Mock::new(None);
        mock.responses.extend(responses.into_iter().map(Ok));
        let mut picker = Picker {
            answer: Some(1),
            ..Picker::default()
        };
        assert!(resolve(&mut mock, &scope(), &mut picker, false).is_err());
        assert!(picker.offers.is_empty());
        assert_eq!((mock.posts, mock.contents), (0, 0));
    }
    let mut mock = Mock::new(None);
    mock.responses.extend([
        Ok(page(1, 2, 2, vec![base])),
        Err(fail("synthetic GET failed")),
    ]);
    let mut picker = Picker::default();
    assert!(resolve(&mut mock, &scope(), &mut picker, false).is_err());
    assert!(picker.offers.is_empty());
}
#[test]
fn explicit_imdb_never_invokes_picker_even_with_no_unique_result() {
    let scope = Scope::from_imdb("tt123", "1", Some("1")).unwrap();
    for (rows, ok) in [
        (vec![row(1, "Other")], true),
        (vec![], false),
        (vec![row(1, "Other"), row(2, "Other")], false),
    ] {
        let mut mock = Mock::new(None);
        mock.responses.push_back(Ok(json!({"data":rows})));
        let mut picker = Picker {
            answer: Some(1),
            ..Picker::default()
        };
        assert_eq!(resolve(&mut mock, &scope, &mut picker, false).is_ok(), ok);
        assert!(picker.offers.is_empty());
        assert_eq!(mock.paths, ["features?imdb_id=123&type=tvshow"]);
    }
}
#[test]
fn explicit_nonexact_choice_is_immutable_reopens_and_only_aliases_with_bound_consent() {
    let root = temp();
    let cache = Cache::open(&root).unwrap();
    let original = manifest();
    cache.freeze(&original).unwrap();
    let query = Scope::new("Original", "1", Some("1")).unwrap();
    let mut mock = Mock::new(None);
    mock.responses
        .push_back(Ok(json!({"data":[row(1, "Original Show")]})));
    let mut picker = Picker {
        answer: Some(1),
        ..Picker::default()
    };
    let chosen = resolve_manifest(
        &cache,
        &query,
        &mut mock,
        Some(&mut Options {
            dry_run: false,
            interaction: &mut picker,
        }),
    )
    .unwrap();
    assert_eq!(mock.gets, 1); // same-ID frozen subtitles reused, never reselected
    assert!(chosen.show_choice.is_some());
    let before = serde_json::to_vec(&chosen).unwrap();
    let unrelated = Scope::new("Orig", "1", Some("1")).unwrap();
    assert!(
        cache
            .alias(&unrelated, 1, "Original Show")
            .unwrap()
            .is_none()
    );
    let range = Scope::new("Original", "1", Some("1")).unwrap();
    assert!(cache.alias(&range, 1, "Original Show").unwrap().is_some());
    let imdb = Scope::from_imdb("tt123", "1", Some("1")).unwrap();
    let alias = cache.alias(&imdb, 1, "Original Show").unwrap().unwrap();
    assert!(alias.show_choice.is_none());
    assert_eq!(alias.selected, chosen.selected);
    assert_eq!(
        serde_json::to_vec(&cache.manifest(&original.scope).unwrap().unwrap()).unwrap(),
        serde_json::to_vec(&original).unwrap()
    );
    drop(cache);
    let cache = Cache::open(&root).unwrap();
    let mut offline = Mock::new(None);
    let mut never = Picker {
        answer: Some(2),
        ..Picker::default()
    };
    let reopened = resolve_manifest(
        &cache,
        &query,
        &mut offline,
        Some(&mut Options {
            dry_run: false,
            interaction: &mut never,
        }),
    )
    .unwrap();
    assert_eq!(serde_json::to_vec(&reopened).unwrap(), before);
    assert!(never.offers.is_empty());
    assert_eq!(offline.gets, 0);
    // Complete cached requests bypass both interaction UIs and Online entirely;
    // name overrides still affect display only, not the stored chosen identity.
    cache.reserve(&chosen.selected[0]).unwrap();
    cache.stage(&chosen.selected[0], &offline.body).unwrap();
    acquire(&cache, &chosen, &mut offline).unwrap();
    cache
        .store_display(
            &chosen,
            &super::super::series::Display {
                policy: "provider-series-display-v1".into(),
                show_id: 1,
                imdb_id: Some(123),
                original_title: Some("Original Show".into()),
                year: 2012,
            },
        )
        .unwrap();
    struct NeverFallback;
    impl FallbackInteraction for NeverFallback {
        fn offer(&mut self, _: &FallbackOffer) -> Result<()> {
            panic!("unexpected fallback")
        }
        fn confirm(&mut self, _: &FallbackOffer) -> Result<bool> {
            panic!("unexpected fallback consent")
        }
    }
    for name in [None, Some("My Display Name")] {
        let (references, prefix) = references_for_rename_with_interactions(
            &cache,
            &query,
            true,
            name,
            true,
            &mut never,
            &mut NeverFallback,
        )
        .unwrap();
        assert_eq!(references.len(), 1);
        assert_eq!(
            prefix,
            format!("{} (2012)", name.unwrap_or("Original Show"))
        );
        assert!(never.offers.is_empty());
        assert_eq!(
            serde_json::to_vec(&cache.manifest(&query).unwrap().unwrap()).unwrap(),
            before
        );
    }
    for (field, value) in [
        ("policy", json!("wrong")),
        ("query", json!("Orig")),
        ("show_id", json!(2)),
        ("title", json!("other")),
        ("unknown", json!(true)),
    ] {
        let mut value_manifest = serde_json::to_value(&chosen).unwrap();
        value_manifest["show_choice"][field] = value;
        let decoded = serde_json::from_value::<Manifest>(value_manifest);
        assert!(
            decoded.is_err() || decoded.unwrap().validate(&query).is_err(),
            "{field}"
        );
    }
    let mut missing = chosen.clone();
    missing.show_choice = None;
    assert!(missing.validate(&query).is_err());
    let bad_candidate = ShowCandidate {
        show_id: 2,
        title: "Original Show".into(),
        year: None,
        imdb_id: None,
    };
    assert!(
        cache
            .alias_with_choice(
                &unrelated,
                1,
                "Original Show",
                Choice::new(&unrelated, &bad_candidate)
            )
            .unwrap()
            .is_none()
    );
    let mut changed = chosen.clone();
    changed.show_title = "Other".into();
    changed.show_choice = Choice::new(
        &query,
        &ShowCandidate {
            show_id: 1,
            title: "Other".into(),
            year: None,
            imdb_id: None,
        },
    );
    assert!(cache.freeze(&changed).is_err());
    assert_eq!(
        serde_json::to_vec(&cache.manifest(&query).unwrap().unwrap()).unwrap(),
        before
    );
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn fresh_nonexact_selection_keeps_provider_title_through_season_catalog() {
    let root = temp();
    let cache = Cache::open(&root).unwrap();
    let query = Scope::new("Original", "1", Some("1")).unwrap();
    let mut mock = metadata_mock();
    mock.responses
        .push_back(Ok(page(1, 1, 1, vec![subtitle(21, 31, true)])));
    let mut picker = Picker {
        answer: Some(1),
        ..Picker::default()
    };
    let chosen = resolve_manifest(
        &cache,
        &query,
        &mut mock,
        Some(&mut Options {
            dry_run: false,
            interaction: &mut picker,
        }),
    )
    .unwrap();
    assert_eq!(chosen.show_title, "Original Show");
    assert_eq!(chosen.scope.show, "Original");
    assert!(chosen.show_choice.is_some());
    assert_eq!((mock.gets, mock.posts, mock.contents), (3, 0, 0));
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn paused_show_prompt_does_not_hold_global_cache_transaction() {
    use std::{sync::mpsc, thread, time::Duration};
    struct Paused {
        entered: mpsc::Sender<()>,
        resume: mpsc::Receiver<()>,
    }
    impl ShowInteraction for Paused {
        fn offer(&mut self, _: &ShowOffer) -> Result<()> {
            Ok(())
        }
        fn select(&mut self, _: &ShowOffer) -> Result<Option<usize>> {
            self.entered.send(()).unwrap();
            self.resume.recv_timeout(Duration::from_secs(15)).unwrap();
            Ok(None)
        }
    }
    let root = temp();
    let worker_root = root.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let cache = Cache::open(&worker_root).unwrap();
        let mut mock = Mock::new(None);
        mock.responses
            .push_back(Ok(json!({"data":[row(1,"Other")]})));
        let mut picker = Paused {
            entered: entered_tx,
            resume: resume_rx,
        };
        assert!(
            resolve_manifest(
                &cache,
                &scope(),
                &mut mock,
                Some(&mut Options {
                    dry_run: false,
                    interaction: &mut picker
                })
            )
            .is_err()
        );
        assert_eq!((mock.gets, mock.posts, mock.contents), (1, 0, 0));
    });
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let cache = Cache::open(&root).unwrap();
    cache.freeze(&manifest()).unwrap();
    resume_tx.send(()).unwrap();
    worker.join().unwrap();
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
