use super::*;

fn filename(title: &str) -> String {
    format!("Silicon Valley - S01E08 - {title}.mkv")
}
fn moving(t: &Temp, old: &str, new: &str) -> Scan {
    let mut scan = t.scan(&filename(old), identified(new));
    fs::write(&scan.path, format!("original bytes of {old}")).unwrap();
    scan.snapshot = Some(Snapshot::take(&scan.path).unwrap());
    scan
}
fn contents(t: &Temp) -> Vec<Vec<u8>> {
    let mut result = fs::read_dir(&t.0)
        .unwrap()
        .map(|e| fs::read(e.unwrap().path()).unwrap())
        .collect::<Vec<_>>();
    result.sort();
    result
}
fn cycle(t: &Temp, count: usize) -> Plan {
    let titles = ["One", "Two", "Three"];
    Plan::build(
        (0..count)
            .map(|i| moving(t, titles[i], titles[(i + 1) % count]))
            .collect(),
    )
}
#[test]
fn mp4_and_m4v_swaps_preserve_extension_case_bytes_and_consent() {
    for ext in ["mp4", "MP4", "m4v"] {
        let t = Temp::new();
        let file = |title: &str| format!("Silicon Valley - S01E08 - {title}.{ext}");
        let mut scans = Vec::new();
        for (old, new) in [("One", "Two"), ("Two", "One")] {
            let mut scan = t.scan(&file(old), identified(new));
            fs::write(&scan.path, old.as_bytes()).unwrap();
            scan.snapshot = Some(Snapshot::take(&scan.path).unwrap());
            assert_eq!(
                temporary_name(&scan.path).unwrap().extension().unwrap(),
                ext
            );
            scans.push(scan);
        }
        let mut plan = Plan::build(scans);
        assert!(plan.entries.iter().all(|e| e.state == State::Planned));
        let before = contents(&t);
        assert_eq!(plan.dry_run(&mut Vec::new()).unwrap(), 0);
        assert_eq!(contents(&t), before);
        assert_eq!(plan.finish(&mut &b"yes\n"[..], &mut Vec::new()).unwrap(), 0);
        assert_eq!(fs::read(t.0.join(file("One"))).unwrap(), b"Two");
        assert_eq!(fs::read(t.0.join(file("Two"))).unwrap(), b"One");
        assert_eq!(contents(&t), before);
    }
}
#[test]
fn dry_run_previews_cycles_without_staging_or_prompt_and_keeps_exit_status() {
    let t = Temp::new();
    let plan = cycle(&t, 2);
    let before = contents(&t);
    let mut output = Vec::new();
    assert_eq!(plan.dry_run(&mut output).unwrap(), 0);
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("Rename preview:"));
    assert!(output.contains("Dry run; no files renamed."));
    assert!(!output.contains("Apply renames?"));
    assert_eq!(contents(&t), before);
    for e in &plan.entries {
        e.scan
            .snapshot
            .as_ref()
            .unwrap()
            .verify(&e.scan.path)
            .unwrap();
    }
    let t = Temp::new();
    let unknown_plan = Plan::build(vec![t.scan("unknown.mkv", unknown())]);
    assert_eq!(unknown_plan.dry_run(&mut Vec::new()).unwrap(), 2);
    let scans = vec![moving(&t, "One", "Two")];
    fs::write(t.0.join(filename("Two")), b"stationary destination").unwrap();
    let conflict = Plan::build(scans);
    assert_eq!(conflict.dry_run(&mut Vec::new()).unwrap(), 1);
}
#[test]
fn chain_preview_resolves_occupants_and_apply_orders_without_temporary_names() {
    let t = Temp::new();
    let mut plan = Plan::build(vec![
        moving(&t, "One", "Two"),
        moving(&t, "Two", "Three"),
        moving(&t, "Three", "Four"),
    ]);
    assert!(plan.entries.iter().all(|e| e.state == State::Planned));
    assert!(plan.preview().contains("will be freed"));
    assert!(!plan.preview().contains("conflict"));
    let before = contents(&t);
    plan.finish(&mut &b"n\n"[..], &mut Vec::new()).unwrap();
    assert_eq!(contents(&t), before);
    for entry in &plan.entries {
        entry
            .scan
            .snapshot
            .as_ref()
            .unwrap()
            .verify(&entry.scan.path)
            .unwrap();
    }
    let mut operations = Vec::new();
    plan.apply_with(|from, to| {
        operations.push((
            from.file_name().unwrap().to_owned(),
            to.file_name().unwrap().to_owned(),
        ));
        native::no_replace(from, to)
    });
    assert_eq!(operations.len(), 3);
    assert_eq!(operations[0].0, OsStr::new(&filename("Three")));
    assert!(
        operations
            .iter()
            .all(|(a, b)| !name(a).contains(".tvmatch-") && !name(b).contains(".tvmatch-"))
    );
    for (old, new) in [("One", "Two"), ("Two", "Three"), ("Three", "Four")] {
        assert_eq!(
            fs::read(t.0.join(filename(new))).unwrap(),
            format!("original bytes of {old}").as_bytes()
        );
    }
    assert_eq!(contents(&t), before);
    assert_eq!(plan.summary(true).1, 0);
}
#[test]
fn swaps_and_three_file_cycles_preserve_all_contents_and_leave_no_temporary_files() {
    for count in [2, 3] {
        let t = Temp::new();
        let mut plan = cycle(&t, count);
        assert!(plan.entries.iter().all(|e| e.state == State::Planned));
        let before = contents(&t);
        plan.finish(&mut &b""[..], &mut Vec::new()).unwrap();
        assert_eq!(contents(&t), before);
        for e in &plan.entries {
            assert!(e.scan.path.exists());
        }
        let mut output = Vec::new();
        assert_eq!(plan.finish(&mut &b"yes\n"[..], &mut output).unwrap(), 0);
        for e in &plan.entries {
            assert_eq!(e.state, State::Applied);
            let old = e
                .scan
                .path
                .file_stem()
                .unwrap()
                .to_str()
                .unwrap()
                .split(" - ")
                .last()
                .unwrap();
            assert_eq!(
                fs::read(e.target.as_ref().unwrap()).unwrap(),
                format!("original bytes of {old}").as_bytes()
            );
        }
        assert_eq!(contents(&t), before);
        assert!(
            fs::read_dir(&t.0)
                .unwrap()
                .all(|e| !name(&e.unwrap().file_name()).starts_with(".tvmatch-"))
        );
    }
}
#[test]
fn case_only_rename_uses_a_temporary_name_without_overwriting() {
    let t = Temp::new();
    let scan = t.scan(&filename("One").to_lowercase(), identified("One"));
    let mut plan = Plan::build(vec![scan]);
    assert_eq!(plan.entries[0].state, State::Planned);
    let mut moves = 0;
    plan.apply_with(|from, to| {
        moves += 1;
        native::no_replace(from, to)
    });
    assert_eq!(moves, 2);
    assert_eq!(plan.entries[0].state, State::Applied);
    let names = fs::read_dir(&t.0)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(names, [std::ffi::OsString::from(filename("One"))]);
    assert_eq!(
        fs::read(plan.entries[0].target.as_ref().unwrap()).unwrap(),
        b"original synthetic bytes"
    );
}
#[test]
fn stationary_or_conflicted_occupants_block_every_dependent_rename() {
    let t = Temp::new();
    let duplicated_source = Plan::build(vec![moving(&t, "One", "Two"), moving(&t, "One", "Three")]);
    assert!(
        duplicated_source
            .entries
            .iter()
            .all(|e| matches!(e.state, State::Conflict(_)))
    );
    let t = Temp::new();
    let scans = vec![
        moving(&t, "One", "Two"),
        moving(&t, "Two", "Three"),
        moving(&t, "Three", "Four"),
    ];
    fs::write(t.0.join(filename("Four")), b"unrelated destination").unwrap();
    let mut plan = Plan::build(scans);
    assert!(
        plan.entries
            .iter()
            .all(|e| matches!(e.state, State::Conflict(_)))
    );
    let before = contents(&t);
    plan.finish(&mut &b"yes\n"[..], &mut Vec::new()).unwrap();
    assert_eq!(contents(&t), before);
    let t = Temp::new();
    let plan = Plan::build(vec![
        moving(&t, "One", "Two"),
        t.scan(&filename("Two"), unknown()),
    ]);
    assert!(matches!(plan.entries[0].state, State::Conflict(_)));
    assert_eq!(plan.entries[1].state, State::Untouched);
    let t = Temp::new();
    let plan = Plan::build(vec![
        moving(&t, "One", "Two"),
        moving(&t, "Two", "Four"),
        moving(&t, "Three", "Four"),
    ]);
    assert!(
        plan.entries
            .iter()
            .all(|e| matches!(e.state, State::Conflict(_)))
    );
}
#[test]
fn changed_occupant_after_preview_blocks_chain_before_any_move() {
    let t = Temp::new();
    let mut plan = Plan::build(vec![moving(&t, "One", "Two"), moving(&t, "Two", "Three")]);
    fs::write(&plan.entries[1].scan.path, b"changed during confirmation").unwrap();
    let before = contents(&t);
    plan.apply_with(|_, _| panic!("blocked chain must not start moving"));
    assert!(
        plan.entries
            .iter()
            .all(|e| matches!(e.state, State::Failed(_)))
    );
    assert_eq!(contents(&t), before);
}
#[test]
fn failures_at_each_swap_step_preserve_every_file_and_report_leftover_names() {
    for fail_at in 1..=3 {
        let t = Temp::new();
        let mut plan = cycle(&t, 2);
        let before = contents(&t);
        let mut step = 0;
        plan.apply_with(|from, to| {
            step += 1;
            if step == fail_at {
                Err(io::Error::other("synthetic move failure"))
            } else {
                native::no_replace(from, to)
            }
        });
        assert_eq!(contents(&t), before, "step {fail_at}");
        let (summary, code) = plan.summary(true);
        assert_eq!(code, 1);
        if fail_at > 1 {
            let temporary = fs::read_dir(&t.0)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .find(|n| name(n).starts_with(".tvmatch-rename-"))
                .unwrap();
            assert!(summary.contains(&name(&temporary)));
        }
        assert!(summary.contains("Apply incomplete"));
    }
}
#[test]
fn final_destination_race_during_swap_never_overwrites_new_file() {
    let t = Temp::new();
    let mut plan = cycle(&t, 2);
    let mut step = 0;
    plan.apply_with(|from, to| {
        step += 1;
        if step == 3 {
            fs::write(to, b"new unrelated file").unwrap();
        }
        native::no_replace(from, to)
    });
    assert_eq!(
        fs::read(t.0.join(filename("Two"))).unwrap(),
        b"new unrelated file"
    );
    let all = contents(&t);
    assert!(all.contains(&b"original bytes of One".to_vec()));
    assert!(all.contains(&b"original bytes of Two".to_vec()));
    assert_eq!(all.len(), 3);
    assert_eq!(plan.summary(true).1, 1);
    assert!(plan.summary(true).0.contains("temporary file left as"));
}
