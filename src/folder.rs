mod progressive;
mod show_picker;
use crate::cli::Options;
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};
use tvmatch::opensubtitles::{Cache, Scope};
fn reference_scope(options: &Options) -> Result<Scope, Box<dyn Error>> {
    Ok(match &options.imdb {
        Some(imdb) => Scope::from_imdb(imdb, &options.season, options.episodes.as_deref())?,
        None => Scope::new(
            options.show.as_deref().unwrap(),
            &options.season,
            options.episodes.as_deref(),
        )?,
    })
}
pub(super) fn run(options: Options) -> Result<u8, Box<dyn Error>> {
    let scope = reference_scope(&options)?;
    let paths = enumerate(&tvmatch::paths::input(&options.folder)?)?;
    let cache = Cache::default_private()?;
    let (references, series) = tvmatch::opensubtitles::references_for_rename_with_interactions(
        &cache,
        &scope,
        true,
        options.show.as_deref(),
        options.dry_run,
        &mut show_picker::ShowPicker {
            dry_run: options.dry_run,
        },
        &mut ReferenceFallback {
            dry_run: options.dry_run,
        },
    )?;
    let expected = cache.selected_references(&scope)?;
    // References and labels are owned in memory; scanning/stdin need no cache lock.
    drop(cache);
    match_folder(&paths, references, &series, options.dry_run, &expected)
}
struct ReferenceFallback {
    dry_run: bool,
}
impl tvmatch::opensubtitles::FallbackInteraction for ReferenceFallback {
    fn offer(
        &mut self,
        offer: &tvmatch::opensubtitles::FallbackOffer,
    ) -> Result<(), tvmatch::opensubtitles::Failure> {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        let result = (|| -> std::io::Result<()> {
            writeln!(
                out,
                "Empty reference S{:02}E{:02}: file_id={} has {} timed records without captions.",
                offer.failed.season,
                offer.failed.episode,
                offer.failed.file_id,
                offer.empty_records
            )?;
            for candidate in &offer.alternatives {
                writeln!(
                    out,
                    "  Fallback file_id={}: {}{}",
                    candidate.file_id,
                    crate::rename::text(&candidate.release),
                    if candidate.file_id == offer.proposed.file_id {
                        " (proposed)"
                    } else {
                        ""
                    }
                )?;
            }
            if offer.retry {
                writeln!(
                    out,
                    "Prior fallback attempt may have been charged; retry needs fresh confirmation."
                )?;
            }
            if self.dry_run {
                writeln!(
                    out,
                    "Dry-run: alternatives only; no prompt or replacement download."
                )?;
            }
            out.flush()
        })();
        result.map_err(|_| {
            tvmatch::opensubtitles::Failure("fallback output failed; no download".into())
        })
    }
    fn confirm(
        &mut self,
        offer: &tvmatch::opensubtitles::FallbackOffer,
    ) -> Result<bool, tvmatch::opensubtitles::Failure> {
        use std::io::Write;
        if self.dry_run {
            return Ok(false);
        }
        let mut out = std::io::stdout().lock();
        write!(
            out,
            "Download fallback file {}? This may use download quota. (y/N): ",
            offer.proposed.file_id
        )
        .and_then(|_| out.flush())
        .map_err(|_| {
            tvmatch::opensubtitles::Failure("fallback prompt failed; no download".into())
        })?;
        Ok(crate::rename::confirm(&mut std::io::stdin().lock()).unwrap_or(false))
    }
}
fn enumerate(folder: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    crate::rename::check_folder(folder)?;
    let mut files = Vec::new();
    for (count, entry) in fs::read_dir(folder)?.enumerate() {
        if count >= 256 {
            return Err("folder enumeration exceeds 256 entries".into());
        }
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|s| {
            ["mkv", "mp4", "m4v"]
                .iter()
                .any(|ext| s.eq_ignore_ascii_case(ext))
        }) {
            crate::rename::Snapshot::take(&path)?;
            files.push(path);
            if files.len() > 32 {
                return Err("folder exceeds 32 MKV/MP4 files".into());
            }
        }
    }
    files.sort();
    if files.is_empty() {
        return Err("folder contains no regular MKV/MP4 files".into());
    }
    Ok(files)
}
fn eligible_tracks(
    tracks: Vec<tvmatch::media::SubtitleTrack>,
) -> Vec<tvmatch::media::SubtitleTrack> {
    let mut tracks: Vec<_> = tracks
        .into_iter()
        .filter(|t| {
            let language = t.language.as_deref().unwrap_or("").to_ascii_lowercase();
            (language == "en" || language == "eng" || language.starts_with("en-"))
                && t.enabled
                && !t.forced
                && (matches!(t.codec_id.as_str(), "S_HDMV/PGS" | "S_TEXT/UTF8" | "tx3g"))
        })
        .collect();
    tracks.sort_by_key(|track| track.number);
    tracks
}
fn match_tracks(
    tracks: Vec<tvmatch::media::SubtitleTrack>,
    mut attempt: impl FnMut(
        &tvmatch::media::SubtitleTrack,
    ) -> Result<tvmatch::MatchOutcome, Box<dyn Error>>,
) -> Result<tvmatch::MatchOutcome, Box<dyn Error>> {
    use tvmatch::MatchOutcome;
    let tracks = eligible_tracks(tracks);
    if tracks.is_empty() {
        return Err("no readable English subtitle tracks (MKV PGS/UTF-8 or MP4 tx3g)".into());
    }
    let mut fallback = None;
    for (i, track) in tracks.iter().enumerate() {
        if tracks.len() > 1 {
            println!(
                "  Trying English subtitle track {} ({}/{}).",
                track.number,
                i + 1,
                tracks.len()
            );
        }
        match attempt(track) {
            Ok(outcome @ MatchOutcome::Identified { .. }) => return Ok(outcome),
            Ok(outcome @ MatchOutcome::Ambiguous { .. }) => {
                if !matches!(fallback, Some(MatchOutcome::Ambiguous { .. })) {
                    fallback = Some(outcome);
                }
            }
            Ok(outcome @ MatchOutcome::Unknown { .. }) => {
                if fallback.is_none() {
                    fallback = Some(outcome);
                }
            }
            Err(error) => println!(
                "  Track {} failed: {}",
                track.number,
                crate::rename::text(&error.to_string())
            ),
        }
    }
    fallback.ok_or_else(|| "all eligible English subtitle tracks failed to scan".into())
}
#[cfg(feature = "ocr")]
fn match_folder(
    paths: &[PathBuf],
    references: Vec<tvmatch::Reference>,
    series: &str,
    dry_run: bool,
    expected: &[tvmatch::opensubtitles::Selected],
) -> Result<u8, Box<dyn Error>> {
    use tvmatch::{
        Index, MatchOutcome,
        media::{self, ocr::LocalOcr},
    };
    let index = Index::build_with_reference_limit(references, 1000)?;
    let mut engine = None;
    let defaults = media_mkv_webm::streaming::StreamingLimits::default();
    let limits = media_mkv_webm::streaming::StreamingLimits {
        elements: 1_000_000,
        io_operations: 4_000_000,
        ..defaults
    };
    use crate::rename::{self, Plan, Scan, Snapshot};
    println!(
        "Scanning English subtitles; image samples start at 64, widening to 128/192 when needed (not a full-file scan)."
    );
    let mut scans = Vec::new();
    for path in paths {
        println!("Scanning: {}", rename::name(path.file_name().unwrap()));
        let mut snapshot = None;
        let outcome = (|| -> Result<MatchOutcome, Box<dyn Error>> {
            snapshot = Some(Snapshot::take(path)?);
            let outcome = match_tracks(
                media::probe_subtitle_tracks(fs::File::open(path)?)?,
                |track| {
                    snapshot.as_ref().unwrap().verify(path)?;
                    let outcome = if matches!(track.codec_id.as_str(), "S_TEXT/UTF8" | "tx3g") {
                        let transcript =
                            media::extract_subtitles(fs::File::open(path)?, Some(track.number))?
                                .transcript;
                        index.match_query_with_limits(&transcript, tvmatch::EPISODE_PAIR_LIMITS)?
                    } else {
                        if engine.is_none() {
                            engine = Some(LocalOcr::bundled()?);
                        }
                        progressive::match_pgs(
                            fs::File::open(path)?,
                            track.number,
                            &index,
                            engine.as_ref().unwrap(),
                            limits,
                            &mut std::io::stdout().lock(),
                        )?
                    };
                    snapshot.as_ref().unwrap().verify(path)?;
                    Ok(outcome)
                },
            )?;
            snapshot.as_ref().unwrap().verify(path)?;
            Ok(outcome)
        })()
        .map_err(|e| e.to_string());
        match &outcome {
            Ok(MatchOutcome::Identified { best, .. }) => {
                let label = best
                    .reference
                    .display_name
                    .split_once(' ')
                    .map(|(code, title)| format!("{code} — {title}"))
                    .unwrap_or_else(|| best.reference.display_name.clone());
                println!("  Identified: {}", rename::text(&label));
            }
            Ok(MatchOutcome::Unknown { .. }) => {
                println!("  Unknown: insufficient matching evidence.")
            }
            Ok(MatchOutcome::Ambiguous { .. }) => {
                println!("  Ambiguous: multiple episodes have qualifying evidence.")
            }
            Err(e) => println!("  Error: {}", rename::text(e)),
        }
        scans.push(Scan {
            series: series.to_owned(),
            path: path.clone(),
            snapshot,
            outcome,
        });
    }
    let mut episodes = std::collections::BTreeMap::new();
    for selected in expected {
        episodes
            .entry((selected.season, selected.episode))
            .or_insert_with(Vec::new)
            .push(tvmatch::ReferenceId::new(
                "opensubtitles",
                &selected.episode_id.to_string(),
            )?);
    }
    let expected = episodes
        .into_iter()
        .map(|((season, number), references)| rename::ExpectedEpisode {
            season,
            number,
            references,
        })
        .collect();
    let mut plan = Plan::build(scans).with_expected_episodes(expected);
    let mut stdout = std::io::stdout().lock();
    if dry_run {
        Ok(plan.dry_run(&mut stdout)?)
    } else {
        let mut stdin = std::io::stdin().lock();
        Ok(plan.finish(&mut stdin, &mut stdout)?)
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn imdb_display_override_never_becomes_a_name_lookup_scope() {
        let args = [
            "--imdb",
            "tt1695360",
            "--show",
            "Unrelated Display Name",
            "--season",
            "3",
        ]
        .map(std::ffi::OsString::from);
        let options = crate::cli::parse(&args).unwrap();
        let scope = super::reference_scope(&options).unwrap();
        assert_eq!(scope.imdb.as_deref(), Some("tt1695360"));
        assert!(scope.show.is_empty());
        assert_eq!(scope.season, 3);
    }
    use super::*;
    #[test]
    fn deterministic_nonrecursive_unicode_folder() {
        let root = std::env::temp_dir().join(format!("tvmatch-folder-test-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("z space.mkv"), b"").unwrap();
        fs::write(root.join("a ü.MKV"), b"").unwrap();
        fs::write(root.join("b.MP4"), b"").unwrap();
        fs::write(root.join("c.m4v"), b"").unwrap();
        fs::write(root.join("ignored.avi"), b"").unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(root.join("nested/ignored.mkv"), b"").unwrap();
        let files = enumerate(&root).unwrap();
        assert_eq!(files.len(), 4);
        assert!(files[0].ends_with("a ü.MKV"));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn mp4_text_tracks_use_the_same_declared_english_enabled_nonforced_gate() {
        let mut en = track(2, "eng");
        en.codec_id = "tx3g".into();
        en.supported = true;
        let mut fr = en.clone();
        fr.number = 1;
        fr.language = Some("fra".into());
        let mut forced = en.clone();
        forced.number = 3;
        forced.forced = true;
        let out = eligible_tracks(vec![en, fr, forced]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].number, 2);
    }
    fn track(n: u64, lang: &str) -> tvmatch::media::SubtitleTrack {
        tvmatch::media::SubtitleTrack {
            number: n,
            uid: n,
            codec_id: "S_HDMV/PGS".into(),
            language: Some(lang.into()),
            name: None,
            enabled: true,
            default: false,
            forced: false,
            default_duration_ns: None,
            supported: false,
        }
    }
    fn unknown() -> tvmatch::MatchOutcome {
        tvmatch::MatchOutcome::Unknown {
            candidates: vec![],
            reasons: vec![],
        }
    }
    fn ambiguous() -> tvmatch::MatchOutcome {
        tvmatch::MatchOutcome::Ambiguous {
            candidates: vec![],
            reason: tvmatch::RejectionReason::CompetingEvidence,
        }
    }
    fn identified() -> tvmatch::MatchOutcome {
        let transcript = tvmatch::srt::Transcript::parse("1\n00:00:00,000 --> 00:00:01,000\nAmber lanterns illuminate quiet gardens\n\n2\n00:00:10,000 --> 00:00:11,000\nSilver otters navigate winding rivers\n\n3\n00:00:20,000 --> 00:00:21,000\nVelvet clouds surround distant mountains\n").unwrap();
        let reference = tvmatch::Reference::new(
            tvmatch::ReferenceId::new("opensubtitles", "123").unwrap(),
            "S01E01 Original Episode",
            "Synthetic test",
            transcript.clone(),
        )
        .unwrap();
        let result = tvmatch::Index::build(vec![reference])
            .unwrap()
            .match_query(&transcript)
            .unwrap();
        assert!(matches!(result, tvmatch::MatchOutcome::Identified { .. }));
        result
    }
    #[test]
    fn eligible_english_tracks_are_ordered_and_filtered() {
        let mut disabled = track(1, "eng");
        disabled.enabled = false;
        let mut forced = track(2, "eng");
        forced.forced = true;
        let mut unsupported = track(3, "eng");
        unsupported.codec_id = "S_TEXT/ASS".into();
        let mut text = track(9, "EN-us");
        text.codec_id = "S_TEXT/UTF8".into();
        let tracks = eligible_tracks(vec![
            text,
            track(8, "eng"),
            track(4, "fra"),
            disabled,
            forced,
            unsupported,
        ]);
        assert_eq!(tracks.iter().map(|t| t.number).collect::<Vec<_>>(), [8, 9]);
    }
    #[test]
    fn tracks_continue_after_errors_unknown_and_ambiguous_then_stop_at_identified() {
        let mut visited = Vec::new();
        let result = match_tracks((8..=12).map(|n| track(n, "eng")).collect(), |t| {
            visited.push(t.number);
            match t.number {
                8 => Err("synthetic unreadable track".into()),
                9 => Ok(unknown()),
                10 => Ok(ambiguous()),
                11 => Ok(identified()),
                _ => panic!("must stop at first confident match"),
            }
        })
        .unwrap();
        assert!(matches!(result, tvmatch::MatchOutcome::Identified { .. }));
        assert_eq!(visited, [8, 9, 10, 11]);
        let mut visited = Vec::new();
        match_tracks(vec![track(8, "eng"), track(9, "eng")], |t| {
            visited.push(t.number);
            Ok(identified())
        })
        .unwrap();
        assert_eq!(visited, [8]);
    }
    #[test]
    fn exhausted_tracks_preserve_ambiguity_unknown_or_all_failed_error() {
        let result = match_tracks(vec![track(8, "eng"), track(9, "eng")], |t| {
            Ok(if t.number == 8 {
                ambiguous()
            } else {
                unknown()
            })
        })
        .unwrap();
        assert!(matches!(result, tvmatch::MatchOutcome::Ambiguous { .. }));
        let result =
            match_tracks(vec![track(8, "eng"), track(9, "eng")], |_| Ok(unknown())).unwrap();
        assert!(matches!(result, tvmatch::MatchOutcome::Unknown { .. }));
        assert!(
            match_tracks(vec![track(8, "eng"), track(9, "eng")], |_| Err(
                "synthetic error".into()
            ))
            .is_err()
        );
        assert!(match_tracks(vec![track(1, "fra")], |_| panic!("ineligible track")).is_err());
    }
}
