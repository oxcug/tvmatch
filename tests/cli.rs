use std::process::Command;
fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tvmatch"))
        .args(args)
        .output()
        .unwrap()
}
#[test]
fn root_help_only_current_interface() {
    for flag in ["--help", "-h"] {
        let out = run(&[flag]);
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(text.contains("[--folder PATH]"));
        assert!(text.contains("--imdb ttID"));
        assert!(text.contains("current directory"));
        assert!(text.contains("optional --show overrides only its display name"));
        assert!(text.contains("provider original_title"));
        assert!(text.contains("show year always comes from metadata"));
        assert!(text.contains("The Legend of Korra (2012) - S03E06 - Old Wounds.mkv"));
        assert!(text.contains("metadata-only GET"));
        for guidance in [
            "up to 5 numbered",
            "10 pages / 1000 results",
            "Nonexact results require explicit selection",
            "stops without reading stdin",
            "frozen cached selections",
        ] {
            assert!(text.contains(guidance), "{guidance}");
        }
        assert!(!text.contains("Exactly one"));
        assert!(text.contains("64 visible frames"));
        assert!(text.contains("MP4/M4V tx3g"));
        assert!(text.contains("Text tracks are decoded without OCR"));
        assert!(text.contains("Reference SRT accepts UTF-8 or BOM-marked UTF-16"));
        assert!(text.contains("no encoding guesses or lossy decoding"));
        assert!(text.contains("Apply renames? (y/N):"));
        assert!(text.contains("Download fallback file ...? (y/N):"));
        assert!(text.contains("one fallback download per run"));
        assert!(text.contains("no fallback prompt or replacement download"));
        assert!(text.contains("[--dry-run]"));
        assert!(text.contains("without prompting or reading stdin"));
        assert!(text.contains("No second scan"));
        assert!(!text.contains("--apply"));
        assert!(!text.contains("--track"));
        assert!(text.contains("tried one at a time"));
        assert!(!text.contains("no renames"));
        for old in [
            "--fetch-references",
            "--ocr-recognition-model",
            "--language",
            "--query",
            "tvmatch folder",
            "tvmatch references",
        ] {
            assert!(!text.contains(old), "{old}");
        }
    }
}
#[test]
fn root_rejects_obsolete_and_duplicate_flags_before_side_effects() {
    for flag in [
        "folder",
        "references",
        "--query",
        "--reference",
        "--media",
        "--query-media",
        "--extract-subtitles",
        "--language",
        "--ocr-recognition-model",
        "--ocr-detection-model",
        "--ocr-max-frames",
        "--ocr-max-elements",
        "--ocr-max-io-operations",
        "--fetch-references",
        "--cache",
        "--apply",
        "--track",
    ] {
        let out = run(&["--show", "Original Show", "--season", "1", flag, "anything"]);
        assert_eq!(out.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&out.stderr).contains("arguments"));
        assert!(out.stdout.is_empty());
    }
    for args in [
        vec![],
        vec!["--show", "Original Show"],
        vec!["--season", "1"],
        vec![
            "--show",
            "Original Show",
            "--imdb",
            "tt1",
            "--imdb",
            "tt2",
            "--season",
            "1",
        ],
        vec!["--show", "Original Show", "--season", "1", "--season", "2"],
        vec!["--help", "extra"],
        vec!["--show", "Original Show", "--season", "1", "--apply"],
        vec!["--show", "Original Show", "--season", "--track", "8"],
        vec!["--show", "Original Show", "--season", "1", "--folder"],
    ] {
        let out = run(&args);
        assert_eq!(out.status.code(), Some(1));
        assert!(out.stdout.is_empty());
    }
}
#[test]
fn root_metadata_and_numbers_validate_before_filesystem_or_credentials() {
    for (flag, values) in [
        (
            "--imdb",
            vec![
                "2575988",
                "tt",
                "tt0",
                "tt-1",
                "tt+1",
                "TT123",
                "tt2147483648",
                "tt00000000001",
                "tt1/2",
            ],
        ),
        ("--season", vec!["0", "101", "+1", "1.0", ""]),
        ("--episodes", vec!["0", "9-1", "1-1001", "1-2-3", "1001"]),
    ] {
        for value in values {
            let mut args = vec!["--folder", "this-path-must-never-be-opened"];
            if flag != "--imdb" {
                args.extend(["--show", "Original Show"]);
            }
            if flag != "--season" {
                args.extend(["--season", "1"]);
            }
            args.extend([flag, value]);
            let out = run(&args);
            assert_eq!(out.status.code(), Some(1));
            assert!(out.stdout.is_empty());
            assert!(!String::from_utf8_lossy(&out.stderr).contains("os error"));
        }
    }
}
#[cfg(not(all(feature = "ocr", feature = "opensubtitles")))]
#[test]
fn minimal_and_media_only_cli_validate_then_report_unavailable() {
    for id in [["--show", "Original Show"], ["--imdb", "tt2575988"]] {
        let out = run(&[id[0], id[1], "--season", "1", "--episodes", "1-24"]);
        assert_eq!(out.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("requires features ocr,opensubtitles")
        );
    }
}
#[cfg(not(feature = "media"))]
#[test]
fn minimal_library_media_feature_is_explicit() {
    assert!(matches!(
        tvmatch::media::extract_subtitles(std::io::Cursor::new([]), None),
        Err(tvmatch::media::MediaError::Unavailable)
    ));
    assert!(matches!(
        tvmatch::media::probe_subtitle_tracks(std::io::Cursor::new([])),
        Err(tvmatch::media::MediaError::Unavailable)
    ));
}

#[cfg(all(feature = "ocr", feature = "opensubtitles"))]
#[test]
fn root_folder_defaults_to_process_cwd_and_large_range_is_not_trial_capped() {
    let root = std::env::temp_dir().join(format!("tvmatch-cli-cwd-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tvmatch"))
        .current_dir(&root)
        .args([
            "--show",
            "Original Show",
            "--season",
            "1",
            "--episodes",
            "1-52",
        ])
        .output()
        .unwrap();
    std::fs::remove_dir(&root).unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("folder contains no regular MKV/MP4 files")
    );
    assert!(out.stdout.is_empty());
}
