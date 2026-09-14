mod dependencies;

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "tvmatch-renames-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn scan(&self, file: &str, outcome: MatchOutcome) -> Scan {
        let path = self.0.join(file);
        fs::write(&path, b"original synthetic bytes").unwrap();
        Scan {
            series: "Silicon Valley".into(),
            snapshot: Some(Snapshot::take(&path).unwrap()),
            path,
            outcome: Ok(outcome),
        }
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn identified(title: &str) -> MatchOutcome {
    // Real matcher gate, not a fabricated production success or filename-based identity.
    let srt = "1\n00:00:00,000 --> 00:00:01,000\nAmber lanterns illuminate quiet gardens\n\n2\n00:00:10,000 --> 00:00:11,000\nSilver otters navigate winding rivers\n\n3\n00:00:20,000 --> 00:00:21,000\nVelvet clouds surround distant mountains\n";
    let transcript = tvmatch::srt::Transcript::parse(srt).unwrap();
    let reference = tvmatch::Reference::new(
        tvmatch::ReferenceId::new("opensubtitles", "123").unwrap(),
        &format!("S01E08 {title}"),
        "original synthetic test",
        transcript.clone(),
    )
    .unwrap();
    let outcome = tvmatch::Index::build(vec![reference])
        .unwrap()
        .match_query(&transcript)
        .unwrap();
    assert!(matches!(outcome, MatchOutcome::Identified { .. }));
    outcome
}
fn unknown() -> MatchOutcome {
    MatchOutcome::Unknown {
        candidates: vec![],
        reasons: vec![],
    }
}
fn ambiguous() -> MatchOutcome {
    let MatchOutcome::Identified { best, .. } = identified("Title") else {
        unreachable!()
    };
    MatchOutcome::Ambiguous {
        candidates: vec![best],
        reason: tvmatch::RejectionReason::CompetingEvidence,
    }
}
#[test]
fn coverage_uses_expected_ids_not_filenames_and_counts_conflicted_identifications_once() {
    let t = Temp::new();
    let scans = vec![
        t.scan("a.mkv", identified("Title")),
        t.scan("b.mkv", identified("Title")),
        t.scan("S01E01.mkv", unknown()),
    ];
    let id = |s| tvmatch::ReferenceId::new("opensubtitles", s).unwrap();
    let plan = Plan::build(scans).with_expected_episodes(vec![
        ExpectedEpisode {
            season: 1,
            number: 1,
            references: vec![id("other1")],
        },
        ExpectedEpisode {
            season: 1,
            number: 8,
            references: vec![id("alternate"), id("123")],
        },
        ExpectedEpisode {
            season: 1,
            number: 10,
            references: vec![id("other10")],
        },
    ]);
    assert!(matches!(plan.entries[0].state, State::Conflict(_)));
    let preview = plan.preview();
    assert!(preview.contains("Episode coverage: 1/3 confidently identified"));
    assert!(preview.contains("Missing matches: S01E01, S01E10"));
    assert!(!preview.contains("S01E02")); // No assumed contiguous 1..N season.
    assert!(preview.contains("⛔") && preview.contains("❓"));
    for symbol in ["✅", "📝", "⛔", "❓", "⚠️", "❌"] {
        assert!(!plan.summary(false).0.contains(symbol));
    }
}
#[test]
fn coverage_does_not_count_ambiguous_candidates_and_can_report_a_complete_range() {
    let t = Temp::new();
    let id = tvmatch::ReferenceId::new("opensubtitles", "123").unwrap();
    let expected = || {
        vec![ExpectedEpisode {
            season: 1,
            number: 8,
            references: vec![id.clone()],
        }]
    };
    let plan =
        Plan::build(vec![t.scan("ambiguous.mkv", ambiguous())]).with_expected_episodes(expected());
    assert!(plan.preview().contains("Missing matches: S01E08"));
    let plan = Plan::build(vec![t.scan("identified.mkv", identified("Title"))])
        .with_expected_episodes(expected());
    assert!(
        plan.preview()
            .contains("1/1 confidently identified; no missing matches")
    );
    let mut out = Vec::new();
    plan.dry_run(&mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.find("Episode coverage:").unwrap() < out.find("Dry run;").unwrap());
    assert!(!out.contains("Apply renames?"));
}
#[test]
fn stdin_confirmation_bounded_and_explicit_only() {
    for s in ["y\n", "YES\r\n", "yes\n", " Y \n"] {
        assert!(confirm(&mut s.as_bytes()).unwrap(), "{s:?}");
    }
    for s in [
        "",
        "\n",
        "n\n",
        "no\n",
        "invalid\n",
        "y",
        "yes",
        "y trailing\n",
        &format!("y{}\n", " ".repeat(65)),
    ] {
        assert!(!confirm(&mut s.as_bytes()).unwrap(), "{s:?}");
    }
    struct Broken;
    impl io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("stdin broken"))
        }
    }
    assert!(confirm(&mut Broken).is_err());
}
#[test]
fn declined_or_failed_input_never_mutates_and_prompt_is_flushed_before_read() {
    use std::{cell::Cell, rc::Rc};
    struct Output(Rc<Cell<bool>>);
    impl io::Write for Output {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0.set(true);
            Ok(())
        }
    }
    struct Input<'a> {
        flushed: Rc<Cell<bool>>,
        bytes: &'a [u8],
        broken: bool,
    }
    impl io::Read for Input<'_> {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            assert!(
                self.flushed.get(),
                "prompt must be visible before waiting for stdin"
            );
            if self.broken {
                return Err(io::Error::other("synthetic stdin failure"));
            }
            self.bytes.read(bytes)
        }
    }
    for answer in ["", "\n", "n\n", "not yes\n", "y", "yes please\n"] {
        for broken in [false, true] {
            let t = Temp::new();
            let mut plan = Plan::build(vec![t.scan("input.mkv", identified("Title"))]);
            let flushed = Rc::new(Cell::new(false));
            let result = plan.finish(
                &mut Input {
                    flushed: flushed.clone(),
                    bytes: answer.as_bytes(),
                    broken,
                },
                &mut Output(flushed),
            );
            assert_eq!(result.is_err(), broken);
            assert_eq!(
                fs::read(&plan.entries[0].scan.path).unwrap(),
                b"original synthetic bytes"
            );
            assert!(!plan.entries[0].target.as_ref().unwrap().exists());
        }
    }
}
#[test]
fn revalidate_after_confirmation_without_a_second_scan() {
    struct Reply {
        target: PathBuf,
        replied: bool,
    }
    impl io::Read for Reply {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if !self.replied {
                // Another process creates the destination while the user considers the preview.
                fs::write(&self.target, b"new unrelated file")?;
                self.replied = true;
                bytes[0] = b'y';
            } else {
                bytes[0] = b'\n';
            }
            Ok(1)
        }
    }
    let t = Temp::new();
    let mut plan = Plan::build(vec![t.scan("input.mkv", identified("Title"))]);
    let target = plan.entries[0].target.clone().unwrap();
    let mut output = Vec::new();
    assert_eq!(
        plan.finish(
            &mut Reply {
                target: target.clone(),
                replied: false
            },
            &mut output
        )
        .unwrap(),
        1
    );
    assert_eq!(fs::read(target).unwrap(), b"new unrelated file");
    assert_eq!(
        fs::read(&plan.entries[0].scan.path).unwrap(),
        b"original synthetic bytes"
    );
    let output = String::from_utf8(output).unwrap();
    assert_eq!(output.matches("Apply renames?").count(), 1);
    assert!(output.contains("Rename failed:"));
}
#[test]
fn preview_decline_and_apply_preserve_bytes_and_metadata_idempotently() {
    let t = Temp::new();
    let mut p = Plan::build(vec![
        t.scan("source.MKV", identified("Optimal Tip-to-Tip Efficiency")),
    ]);
    let original = p.entries[0].scan.path.clone();
    let target = p.entries[0].target.clone().unwrap();
    let before = Snapshot::take(&original).unwrap();
    let mut out = Vec::new();
    assert_eq!(p.finish(&mut &b"\n"[..], &mut out).unwrap(), 0);
    assert!(original.exists());
    assert!(!target.exists());
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("Apply renames? (y/N): "));
    assert!(text.contains("Silicon Valley - S01E08 - Optimal Tip-to-Tip Efficiency.MKV"));
    assert!(!text.contains("No changes needed"));
    p.finish(&mut &b"yes\n"[..], &mut Vec::new()).unwrap();
    assert!(!original.exists());
    assert_eq!(fs::read(&target).unwrap(), b"original synthetic bytes");
    let after = Snapshot::take(&target).unwrap();
    assert_eq!(before.identity, after.identity);
    assert_eq!(before.modified, after.modified);
    let mut p = Plan::build(vec![Scan {
        series: "Silicon Valley".into(),
        snapshot: Some(after),
        path: target,
        outcome: Ok(identified("Optimal Tip-to-Tip Efficiency")),
    }]);
    let mut out = Vec::new();
    p.finish(&mut &b"yes\n"[..], &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("No changes needed"));
    assert!(!text.contains("Apply renames?"));
}
#[test]
fn existing_stationary_duplicate_sanitized_and_case_targets_conflict() {
    for (a, b) in [
        ("Same", "Same"),
        ("Bad/Title", "Bad:Title"),
        ("Title", "TITLE"),
    ] {
        let t = Temp::new();
        let mut p = Plan::build(vec![
            t.scan("a.mkv", identified(a)),
            t.scan("b.mkv", identified(b)),
        ]);
        assert!(
            p.entries
                .iter()
                .all(|e| matches!(e.state, State::Conflict(_)))
        );
        p.apply();
        assert!(t.0.join("a.mkv").exists());
        assert!(t.0.join("b.mkv").exists());
        assert_eq!(p.summary(true).1, 1);
    }
    for directory in [false, true] {
        let t = Temp::new();
        let scan = t.scan("a.mkv", identified("Title"));
        let target = t.0.join("Silicon Valley - S01E08 - Title.mkv");
        if directory {
            fs::create_dir(&target).unwrap();
        } else {
            fs::write(&target, b"existing").unwrap();
        }
        let p = Plan::build(vec![scan]);
        assert!(matches!(p.entries[0].state, State::Conflict(_)));
    }
}
#[test]
fn wait_revalidation_changed_deleted_and_new_destination_partial_summary() {
    for action in 0..3 {
        let t = Temp::new();
        let mut p = Plan::build(vec![
            t.scan("a.mkv", identified("One")),
            t.scan("b.mkv", identified("Two")),
        ]);
        match action {
            0 => fs::write(&p.entries[1].scan.path, b"changed").unwrap(),
            1 => fs::remove_file(&p.entries[1].scan.path).unwrap(),
            _ => fs::write(p.entries[1].target.as_ref().unwrap(), b"racing destination").unwrap(),
        }
        p.apply();
        assert_eq!(p.entries[0].state, State::Applied);
        assert!(matches!(p.entries[1].state, State::Failed(_)));
        let (summary, code) = p.summary(true);
        assert_eq!(code, 1);
        assert!(summary.contains("1 applied, 1 failed"));
        assert!(summary.contains("Apply incomplete"));
        if action == 2 {
            assert_eq!(
                fs::read(p.entries[1].target.as_ref().unwrap()).unwrap(),
                b"racing destination"
            );
        }
    }
}
#[test]
fn unknown_ambiguous_errors_untouched_and_truthful_rendering() {
    let t = Temp::new();
    let mut p = Plan::build(vec![
        t.scan("unknown.mkv", unknown()),
        t.scan("ambiguous.mkv", ambiguous()),
        t.scan("known.mkv", identified("Title")),
    ]);
    p.entries.push(Entry {
        scan: Scan {
            series: "Silicon Valley".into(),
            path: t.0.join("error.mkv"),
            snapshot: None,
            outcome: Err("bad container".into()),
        },
        target: None,
        state: State::Untouched,
    });
    p.apply();
    assert!(t.0.join("unknown.mkv").exists());
    assert!(t.0.join("ambiguous.mkv").exists());
    let (s, code) = p.summary(true);
    assert_eq!(code, 1);
    assert!(s.contains("1 identified, 1 unknown, 1 ambiguous, 1 errors"));
    let mut p = Plan::build(vec![t.scan("other.mkv", unknown())]);
    let mut out = Vec::new();
    assert_eq!(p.finish(&mut &b"y\n"[..], &mut out).unwrap(), 2);
    let s = String::from_utf8(out).unwrap();
    assert!(!s.contains("No changes needed"));
    assert!(!s.contains("Apply renames?"));
}
#[test]
fn malicious_titles_and_terminal_controls_are_bounded_safe_components() {
    let t = Temp::new();
    for title in [
        "../../CON:<bad>|?* .",
        "é".repeat(250).as_str(),
        "CON",
        "A\u{202e}B",
    ] {
        let outcome = identified(title);
        let base = basename(&outcome, &t.0.join("input.MKV"), "Silicon Valley").unwrap();
        assert!(base.len() <= 240);
        assert_eq!(Path::new(&base).components().count(), 1);
        assert!(
            !base
                .chars()
                .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        );
    }
    assert_eq!(name(OsStr::new("normal name.mkv")), "normal name.mkv");
    assert_eq!(text("a\x1b\n⟦"), "a⟦U+001B⟧⟦U+000A⟧⟦⟦");
}
#[test]
fn series_prefix_is_required_safe_and_never_truncates_episode_code() {
    let outcome = identified(&"Long title".repeat(50));
    let path = Path::new("input.mkv");
    let base = basename(&outcome, path, &"é".repeat(100)).unwrap();
    assert!(base.len() <= 240);
    assert!(base.contains(" - S01E08 - "));
    assert!(basename(&outcome, path, &"x".repeat(208)).is_err());
    assert!(
        basename(&outcome, path, "The Legend of Korra (2012)")
            .unwrap()
            .starts_with("The Legend of Korra (2012) - S")
    );
    assert!(basename(&outcome, path, " .. ").is_err());
    let base = basename(&identified("Title"), path, "../Show:Name").unwrap();
    assert_eq!(base, ".._Show_Name - S01E08 - Title.mkv");
    assert_eq!(Path::new(&base).components().count(), 1);
}
#[test]
fn non_unicode_source_preserved_and_renamed_without_lossy_lookup() {
    #[cfg(windows)]
    let source = {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&[0xd800, 46, 109, 107, 118])
    };
    #[cfg(unix)]
    let source = {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(vec![255, b'.', b'm', b'k', b'v'])
    };
    let t = Temp::new();
    let path = t.0.join(&source);
    fs::write(&path, b"native bytes").unwrap();
    assert!(name(&source).contains('⟦'));
    let mut p = Plan::build(vec![Scan {
        series: "Silicon Valley".into(),
        snapshot: Some(Snapshot::take(&path).unwrap()),
        path,
        outcome: Ok(identified("Title")),
    }]);
    p.apply();
    assert_eq!(p.entries[0].state, State::Applied);
    assert_eq!(
        fs::read(p.entries[0].target.as_ref().unwrap()).unwrap(),
        b"native bytes"
    );
}
#[cfg(windows)]
#[test]
fn junction_folder_and_ancestors_are_refused() {
    let t = Temp::new();
    let real = t.0.join("real");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("source.mkv"), b"untouched").unwrap();
    let junction = t.0.join("junction");
    let output = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&real)
        .output()
        .unwrap();
    assert!(output.status.success(), "synthetic junction setup failed");
    let folder_refused = check_folder(&junction).is_err();
    let source_refused = Snapshot::take(&junction.join("source.mkv")).is_err();
    fs::remove_dir(&junction).unwrap();
    assert!(folder_refused && source_refused);
    assert_eq!(fs::read(real.join("source.mkv")).unwrap(), b"untouched");
}
#[cfg(unix)]
#[test]
fn symlink_sources_folders_and_destinations_refused() {
    use std::os::unix::fs::symlink;
    let t = Temp::new();
    let scan = t.scan("source.mkv", identified("Title"));
    symlink(&scan.path, t.0.join("link.mkv")).unwrap();
    assert!(Snapshot::take(&t.0.join("link.mkv")).is_err());
    symlink(&t.0, t.0.join("folder-link")).unwrap();
    assert!(check_folder(&t.0.join("folder-link")).is_err());
    symlink(
        t.0.join("missing"),
        t.0.join("Silicon Valley - S01E08 - Title.mkv"),
    )
    .unwrap();
    let p = Plan::build(vec![scan]);
    assert!(matches!(p.entries[0].state, State::Conflict(_)));
}
