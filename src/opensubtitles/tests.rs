mod alternatives;
mod availability;
mod boundaries;
mod community;
mod compatibility;
mod fallback;
mod framing;
mod locking;
mod ordering;
mod paragraphs;
mod records;
mod series;
mod show_selection;

use super::transport::Transport;
use super::*;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
struct Mock {
    responses: VecDeque<Result<Value>>,
    posts: usize,
    gets: usize,
    contents: usize,
    authenticated: bool,
    quota: Option<u64>,
    body: Vec<u8>,
    paths: Vec<String>,
    bodies: VecDeque<Result<Vec<u8>>>,
    files: Vec<u64>,
}
impl Mock {
    fn new(quota: Option<u64>) -> Self {
        Self {
            responses: VecDeque::new(),
            bodies: VecDeque::new(),
            files: Vec::new(),
            paths: Vec::new(),
            posts: 0,
            gets: 0,
            contents: 0,
            authenticated: quota.is_some(),
            quota,
            body: b"1\n00:00:01,000 --> 00:00:02,000\nOriginal synthetic dialogue only.\n".to_vec(),
        }
    }
}
impl Transport for Mock {
    fn api(&mut self, path: &str, body: Option<Value>) -> Result<Value> {
        self.paths.push(path.into());
        if let Some(body) = body {
            self.files.push(body["file_id"].as_u64().unwrap());
            self.posts += 1;
        } else {
            self.gets += 1;
        }
        self.responses
            .pop_front()
            .unwrap_or_else(|| Err(fail("unexpected HTTP call")))
    }
    fn content(&mut self, _url: &str) -> Result<Vec<u8>> {
        self.contents += 1;
        self.bodies
            .pop_front()
            .unwrap_or_else(|| Ok(self.body.clone()))
    }
    fn authenticated(&self) -> bool {
        self.authenticated
    }
    fn quota(&mut self) -> Result<Option<u64>> {
        Ok(self.quota)
    }
}
fn temp() -> PathBuf {
    static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "tvmatch-acquire-test-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        SERIAL.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ))
}
fn manifest() -> Manifest {
    let scope = Scope::new("Original Show", "1", Some("1-8")).unwrap();
    Manifest {
        show_choice: None,
        policy: POLICY,
        scope,
        show_id: 1,
        show_title: "Original Show".into(),
        unavailable: Vec::new(),
        selected: (1..=8)
            .map(|episode| Selected {
                show_id: 1,
                episode_id: 10 + episode as u64,
                season: 1,
                episode,
                title: format!("Original Episode {episode}"),
                subtitle_id: 20 + episode as u64,
                file_id: 30 + episode as u64,
                language: "en".into(),
                release: format!("Original.S01E{episode:02}"),
                uploader: "Original".into(),
                source: format!(
                    "https://www.opensubtitles.com/en/subtitles/{}",
                    20 + episode
                ),
            })
            .collect(),
    }
}
#[test]
fn frozen_selection_coverage_respects_whole_season_holes_and_explicit_range() {
    let root = temp();
    let cache = Cache::open(&root).unwrap();
    let mut whole = manifest();
    whole.scope = Scope::new("Original Show", "1", None).unwrap();
    whole.selected.retain(|s| [1, 3, 5].contains(&s.episode));
    cache.freeze(&whole).unwrap();
    assert_eq!(
        cache.selected_references(&whole.scope).unwrap(),
        whole.selected
    );
    let mut range = manifest();
    range.scope = Scope::new("Original Show", "1", Some("3-5")).unwrap();
    range.selected.retain(|s| (3..=5).contains(&s.episode));
    cache.freeze(&range).unwrap();
    assert_eq!(
        cache.selected_references(&range.scope).unwrap(),
        range.selected
    );
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn quota_zero_five_eight_twenty_and_offline_zero_http() {
    for quota in [0, 5, 8, 20] {
        let root = temp();
        let cache = Cache::open(&root).unwrap();
        let m = manifest();
        cache.freeze(&m).unwrap();
        let mut mock = Mock::new(Some(quota));
        for i in 1..=8 {
            mock.responses.push_back(Ok(json!({"remaining":quota.saturating_sub(i),"link":"https://www.opensubtitles.com/synthetic","reset_time_utc":"2026-09-11T00:00:00Z"})));
        }
        let result = acquire(&cache, &m, &mut mock);
        if quota < 8 {
            assert!(result.is_err());
            assert_eq!(mock.posts, 0);
        } else {
            result.unwrap();
            assert_eq!(mock.posts, 8);
            assert_eq!(mock.contents, 8);
            let mut offline = Mock::new(None);
            acquire(&cache, &m, &mut offline).unwrap();
            assert_eq!((offline.posts, offline.gets, offline.contents), (0, 0, 0));
            assert_eq!(references(&cache, &m.scope, false).unwrap().len(), 8);
            // Even explicit fetch is an offline cache hit; no credentials needed.
            assert_eq!(references(&cache, &m.scope, true).unwrap().len(), 8);
        }
        drop(cache);
        fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn anonymous_first_reference_checkpoint_and_no_replay() {
    let root = temp();
    let cache = Cache::open(&root).unwrap();
    let m = manifest();
    let mut mock = Mock::new(None);
    mock.responses.push_back(Ok(
        json!({"remaining":4,"link":"https://www.opensubtitles.com/synthetic"}),
    ));
    assert!(acquire(&cache, &m, &mut mock).is_err());
    assert_eq!(mock.posts, 1);
    assert!(cache.contains(&m.selected[0]).unwrap());
    assert_eq!(cache.missing(&m).unwrap().len(), 7);
    // Simulate a timeout after the next potentially chargeable POST, then a rerun.
    mock.quota = Some(20);
    mock.responses
        .push_back(Err(fail("POST uncertain redacted")));
    assert!(acquire(&cache, &m, &mut mock).is_err());
    assert_eq!(mock.posts, 2);
    assert!(acquire(&cache, &m, &mut mock).is_err());
    assert_eq!(mock.posts, 3); // next never-attempted episode, not the failed file
    assert_eq!(mock.files, [31, 32, 33]);
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn corrupt_content_does_not_publish_or_retry() {
    let root = temp();
    let cache = Cache::open(&root).unwrap();
    let mut m = manifest();
    m.scope = scope();
    m.selected.truncate(1);
    let mut mock = Mock::new(Some(20));
    mock.body = vec![255];
    mock.responses.push_back(Ok(
        json!({"remaining":19,"link":"https://www.opensubtitles.com/synthetic"}),
    ));
    assert!(acquire(&cache, &m, &mut mock).is_err());
    assert!(!cache.contains(&m.selected[0]).unwrap());
    assert_eq!(mock.posts, 1);
    assert!(acquire(&cache, &m, &mut mock).is_err());
    assert_eq!(mock.posts, 1);
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
fn show(id: u64) -> Value {
    json!({"id":id.to_string(),"attributes":{"feature_id":id,"title":"Original Show","feature_type":"Tvshow"}})
}
fn detail() -> Value {
    json!({"data":[{"id":"1","attributes":{"feature_id":1,"title":"Original Show","seasons":[{"season_number":1,"episodes":[{"episode_number":1,"feature_id":11,"title":"Original Episode"}]}]}}]})
}
fn subtitle(id: u64, file: u64, trusted: bool) -> Value {
    json!({"id":id.to_string(),"attributes":{"subtitle_id":id.to_string(),"language":"en","feature_details":{"parent_feature_id":1,"feature_id":11,"feature_type":"Episode","season_number":1,"episode_number":1,"title":"Original Episode"},"foreign_parts_only":false,"ai_translated":false,"machine_translated":false,"hearing_impaired":false,"from_trusted":trusted,"download_count":100,"files":[{"file_id":file,"cd_number":1,"file_name":"Original.S01E01.srt"}],"release":"Original.S01E01","uploader":{"name":"Original"}}})
}
fn scope() -> Scope {
    Scope::new("Original Show", "1", Some("1")).unwrap()
}
fn metadata_mock() -> Mock {
    let mut m = Mock::new(None);
    m.responses.push_back(Ok(json!({"data":[show(1)]})));
    m.responses.push_back(Ok(detail()));
    m
}
#[test]
fn psycho_pass_opaque_release_tag_does_not_hide_the_independent_episode_reference() {
    let release = "[Commie] Psycho-Pass - S01E18 [S01E2l12BC8]";
    for conflicting_field in [None, Some("release"), Some("filename"), Some("comments")] {
        let mut online = Mock::new(None);
        online.responses.push_back(Ok(json!({"data":[{"id":"13694","attributes":{"feature_id":13694,"imdb_id":2379308,"title":"psycho-pass","feature_type":"Tvshow"}}]})));
        online.responses.push_back(Ok(json!({"data":[{"id":"13694","attributes":{"feature_id":13694,"title":"psycho-pass","seasons":[{"season_number":1,"episodes":[{"episode_number":18,"feature_id":84384,"title":"A Promise Written on Water"}]}]}}]})));
        let mut candidate = subtitle(6240394, 7152225, false);
        let a = &mut candidate["attributes"];
        a["feature_details"] = json!({"parent_feature_id":13694,"feature_id":84384,"feature_type":"Episode","season_number":1,"episode_number":18,"title":"A Promise Written on Water"});
        a["release"] = json!(release);
        a["files"][0]["file_name"] = json!(release);
        if let Some(field) = conflicting_field {
            let destination = if field == "filename" {
                &mut a["files"][0]["file_name"]
            } else {
                &mut a[field]
            };
            *destination = json!("S01E18 [S01E02]");
        }
        online.responses.push_back(Ok(
            json!({"page":1,"total_pages":1,"total_count":1,"data":[candidate]}),
        ));
        let selected = catalog::resolve(
            &mut online,
            &Scope::from_imdb("tt2379308", "1", Some("18")).unwrap(),
        );
        if conflicting_field.is_some() {
            assert!(
                selected.unwrap_err().0.contains(
                    "no eligible independent English S01E18 reference for episode ID 84384"
                )
            );
        } else {
            let m = selected.unwrap();
            assert_eq!(m.selected.len(), 1);
            assert_eq!(
                (
                    m.selected[0].episode_id,
                    m.selected[0].episode,
                    m.selected[0].file_id
                ),
                (84384, 18, 7152225)
            );
        }
        assert_eq!((online.gets, online.posts, online.contents), (3, 0, 0));
    }
}
#[test]
fn ambiguous_show_ids_missing_episode_and_duplicate_identity() {
    let mut m = Mock::new(None);
    m.responses.push_back(Ok(json!({"data":[show(1),show(2)]})));
    assert!(catalog::resolve(&mut m, &scope()).is_err());
    assert_eq!(m.posts, 0);
    let mut m = metadata_mock();
    let missing = Scope::new("Original Show", "1", Some("1-2")).unwrap();
    assert!(catalog::resolve(&mut m, &missing).is_err());
    let mut m = metadata_mock();
    m.responses.push_back(Ok(json!({"page":1,"total_pages":1,"total_count":2,"data":[subtitle(21,31,true),subtitle(22,31,false)]})));
    assert!(catalog::resolve(&mut m, &scope()).is_err());
}
#[test]
fn pagination_complete_ranking_stable_and_failed_progression() {
    let mut m = metadata_mock();
    m.responses.push_back(Ok(
        json!({"page":1,"total_pages":2,"total_count":2,"data":[subtitle(21,31,false)]}),
    ));
    m.responses.push_back(Ok(
        json!({"page":2,"total_pages":2,"total_count":2,"data":[subtitle(22,32,true)]}),
    ));
    let selected = catalog::resolve(&mut m, &scope()).unwrap();
    assert_eq!(selected.selected[0].file_id, 32);
    assert_eq!(m.posts, 0);
    for (page, total_pages, total_count) in [(1, 2, 2), (2, 11, 2), (2, 2, 3)] {
        let mut m = metadata_mock();
        m.responses.push_back(Ok(
            json!({"page":1,"total_pages":2,"total_count":2,"data":[subtitle(21,31,false)]}),
        ));
        m.responses.push_back(Ok(json!({"page":page,"total_pages":total_pages,"total_count":total_count,"data":[subtitle(22,32,true)]})));
        assert!(catalog::resolve(&mut m, &scope()).is_err());
    }
}
#[test]
fn x_episode_range_candidate_is_rejected_before_download() {
    for field in ["release", "file_name"] {
        for (label, accepted) in [
            ("Original.1x01-02", false),
            ("Original.1x01-1x02", false),
            ("Original.1x01.HDTV", true),
        ] {
            let mut value = subtitle(21, 31, true);
            if field == "release" {
                value["attributes"]["release"] = json!(label);
            } else {
                value["attributes"]["files"][0]["file_name"] = json!(label);
            }
            let mut m = metadata_mock();
            m.responses.push_back(Ok(
                json!({"page":1,"total_pages":1,"total_count":1,"data":[value]}),
            ));
            assert_eq!(
                catalog::resolve(&mut m, &scope()).is_ok(),
                accepted,
                "{field}: {label}"
            );
            assert_eq!((m.posts, m.contents), (0, 0));
        }
    }
}
#[test]
fn wrong_language_episode_title_and_unknown_eligibility_rejected() {
    for field in ["language", "episode", "title", "eligibility"] {
        let mut value = subtitle(21, 31, true);
        match field {
            "language" => value["attributes"]["language"] = json!("fr"),
            "episode" => value["attributes"]["feature_details"]["episode_number"] = json!(2),
            "title" => value["attributes"]["feature_details"]["title"] = json!("Conflicting title"),
            _ => value["attributes"]["ai_translated"] = Value::Null,
        }
        let mut m = metadata_mock();
        m.responses.push_back(Ok(
            json!({"page":1,"total_pages":1,"total_count":1,"data":[value]}),
        ));
        assert!(catalog::resolve(&mut m, &scope()).is_err());
        assert_eq!(m.posts, 0);
    }
}

#[test]
fn imdb_exact_identity_type_missing_and_alias_cache_reuse() {
    let scope = Scope::from_imdb("tt2575988", "1", Some("1-8")).unwrap();
    for (identity, kind, ok) in [
        (json!(2575988), "Tvshow", true),
        (json!(123), "Tvshow", false),
        (Value::Null, "Tvshow", false),
        (json!(2575988), "Movie", false),
    ] {
        let mut value = show(1);
        value["attributes"]["imdb_id"] = identity;
        value["attributes"]["feature_type"] = json!(kind);
        let mut mock = Mock::new(None);
        mock.responses.push_back(Ok(json!({"data":[value]})));
        assert_eq!(catalog::resolve_show(&mut mock, &scope).is_ok(), ok);
        assert_eq!((mock.gets, mock.posts), (1, 0));
        assert_eq!(mock.paths, ["features?imdb_id=2575988&type=tvshow"]);
    }
    let root = temp();
    let c = Cache::open(&root).unwrap();
    let m = manifest();
    c.freeze(&m).unwrap();
    let alias = c.alias(&scope, 1, "Original Show").unwrap().unwrap();
    assert_eq!(alias.selected, m.selected);
    c.freeze(&alias).unwrap();
    assert!(c.alias(&scope, 2, "Original Show").unwrap().is_none());
    let other = Scope::from_imdb("tt2575988", "2", Some("1-8")).unwrap();
    assert!(c.alias(&other, 1, "Original Show").unwrap().is_none());
    assert_eq!(c.manifest(&scope).unwrap().unwrap().selected, m.selected);
    drop(c);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn provider_seasons_22_24_40_52_are_actual_lists_and_admitted_independently_of_quota() {
    for count in [22u32, 24, 40, 52] {
        let scope = Scope::new("Original Show", "1", None).unwrap();
        let mut mock = Mock::new(None);
        mock.responses.push_back(Ok(json!({"data":[show(1)]})));
        let episodes: Vec<_> = (1..=count).map(|n| json!({"episode_number":n,"feature_id":100+n,"title":format!("Original Episode {n}")})).collect();
        mock.responses.push_back(Ok(json!({"data":[{"id":"1","attributes":{"feature_id":1,"title":"Original Show","seasons":[{"season_number":1,"episodes":episodes}]}}]})));
        for n in 1..=count {
            let mut value = subtitle(200 + n as u64, 300 + n as u64, true);
            value["attributes"]["feature_details"]["episode_number"] = json!(n);
            value["attributes"]["feature_details"]["feature_id"] = json!(100 + n);
            value["attributes"]["feature_details"]["title"] =
                json!(format!("Original Episode {n}"));
            value["attributes"]["release"] = json!(format!("Original.S01E{n:02}"));
            value["attributes"]["files"][0]["file_name"] =
                json!(format!("Original.S01E{n:02}.srt"));
            mock.responses.push_back(Ok(
                json!({"page":1,"total_pages":1,"total_count":1,"data":[value]}),
            ));
        }
        let m = catalog::resolve(&mut mock, &scope).unwrap();
        assert_eq!(m.selected.len(), count as usize);
        assert_eq!(mock.posts, 0);
        let root = temp();
        let c = Cache::open(&root).unwrap();
        c.freeze(&m).unwrap();
        assert_eq!(
            c.manifest(&scope).unwrap().unwrap().selected.len(),
            count as usize
        );
        for s in &m.selected {
            c.reserve(s).unwrap();
            c.publish(s, &mock.body).unwrap();
        }
        let refs = c.references(&m).unwrap();
        assert_eq!(
            crate::Index::build_with_reference_limit(refs, 1000)
                .unwrap()
                .stats()
                .references,
            count as usize
        );
        let range = Scope::new("Original Show", "1", Some(&format!("1-{count}"))).unwrap();
        assert_eq!(
            c.alias(&range, 1, "Original Show")
                .unwrap()
                .unwrap()
                .selected
                .len(),
            count as usize
        );
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn full_season_cached_metadata_retains_noncontiguous_provider_numbers() {
    let root = temp();
    let c = Cache::open(&root).unwrap();
    let mut m = manifest();
    m.scope = Scope::new("Original Show", "1", None).unwrap();
    m.selected.retain(|s| matches!(s.episode, 1 | 3 | 8));
    c.freeze(&m).unwrap();
    let scope = Scope::from_imdb("tt2575988", "1", None).unwrap();
    assert_eq!(
        c.alias(&scope, 1, "Original Show")
            .unwrap()
            .unwrap()
            .selected
            .iter()
            .map(|s| s.episode)
            .collect::<Vec<_>>(),
        vec![1, 3, 8]
    );
    let missing = Scope::new("Original Show", "1", Some("1-3")).unwrap();
    assert!(c.alias(&missing, 1, "Original Show").unwrap().is_none());
    assert!(
        c.selected(1, 1, 3, 999, "Original Episode 3")
            .unwrap()
            .is_none()
    );
    assert!(c.selected(1, 1, 3, 13, "Changed title").is_err());
    assert!(c.selected(1, 1, 4, 13, "Original Episode 3").is_err());
    drop(c);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn larger_provider_season_does_not_override_account_quota() {
    let root = temp();
    let c = Cache::open(&root).unwrap();
    let mut m = manifest();
    m.scope = Scope::new("Original Show", "1", None).unwrap();
    m.selected = (1..=52)
        .map(|n| {
            let mut s = m.selected[0].clone();
            s.episode = n;
            s.episode_id = 100 + n as u64;
            s.file_id = 200 + n as u64;
            s.subtitle_id = 300 + n as u64;
            s.source = format!(
                "https://www.opensubtitles.com/en/subtitles/{}",
                s.subtitle_id
            );
            s
        })
        .collect();
    c.freeze(&m).unwrap();
    let mut mock = Mock::new(Some(20));
    assert!(acquire(&c, &m, &mut mock).is_err());
    assert_eq!(mock.posts, 0);
    drop(c);
    fs::remove_dir_all(root).unwrap();
}

fn season13() -> Manifest {
    let mut m = manifest();
    m.scope = Scope::new("Original Show", "1", None).unwrap();
    m.selected = (1..=13)
        .map(|n| {
            let mut s = m.selected[0].clone();
            s.episode = n;
            s.episode_id = 100 + n as u64;
            s.file_id = 200 + n as u64;
            s
        })
        .collect();
    m
}
fn partial_path(root: &std::path::Path, s: &Selected) -> PathBuf {
    root.join(format!(
        "show-{}/season-{}/episode-{}/file-{}.partial",
        s.show_id, s.season, s.episode, s.file_id
    ))
}
#[test]
fn legacy_attempt_only_next_invocation_once_and_complete_eleven_zero_posts() {
    let root = temp();
    let m = season13();
    let cache = Cache::open(&root).unwrap();
    cache.freeze(&m).unwrap();
    let mut mock = Mock::new(Some(20));
    for s in &m.selected[..11] {
        cache.publish(s, &mock.body).unwrap();
    }
    cache.reserve(&m.selected[11]).unwrap();
    drop(cache);
    let cache = Cache::open(&root).unwrap();
    for remaining in [19, 18] {
        mock.responses.push_back(Ok(
            json!({"remaining":remaining,"link":"https://www.opensubtitles.com/synthetic"}),
        ));
    }
    acquire(&cache, &m, &mut mock).unwrap();
    assert_eq!(mock.files, [212, 213]);
    assert_eq!(cache.references(&m).unwrap().len(), 13);
    acquire(&cache, &m, &mut mock).unwrap();
    assert_eq!(mock.posts, 2);
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn episode12_parse_failure_retained_episode13_succeeds_and_rerun_never_redownloads() {
    let root = temp();
    let m = season13();
    let cache = Cache::open(&root).unwrap();
    cache.freeze(&m).unwrap();
    let mut mock = Mock::new(Some(20));
    for s in &m.selected[..11] {
        cache.publish(s, &mock.body).unwrap();
    }
    // A duplicated index remains invalid; a numeric gap is now explicitly repairable.
    let bad = b"1\n00:00:01,000 --> 00:00:02,000\nSynthetic only.\n\n1\n00:00:03,000 --> 00:00:04,000\nMore synthetic.\n".to_vec();
    mock.bodies.push_back(Ok(bad.clone()));
    for remaining in [19, 18] {
        mock.responses.push_back(Ok(
            json!({"remaining":remaining,"link":"https://www.opensubtitles.com/synthetic"}),
        ));
    }
    let error = acquire(&cache, &m, &mut mock).unwrap_err().to_string();
    assert!(error.contains("S01E12 file_id=212"));
    assert!(error.contains("line 5: InvalidIndex"));
    assert!(!error.contains("Synthetic"));
    assert_eq!(mock.files, [212, 213]);
    assert!(cache.contains(&m.selected[12]).unwrap());
    assert_eq!(
        fs::read(partial_path(&root, &m.selected[11]).join("content.srt")).unwrap(),
        bad
    );
    drop(cache);
    let cache = Cache::open(&root).unwrap();
    let mut offline = Mock::new(None);
    assert!(acquire(&cache, &m, &mut offline).is_err());
    assert_eq!((offline.posts, offline.gets, offline.contents), (0, 0, 0));
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn valid_raw_staging_resumes_without_http_and_records_recovery_time_caveat() {
    let root = temp();
    let mut m = manifest();
    m.scope = scope();
    m.selected.truncate(1);
    let cache = Cache::open(&root).unwrap();
    cache.freeze(&m).unwrap();
    cache.reserve(&m.selected[0]).unwrap();
    let partial = cache.stage(&m.selected[0], &Mock::new(None).body).unwrap();
    drop(cache);
    let cache = Cache::open(&root).unwrap();
    assert_eq!(references(&cache, &m.scope, true).unwrap().len(), 1);
    let p: Value = serde_json::from_slice(
        &fs::read(partial.with_extension("").join("provenance.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(p["recovered_without_fetch_metadata"], true);
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn interrupted_raw_body_cannot_recover_from_valid_prefix_or_changed_body() {
    for fault in ["truncated", "changed", "missing", "malformed", "identity"] {
        let root = temp();
        let mut m = manifest();
        m.scope = scope();
        m.selected.truncate(1);
        let cache = Cache::open(&root).unwrap();
        cache.freeze(&m).unwrap();
        cache.reserve(&m.selected[0]).unwrap();
        let first = b"1\n00:00:01,000 --> 00:00:02,000\nOriginal dialogue.\n";
        let mut full = first.to_vec();
        full.extend_from_slice(b"\n2\n00:00:03,000 --> 00:00:04,000\nSecond dialogue.\n");
        assert!(Transcript::parse(std::str::from_utf8(first).unwrap()).is_ok());
        assert!(Transcript::parse(std::str::from_utf8(&full).unwrap()).is_ok());
        let partial = cache.stage(&m.selected[0], &full).unwrap();
        let receipt = partial.join("download.json");
        match fault {
            "truncated" => fs::write(partial.join("content.srt"), first).unwrap(),
            "changed" => {
                let text = String::from_utf8(full)
                    .unwrap()
                    .replace("Original", "Modified");
                fs::write(partial.join("content.srt"), text).unwrap();
            }
            "missing" => fs::remove_file(&receipt).unwrap(),
            "malformed" => fs::write(&receipt, b"{").unwrap(),
            "identity" => {
                let mut value: Value =
                    serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
                value["selected"]["file_id"] = json!(999);
                fs::write(&receipt, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            _ => unreachable!(),
        }
        let retained = fs::read(partial.join("content.srt")).unwrap();
        drop(cache);
        let cache = Cache::open(&root).unwrap();
        let mut mock = Mock::new(None);
        assert!(acquire(&cache, &m, &mut mock).is_err(), "{fault}");
        assert_eq!((mock.posts, mock.gets, mock.contents), (0, 0, 0));
        assert!(!cache.contains(&m.selected[0]).unwrap());
        assert!(!partial.join("provenance.json").exists());
        assert_eq!(fs::read(partial.join("content.srt")).unwrap(), retained);
        drop(cache);
        fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn parse_failure_quota_checkpoint_stops_without_second_post_and_retains_bytes() {
    let root = temp();
    let m = manifest();
    let cache = Cache::open(&root).unwrap();
    let mut mock = Mock::new(None);
    mock.body = vec![255];
    mock.responses.push_back(Ok(
        json!({"remaining":0,"link":"https://www.opensubtitles.com/synthetic"}),
    ));
    assert!(
        acquire(&cache, &m, &mut mock)
            .unwrap_err()
            .to_string()
            .contains("remaining insufficient")
    );
    assert_eq!(mock.posts, 1);
    assert_eq!(
        fs::read(partial_path(&root, &m.selected[0]).join("content.srt")).unwrap(),
        vec![255]
    );
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn content_get_failure_continues_but_systemic_status_stops() {
    for (message, posts) in [
        (
            "HTTPS status=502 host=www.opensubtitles.com; stopped (POST never replayed)",
            2,
        ),
        ("HTTPS GET transport failure (details redacted)", 2),
        (
            "HTTPS status=403 host=www.opensubtitles.com; stopped (POST never replayed)",
            1,
        ),
        (
            "HTTPS status=429 host=www.opensubtitles.com; stopped (POST never replayed)",
            1,
        ),
    ] {
        let root = temp();
        let mut m = manifest();
        m.scope = Scope::new("Original Show", "1", Some("1-2")).unwrap();
        m.selected.truncate(2);
        let cache = Cache::open(&root).unwrap();
        let mut mock = Mock::new(Some(20));
        mock.bodies.push_back(Err(fail(message)));
        for remaining in [19, 18] {
            mock.responses.push_back(Ok(
                json!({"remaining":remaining,"link":"https://www.opensubtitles.com/synthetic"}),
            ));
        }
        assert!(acquire(&cache, &m, &mut mock).is_err());
        assert_eq!(mock.posts, posts);
        assert_eq!(cache.contains(&m.selected[1]).unwrap(), posts == 2);
        assert_eq!(mock.files.iter().filter(|id| **id == 31).count(), 1);
        drop(cache);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn provider_u009d_caption_derivative_preserves_raw_timing_and_rejects_structural_repairs() {
    let raw = "1\r\n00:00:01,000 --> 00:00:02,000\r\nOriginal\u{009d}dialogue.\r\n";
    assert!(Transcript::parse(raw).is_err());
    let Prepared {
        transcript: parsed,
        caption_normalization: policy,
        cue_ordering,
        cue_boundaries,
        zero_duration,
        cue_numbering,
        record_framing,
        missing_text,
        caption_paragraphs,
        source_layout,
    } = transcript(raw.as_bytes()).unwrap();
    assert!(missing_text.is_none());
    assert!(caption_paragraphs.is_none());
    assert!(source_layout.is_none());
    assert!(record_framing.is_none());
    assert!(zero_duration.is_none());
    assert!(cue_numbering.is_none());
    assert!(cue_ordering.is_none());
    assert!(cue_boundaries.is_none());
    assert_eq!(parsed.cues()[0].text, "Original\u{fffd}dialogue.");
    assert_eq!(
        (parsed.cues()[0].start_ms, parsed.cues()[0].end_ms),
        (1000, 2000)
    );
    assert_eq!(policy.unwrap().replacements, 1);
    for bad in [
        "1\u{009d}\n00:00:01,000 --> 00:00:02,000\nCaption\n",
        "1\n00:00:01,000 --> 00:00:02,00\u{009d}\nCaption\n",
        "1\n00:00:01,000 --> 00:00:02,000\n \u{009d}\t\n",
        "1\n00:00:01,000 --> 00:00:02,000\nText\u{009c}only\n",
    ] {
        assert!(transcript(bad.as_bytes()).is_err());
    }
    let root = temp();
    let mut m = manifest();
    m.scope = scope();
    m.selected.truncate(1);
    let cache = Cache::open(&root).unwrap();
    cache.freeze(&m).unwrap();
    cache.reserve(&m.selected[0]).unwrap();
    let partial = cache.stage(&m.selected[0], raw.as_bytes()).unwrap();
    let mut mock = Mock::new(None);
    acquire(&cache, &m, &mut mock).unwrap();
    assert_eq!((mock.posts, mock.gets, mock.contents), (0, 0, 0));
    let entry = partial.with_extension("");
    assert_eq!(fs::read(entry.join("content.srt")).unwrap(), raw.as_bytes());
    let path = entry.join("provenance.json");
    let mut p: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        p["caption_normalization"]["policy"],
        "caption-u009d-to-ufffd-v1"
    );
    assert_eq!(p["caption_normalization"]["replacements"], 1);
    assert_eq!(cache.references(&m).unwrap().len(), 1);
    p["caption_normalization"]["replacements"] = json!(2);
    fs::write(path, serde_json::to_vec(&p).unwrap()).unwrap();
    assert!(cache.contains(&m.selected[0]).is_err());
    drop(cache);
    fs::remove_dir_all(root).unwrap();
}
