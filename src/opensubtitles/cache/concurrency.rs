use super::tests::{selected, temp};
use super::*;
const BODY: &[u8] = b"1\n00:00:01,000 --> 00:00:02,000\nOriginal concurrency fixture.\n";
fn manifest(show: u64, season: u32) -> Manifest {
    let mut s = selected();
    s.show_id = show;
    s.season = season;
    s.episode_id = show * 10000 + u64::from(season) * 100 + 1;
    s.file_id = s.episode_id + 1;
    Manifest {
        show_choice: None,
        policy: super::super::POLICY,
        scope: Scope::new(&format!("Show {show}"), &season.to_string(), Some("1")).unwrap(),
        show_id: show,
        show_title: format!("Show {show}"),
        selected: vec![s],
        unavailable: Vec::new(),
    }
}
#[test]
fn season_ownership_converges_aliases_but_other_seasons_and_shows_can_publish() {
    let root = temp();
    let a = Cache::open(&root).unwrap();
    let m = manifest(1, 1);
    a.freeze(&m).unwrap();
    a.prepare(&m).unwrap();
    let b = Cache::open(&root).unwrap();
    assert!(
        b.manifest(&m.scope)
            .unwrap_err()
            .0
            .contains("show 1 season 1")
    );
    let imdb = Scope::from_imdb("tt1234567", "1", Some("1")).unwrap();
    assert!(b.alias(&imdb, 1, "Show 1").is_err());
    assert!(
        b.selected(1, 1, 1, m.selected[0].episode_id, &m.selected[0].title)
            .is_err()
    );
    for (show, season) in [(1, 2), (2, 1)] {
        let other = manifest(show, season);
        b.freeze(&other).unwrap();
        b.prepare(&other).unwrap();
        b.reserve(&other.selected[0]).unwrap();
        b.publish(&other.selected[0], BODY).unwrap();
        assert_eq!(b.references(&other).unwrap().len(), 1);
    }
    a.reserve(&m.selected[0]).unwrap();
    a.publish(&m.selected[0], BODY).unwrap();
    assert!(b.contains(&m.selected[0]).is_err());
    drop(a);
    assert!(b.contains(&m.selected[0]).unwrap());
    drop(b);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn outstanding_capacity_is_shared_and_consumed_without_evicting_another_active_season() {
    let root = temp();
    let a = Cache::open(&root).unwrap();
    let mut b = Cache::open(&root).unwrap();
    let ma = manifest(1, 1);
    let mb = manifest(2, 1);
    a.freeze(&ma).unwrap();
    b.freeze(&mb).unwrap();
    let one = crate::srt::MAX_SRT_BYTES as u64 + 2 * MANIFEST_CAP as u64 + ATTEMPT.len() as u64;
    b.cap = a.inventory().unwrap().total + one;
    a.prepare(&ma).unwrap();
    let total = a.inventory().unwrap().total;
    assert!(b.prepare(&mb).is_err());
    assert!(
        !b.entry(&mb.selected[0])
            .unwrap()
            .with_extension("attempt")
            .exists()
    );
    assert_eq!(a.locks.budget((1, 1)).unwrap(), one);
    a.reserve(&ma.selected[0]).unwrap();
    a.publish(&ma.selected[0], BODY).unwrap();
    let used = a.inventory().unwrap().total - total;
    assert!(a.locks.budget((1, 1)).unwrap() + used <= one);
    // Even unpinning locally cannot let another owner evict this live season.
    a.pins.borrow_mut().clear();
    b.cap = 1;
    assert!(b.space(0).is_err());
    assert!(a.contains(&ma.selected[0]).unwrap());
    b.cap = total + one;
    drop(a); // Stale reservation bytes cease to count on OS unlock.
    b.prepare(&mb).unwrap();
    b.reserve(&mb.selected[0]).unwrap();
    b.publish(&mb.selected[0], BODY).unwrap();
    assert!(b.inventory().unwrap().total <= b.cap);
    drop(b);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn legacy_lock_is_preserved_and_new_protocol_fences_legacy_clients() {
    let root = temp();
    ensure_dir(&root).unwrap();
    new_file(&root.join(".lock"), b"").unwrap();
    assert!(Cache::open(&root).is_err());
    assert_eq!(fs::read(root.join(".lock")).unwrap(), b"");
    // Only this synthetic fixture's owner removes its own unheld legacy marker.
    fs::remove_file(root.join(".lock")).unwrap();
    let a = Cache::open(&root).unwrap();
    assert!(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.join(".lock"))
            .is_err()
    );
    let b = Cache::open(&root).unwrap();
    drop(a);
    drop(b);
    assert!(root.join(".lock").exists());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn malformed_active_reservation_fails_closed_but_inactive_crash_state_is_ignored() {
    let root = temp();
    let a = Cache::open(&root).unwrap();
    a.lock_season(1, 1).unwrap();
    let budget = root.join(".locks-v2/show-1-season-1.budget");
    fs::write(&budget, b"bad").unwrap();
    assert!(Cache::open(&root).is_err());
    drop(a);
    let b = Cache::open(&root).unwrap();
    b.lock_season(1, 1).unwrap();
    assert_eq!(b.locks.budget((1, 1)).unwrap(), 0);
    drop(b);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn process_exit_releases_season_and_housekeeping_locks_without_unlinking() {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let root = temp();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "opensubtitles::cache::concurrency::process_helper",
            "--ignored",
            "--nocapture",
        ])
        .env("TVMATCH_SCOPED_LOCK_FIXTURE", &root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.join("ready").exists() {
        assert!(Instant::now() < deadline, "child readiness timed out");
        assert!(child.try_wait().unwrap().is_none(), "child exited early");
        std::thread::sleep(Duration::from_millis(10));
    }
    let b = Cache::open(&root).unwrap();
    assert!(b.lock_season(1, 1).is_err());
    b.lock_season(1, 2).unwrap();
    assert!(Cache::open_with_cap(&root, 64 * 1024).is_err());
    fs::write(root.join("release"), b"synthetic release").unwrap();
    assert!(child.wait().unwrap().success());
    let c = Cache::open_with_cap(&root, 64 * 1024).unwrap();
    c.lock_season(1, 1).unwrap();
    assert_eq!(c.locks.budget((1, 1)).unwrap(), 0);
    assert!(root.join(".locks-v2/show-1-season-1.lock").exists());
    drop(c);
    drop(b);
    fs::remove_dir_all(root).unwrap();
}
#[test]
#[ignore = "owned subprocess helper; run only by its parent fixture"]
fn process_helper() {
    use std::time::{Duration, Instant};
    let Some(root) = std::env::var_os("TVMATCH_SCOPED_LOCK_FIXTURE") else {
        return;
    };
    let root = PathBuf::from(root);
    assert!(root.starts_with(std::env::temp_dir()));
    assert!(
        root.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("tvmatch-cache-test-")
    );
    let a = Cache::open(&root).unwrap();
    let m = manifest(1, 1);
    a.freeze(&m).unwrap();
    a.prepare(&m).unwrap();
    fs::write(root.join("ready"), b"synthetic ready").unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !root.join("release").exists() {
        if Instant::now() > deadline {
            std::process::exit(2);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _transaction = a.locks.transaction().unwrap();
    std::process::exit(0); // Deliberately no Rust destructors for either lock.
}
