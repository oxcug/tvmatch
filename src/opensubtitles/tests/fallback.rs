use super::*;
use crate::opensubtitles::fallback::Options as FallbackOptions;
const EMPTY: &[u8] = b"1\n00:00:01,000 --> 00:00:02,000\n\n";
struct Ui {
    offered: usize,
    confirmed: usize,
    yes: bool,
}
impl FallbackInteraction for Ui {
    fn offer(&mut self, o: &FallbackOffer) -> Result<()> {
        self.offered += 1;
        assert!(!o.alternatives.is_empty());
        assert!(o.alternatives.len() <= 3);
        assert_eq!(o.empty_records, 1);
        Ok(())
    }
    fn confirm(&mut self, _: &FallbackOffer) -> Result<bool> {
        self.confirmed += 1;
        Ok(self.yes)
    }
}
fn setup() -> (PathBuf, Cache, Manifest, PathBuf) {
    let root = temp();
    let c = Cache::open(&root).unwrap();
    let mut m = manifest();
    m.scope = scope();
    m.selected.truncate(1);
    m.selected[0].title = "Original Episode".into();
    c.freeze(&m).unwrap();
    c.reserve(&m.selected[0]).unwrap();
    let p = c.stage(&m.selected[0], EMPTY).unwrap();
    (root, c, m, p)
}
fn choices(mock: &mut Mock) {
    mock.responses.push_back(Ok(json!({"page":1,"total_pages":1,"total_count":4,"data":[subtitle(21,31,true),subtitle(22,32,true),subtitle(23,33,true),subtitle(24,34,false)]})));
}
fn download(mock: &mut Mock) {
    mock.responses.push_back(Ok(
        json!({"remaining":10,"link":"https://www.opensubtitles.com/synthetic"}),
    ));
}
fn run(
    c: &Cache,
    m: &Manifest,
    mock: &mut Mock,
    ui: &mut impl FallbackInteraction,
    dry_run: bool,
) -> Result<Manifest> {
    acquire_with_fallback(
        c,
        m,
        mock,
        Some(&mut FallbackOptions {
            dry_run,
            interaction: ui,
        }),
    )
}
#[test]
fn initial_empty_download_can_offer_once_but_usable_content_never_triggers_fallback() {
    for empty in [false, true] {
        let root = temp();
        let c = Cache::open(&root).unwrap();
        let mut m = manifest();
        m.scope = scope();
        m.selected.truncate(1);
        m.selected[0].title = "Original Episode".into();
        c.freeze(&m).unwrap();
        let mut mock = Mock::new(None);
        download(&mut mock);
        if empty {
            mock.bodies.push_back(Ok(EMPTY.to_vec()));
            mock.bodies.push_back(Ok(mock.body.clone()));
            choices(&mut mock);
            download(&mut mock);
        }
        let mut ui = Ui {
            offered: 0,
            confirmed: 0,
            yes: true,
        };
        let effective = run(&c, &m, &mut mock, &mut ui, false).unwrap();
        assert_eq!(mock.files, if empty { vec![31, 32] } else { vec![31] });
        assert_eq!(ui.confirmed, usize::from(empty));
        assert_eq!(effective.selected[0].file_id, if empty { 32 } else { 31 });
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn decline_and_dry_run_never_approve_or_post_and_keep_original_selection() {
    for dry in [false, true] {
        let (root, c, m, p) = setup();
        let receipt = fs::read(p.join("download.json")).unwrap();
        let mut mock = Mock::new(None);
        choices(&mut mock);
        let mut ui = Ui {
            offered: 0,
            confirmed: 0,
            yes: dry,
        };
        assert!(run(&c, &m, &mut mock, &mut ui, dry).is_err());
        assert_eq!((ui.offered, ui.confirmed), (1, usize::from(!dry)));
        assert_eq!((mock.gets, mock.posts, mock.contents), (1, 0, 0));
        assert!(!p.parent().unwrap().join("fallback-31-1.json").exists());
        assert_eq!(c.selected_references(&m.scope).unwrap(), m.selected);
        assert_eq!(fs::read(p.join("content.srt")).unwrap(), EMPTY);
        assert_eq!(fs::read(p.join("download.json")).unwrap(), receipt);
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn consented_replacement_is_canonical_offline_and_original_bytes_are_retained() {
    let (root, c, m, p) = setup();
    let mut alias = m.clone();
    alias.scope = Scope::from_imdb("tt123", "1", Some("1")).unwrap();
    c.freeze(&alias).unwrap();
    let receipt = fs::read(p.join("download.json")).unwrap();
    let mut mock = Mock::new(None);
    choices(&mut mock);
    download(&mut mock);
    let mut ui = Ui {
        offered: 0,
        confirmed: 0,
        yes: true,
    };
    let effective = run(&c, &m, &mut mock, &mut ui, false).unwrap();
    assert_eq!(effective.selected[0].file_id, 32);
    assert_eq!((mock.gets, mock.posts, mock.contents), (1, 1, 1));
    assert_eq!(mock.files, vec![32]);
    assert_eq!(ui.confirmed, 1);
    assert_eq!(fs::read(p.join("content.srt")).unwrap(), EMPTY);
    assert_eq!(fs::read(p.join("download.json")).unwrap(), receipt);
    assert_eq!(c.selected_references(&m.scope).unwrap(), m.selected);
    drop(c);
    let c = Cache::open(&root).unwrap();
    assert_eq!(references(&c, &m.scope, false).unwrap().len(), 1);
    assert_eq!(references(&c, &alias.scope, false).unwrap().len(), 1);
    assert_eq!(
        c.effective_manifest(&alias).unwrap().selected,
        effective.selected
    );
    drop(c);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn failed_post_requires_fresh_consent_and_cannot_replay_in_one_session() {
    let (root, c, m, _) = setup();
    let mut mock = Mock::new(None);
    choices(&mut mock);
    mock.responses
        .push_back(Err(fail("simulated POST failure")));
    let mut ui = Ui {
        offered: 0,
        confirmed: 0,
        yes: true,
    };
    assert!(run(&c, &m, &mut mock, &mut ui, false).is_err());
    assert_eq!(mock.posts, 1);
    assert!(references(&c, &m.scope, true).is_err());
    // No extra catalog shopping; the approved pending candidate remains frozen.
    assert!(run(&c, &m, &mut mock, &mut ui, false).is_err());
    assert_eq!((mock.gets, mock.posts), (1, 1));
    drop(c);
    let c = Cache::open(&root).unwrap();
    let mut retry = Mock::new(None);
    let mut no = Ui {
        offered: 0,
        confirmed: 0,
        yes: false,
    };
    assert!(run(&c, &m, &mut retry, &mut no, false).is_err());
    assert_eq!(no.confirmed, 1);
    assert_eq!((retry.gets, retry.posts, retry.contents), (0, 0, 0));
    download(&mut retry);
    no.yes = true;
    assert!(run(&c, &m, &mut retry, &mut no, false).is_ok());
    assert_eq!((retry.gets, retry.posts, retry.contents), (0, 1, 1));
    drop(c);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn previously_approved_complete_body_recovers_without_another_post_or_prompt() {
    let (root, c, m, _) = setup();
    let mut mock = Mock::new(None);
    choices(&mut mock);
    mock.responses
        .push_back(Err(fail("simulated interrupted response")));
    let mut ui = Ui {
        offered: 0,
        confirmed: 0,
        yes: true,
    };
    assert!(run(&c, &m, &mut mock, &mut ui, false).is_err());
    let pending = c.fallback_state(&m.selected[0]).unwrap().pending.unwrap();
    c.stage(&pending.candidate, &mock.body).unwrap();
    drop(c);
    let c = Cache::open(&root).unwrap();
    assert_eq!(references(&c, &m.scope, false).unwrap().len(), 1);
    assert_eq!(c.effective_manifest(&m).unwrap().selected[0].file_id, 32);
    drop(c);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn all_empty_fallbacks_are_one_per_call_and_three_total_not_score_shopping() {
    let (root, c, m, p) = setup();
    let mut ui = Ui {
        offered: 0,
        confirmed: 0,
        yes: true,
    };
    for file in [32, 33, 34] {
        let mut mock = Mock::new(None);
        mock.body = EMPTY.to_vec();
        choices(&mut mock);
        download(&mut mock);
        assert!(run(&c, &m, &mut mock, &mut ui, false).is_err());
        assert_eq!(mock.files, vec![file]);
        assert_eq!((mock.posts, mock.contents), (1, 1));
        assert!(p
            .parent()
            .unwrap()
            .join(format!("file-{file}.partial/content.srt"))
            .exists());
    }
    let mut mock = Mock::new(None);
    assert!(run(&c, &m, &mut mock, &mut ui, false)
        .unwrap_err()
        .0
        .contains("limit"));
    assert_eq!((mock.gets, mock.posts), (0, 0));
    drop(c);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn malformed_or_nonempty_references_and_zero_quota_never_trigger_replacement_posts() {
    for raw in [
        b"invalid SRT".as_slice(),
        b"1\n00:00:00,000 --> 00:00:00,000\nNonempty zero cue.\n",
        b"1\n00:00:02,000 --> 00:00:01,000\n\n",
    ] {
        let root = temp();
        let c = Cache::open(&root).unwrap();
        let mut m = manifest();
        m.scope = scope();
        m.selected.truncate(1);
        c.freeze(&m).unwrap();
        c.reserve(&m.selected[0]).unwrap();
        c.stage(&m.selected[0], raw).unwrap();
        let mut mock = Mock::new(None);
        let mut ui = Ui {
            offered: 0,
            confirmed: 0,
            yes: true,
        };
        let error = run(&c, &m, &mut mock, &mut ui, false).unwrap_err().0;
        assert_eq!((mock.gets, mock.posts, ui.offered), (0, 0, 0));
        assert!(
            error.contains("bytes retained")
                || error.contains("raw staging parse")
                || error.contains("incomplete")
                || error.contains("zero"),
            "{error}"
        );
        drop(c);
        fs::remove_dir_all(root).unwrap();
    }
    let (root, c, m, _) = setup();
    let mut mock = Mock::new(Some(0));
    choices(&mut mock);
    let mut ui = Ui {
        offered: 0,
        confirmed: 0,
        yes: true,
    };
    assert!(run(&c, &m, &mut mock, &mut ui, false).is_err());
    assert_eq!((mock.posts, ui.confirmed), (0, 0));
    drop(c);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn consent_does_not_hold_shared_transaction_and_source_is_revalidated_after_prompt() {
    let (root, c, m, p) = setup();
    struct ChangingUi {
        root: PathBuf,
        body: PathBuf,
    }
    impl FallbackInteraction for ChangingUi {
        fn offer(&mut self, _: &FallbackOffer) -> Result<()> {
            Ok(())
        }
        fn confirm(&mut self, _: &FallbackOffer) -> Result<bool> {
            let other = Cache::open(&self.root)?;
            assert!(other.lock_season(1, 1).is_err());
            other.lock_season(2, 1)?;
            fs::write(&self.body, b"changed while deciding").unwrap();
            Ok(true)
        }
    }
    let mut mock = Mock::new(None);
    choices(&mut mock);
    let mut ui = ChangingUi {
        root: root.clone(),
        body: p.join("content.srt"),
    };
    assert!(run(&c, &m, &mut mock, &mut ui, false).is_err());
    assert_eq!(mock.posts, 0);
    assert!(!p.parent().unwrap().join("fallback-31-1.json").exists());
    drop(c);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn approval_tampering_and_partial_approval_fail_without_fetch_or_refreezing() {
    let (root, c, m, p) = setup();
    let mut mock = Mock::new(None);
    choices(&mut mock);
    download(&mut mock);
    let mut ui = Ui {
        offered: 0,
        confirmed: 0,
        yes: true,
    };
    run(&c, &m, &mut mock, &mut ui, false).unwrap();
    let path = p.parent().unwrap().join("fallback-31-1.json");
    let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for field in [
        "policy",
        "step",
        "origin",
        "from",
        "candidate",
        "proof",
        "approved_unix_seconds",
        "unknown",
    ] {
        let mut changed = original.clone();
        changed[field] = json!("wrong");
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(c.effective_manifest(&m).is_err());
    }
    let mut changed = original.clone();
    changed["proof"]["sha256"] = json!("0".repeat(64));
    fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(c.effective_manifest(&m).is_err());
    fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    fs::write(path.with_extension("partial"), b"incomplete").unwrap();
    assert!(c.effective_manifest(&m).is_err());
    assert_eq!(fs::read(p.join("content.srt")).unwrap(), EMPTY);
    assert_eq!((mock.gets, mock.posts), (1, 1));
    drop(c);
    fs::remove_dir_all(root).unwrap();
}
