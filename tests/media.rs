#![cfg(feature = "media")]
mod support;
use media_mkv_webm::{
    TrackKind,
    ebml::{schema::ids, writer::*},
};
use std::io::Cursor;
use support::*;
use tvmatch::{
    Index, MatchOutcome, Reference, ReferenceId,
    media::{MediaError, extract_subtitles, probe_subtitle_tracks},
    srt::{MAX_CUE_BYTES, MAX_CUES, Transcript},
};
#[test]
#[ignore = "opt-in local metadata only; no payloads, OCR, cache or network"]
fn inspect_local_track_eligibility_metadata() {
    let Some(path) = std::env::var_os("TVMATCH_TEST_METADATA_FILE") else {
        return;
    };
    let before = std::fs::symlink_metadata(&path).unwrap();
    assert!(before.is_file() && !before.file_type().is_symlink());
    let stream = media_mkv_webm::streaming::open_streaming_with_limits(
        std::fs::File::open(&path).unwrap(),
        media_mkv_webm::streaming::StreamingLimits {
            skip_cues: true,
            ..Default::default()
        },
    )
    .unwrap();
    println!("container_tracks={}", stream.demuxer.tracks.len());
    for track in &stream.demuxer.tracks {
        // Debug formatting escapes terminal controls; never print names/private data.
        println!(
            "track={} kind={:?} codec={:?} language={:?} enabled={} default={} forced={}",
            track.number,
            track.kind,
            track.codec_id,
            track.language,
            track.flag_enabled,
            track.flag_default,
            track.flag_forced
        );
    }
    let subtitles = probe_subtitle_tracks(std::fs::File::open(&path).unwrap()).unwrap();
    println!("application_subtitle_tracks={}", subtitles.len());
    for track in subtitles {
        println!(
            "subtitle={} codec={:?} language={:?} enabled={} forced={} text_supported={}",
            track.number,
            track.codec_id,
            track.language,
            track.enabled,
            track.forced,
            track.supported
        );
    }
    let after = std::fs::symlink_metadata(&path).unwrap();
    assert_eq!(before.len(), after.len());
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    assert_eq!(before.created().ok(), after.created().ok());
    println!("size/modified/created unchanged; metadata only; payload suffix unvalidated");
}
fn extract(bytes: Vec<u8>) -> Result<tvmatch::media::EmbeddedSubtitles, MediaError> {
    extract_subtitles(Cursor::new(bytes), None)
}
fn single(text: &[u8]) -> Vec<u8> {
    muxed(&[("S_TEXT/UTF8", TrackKind::Subtitle)], &[(1, 0, text)])
}
fn query() -> Vec<u8> {
    muxed(
        &[("S_TEXT/UTF8", TrackKind::Subtitle)],
        &[
            (1, 400, DIALOGUE[0].as_bytes()),
            (1, 11_700, DIALOGUE[1].as_bytes()),
            (1, 24_100, DIALOGUE[2].as_bytes()),
        ],
    )
}
#[test]
fn embedded_dialogue_matches_srt_with_offset_and_container_not_audio_evidence() {
    let embedded = extract(query()).unwrap();
    let reference = Reference::new(
        ReferenceId::new("original", "observatory-v1").unwrap(),
        "Not inferred",
        "Original synthetic v1 UNVERIFIED",
        Transcript::parse(include_str!("../fixtures/observatory.srt")).unwrap(),
    )
    .unwrap();
    let index = Index::build(vec![reference]).unwrap();
    let MatchOutcome::Identified { best, .. } = index.match_query(&embedded.transcript).unwrap()
    else {
        panic!("expected content match")
    };
    assert_eq!(best.offset_ms, 59_950);
    assert_eq!(best.evidence[0].query_start_ms, 400);
    assert_eq!(embedded.timestamps[0].start_ns, 400_000_000);
    assert_eq!(embedded.track.codec_id, "S_TEXT/UTF8");
    assert_eq!(embedded.track.language.as_deref(), Some("eng"));
    assert!(
        embedded
            .timestamps
            .iter()
            .all(|t| t.transcript_end_synthetic && t.declared_end_ns.is_none())
    );
}
#[test]
fn explicit_selection_never_picks_first_default_or_only_supported_track() {
    let bytes = muxed(
        &[
            ("S_TEXT/UTF8", TrackKind::Subtitle),
            ("S_TEXT/ASS", TrackKind::Subtitle),
            ("V_VP9", TrackKind::Video),
        ],
        &[(1, 0, b"caption")],
    );
    let tracks = probe_subtitle_tracks(Cursor::new(bytes.clone())).unwrap();
    assert_eq!(tracks.len(), 2);
    assert!(!tracks[1].supported);
    assert!(
        matches!(extract(bytes.clone()), Err(MediaError::AmbiguousSubtitleTracks(v)) if v == vec![1, 2])
    );
    assert_eq!(
        extract_subtitles(Cursor::new(bytes.clone()), Some(1))
            .unwrap()
            .track
            .number,
        1
    );
    for number in [2, 3] {
        assert!(
            matches!(extract_subtitles(Cursor::new(bytes.clone()), Some(number)), Err(MediaError::UnsupportedTrack(n)) if n == number)
        );
    }
    assert!(matches!(
        extract_subtitles(Cursor::new(bytes), Some(99)),
        Err(MediaError::TrackNotFound(99))
    ));
}
#[test]
fn unsupported_no_tracks_and_no_packets_are_errors_not_unknown() {
    assert!(matches!(
        extract(muxed(&[("S_TEXT/ASS", TrackKind::Subtitle)], &[])),
        Err(MediaError::UnsupportedTrack(1))
    ));
    assert!(matches!(
        extract(muxed(&[("V_VP9", TrackKind::Video)], &[])),
        Err(MediaError::NoSubtitleTracks)
    ));
    assert!(matches!(
        extract(muxed(&[("S_TEXT/UTF8", TrackKind::Subtitle)], &[])),
        Err(MediaError::NoCaptions)
    ));
    assert!(extract(b"not an MKV/MP4 parser".to_vec()).is_err());
}
#[test]
fn block_default_missing_zero_and_submillisecond_durations_have_honest_evidence() {
    for (block_duration, default_duration, expected) in [
        (Some(2_000), None, Some(2_000_000_000)),
        (None, Some(3_000_000_000), Some(3_000_000_000)),
        (None, None, None),
        (Some(0), Some(3_000_000_000), Some(0)),
    ] {
        let mut extra = Vec::new();
        if let Some(d) = default_duration {
            write_uint(ids::DEFAULT_DURATION, d, &mut extra);
        }
        let bytes = container(
            &track(1, "S_TEXT/UTF8", &extra),
            1_000_000,
            &cluster(
                1000,
                &group(&block(ids::BLOCK, 1, 0, 0, b"caption"), block_duration),
            ),
        );
        let embedded = extract(bytes).unwrap();
        let t = &embedded.timestamps[0];
        assert_eq!(t.declared_end_ns, expected.map(|d| 1_000_000_000 + d));
        assert_eq!(t.block_duration_ns, block_duration.map(|d| d * 1_000_000));
        assert_eq!(t.transcript_end_synthetic, expected.is_none_or(|d| d == 0));
    }
    let bytes = container(
        &track(1, "S_TEXT/UTF8", &[]),
        1,
        &cluster(
            400_999_999,
            &group(&block(ids::BLOCK, 1, 0, 0, b"caption"), Some(1)),
        ),
    );
    let e = extract(bytes).unwrap();
    assert_eq!(e.timestamps[0].start_ns, 400_999_999);
    assert_eq!(e.transcript.cues()[0].start_ms, 400);
    assert_eq!(e.transcript.cues()[0].end_ms, 401);
    assert!(!e.timestamps[0].transcript_end_synthetic);
}
#[test]
fn invalid_utf8_control_blank_and_backwards_captions_fail() {
    assert!(matches!(
        extract(single(&[0xff])),
        Err(MediaError::InvalidUtf8)
    ));
    for text in [
        b"caption\0".as_slice(),
        b"caption\x0c",
        b"\x0b",
        b"a\rb",
        b" \t\r\n",
        b"",
    ] {
        assert!(extract(single(text)).is_err(), "{text:?}");
    }
    let good = extract(single("Étoile\t東京\r\nпривет".as_bytes())).unwrap();
    assert_eq!(good.transcript.cues()[0].text, "Étoile\t東京\nпривет");
    let mut blocks = block(ids::SIMPLE_BLOCK, 1, 10, 0, b"first");
    blocks.extend(block(ids::SIMPLE_BLOCK, 1, 0, 0, b"second"));
    assert!(
        extract(container(
            &track(1, "S_TEXT/UTF8", &[]),
            1_000_000,
            &cluster(0, &blocks)
        ))
        .is_err()
    );
}
#[test]
fn raw_submillisecond_order_is_checked_before_rounding() {
    for (second_delta, accepted) in [(1, false), (2, true), (3, true)] {
        let mut blocks = block(ids::SIMPLE_BLOCK, 1, 2, 0, b"first caption");
        blocks.extend(block(
            ids::SIMPLE_BLOCK,
            1,
            second_delta,
            0,
            b"second caption",
        ));
        let result = extract(container(
            &track(1, "S_TEXT/UTF8", &[]),
            1,
            &cluster(1_000_000, &blocks),
        ));
        if accepted {
            let embedded = result.unwrap();
            assert_eq!(embedded.timestamps[0].start_ns, 1_000_002);
            assert_eq!(
                embedded.timestamps[1].start_ns,
                1_000_000 + second_delta as u64
            );
            assert!(
                embedded
                    .transcript
                    .cues()
                    .iter()
                    .all(|cue| cue.start_ms == 1)
            );
        } else {
            assert!(
                matches!(result, Err(MediaError::InvalidTimestamp)),
                "{result:?}"
            );
        }
    }
}

#[test]
fn ietf_language_takes_precedence_in_both_element_orders() {
    for ietf_first in [false, true] {
        let declarations = if ietf_first {
            [(ids::LANGUAGE_IETF, "fr-CA"), (ids::LANGUAGE, "eng")]
        } else {
            [(ids::LANGUAGE, "eng"), (ids::LANGUAGE_IETF, "fr-CA")]
        };
        let mut extra = Vec::new();
        for (id, text) in declarations {
            write_ascii(id, text, &mut extra);
        }
        let bytes = container(
            &track(1, "S_TEXT/UTF8", &extra),
            1_000_000,
            &cluster(0, &block(ids::SIMPLE_BLOCK, 1, 0, 0, b"caption")),
        );
        assert_eq!(
            probe_subtitle_tracks(Cursor::new(bytes.clone())).unwrap()[0]
                .language
                .as_deref(),
            Some("fr-CA"),
        );
        assert_eq!(
            extract(bytes).unwrap().track.language.as_deref(),
            Some("fr-CA")
        );
    }
}

#[test]
fn packet_cue_and_aggregate_text_limits_do_not_return_partial_evidence() {
    assert!(matches!(
        extract(single(&vec![b'a'; MAX_CUE_BYTES + 1])),
        Err(MediaError::LimitExceeded("selected packet bytes"))
    ));
    let text = vec![b'a'; MAX_CUE_BYTES];
    let packets: Vec<_> = (0..257).map(|i| (1, i, text.as_slice())).collect();
    assert!(matches!(
        extract(muxed(&[("S_TEXT/UTF8", TrackKind::Subtitle)], &packets)),
        Err(MediaError::LimitExceeded("subtitle text bytes"))
    ));
    // One cluster avoids unrelated I/O read-ahead exhaustion before the packet cap.
    let mut blocks = Vec::new();
    for _ in 0..=MAX_CUES {
        blocks.extend(block(ids::SIMPLE_BLOCK, 1, 0, 0, b"caption"));
    }
    assert!(matches!(
        extract(container(
            &track(1, "S_TEXT/UTF8", &[]),
            1_000_000,
            &cluster(0, &blocks)
        )),
        Err(MediaError::LimitExceeded("selected packets/cues"))
    ));
}
#[test]
fn timestamp_range_end_overflow_and_valid_prefix_truncation_fail() {
    let tracks = track(1, "S_TEXT/UTF8", &[]);
    for (scale, ticks, duration) in [
        (1, u64::MAX, None),
        (1_000_000, 360_000_000, None),
        (1, 1, Some(u64::MAX)),
    ] {
        assert!(
            extract(container(
                &tracks,
                scale,
                &cluster(
                    ticks,
                    &group(&block(ids::BLOCK, 1, 0, 0, b"caption"), duration)
                )
            ))
            .is_err()
        );
    }
    let mut tail = cluster(0, &block(ids::SIMPLE_BLOCK, 1, 0, 0, b"valid caption"));
    tail.extend([0x1f, 0x43]);
    assert!(extract(container(&tracks, 1_000_000, &tail)).is_err());
}
#[test]
fn duplicate_numbers_track_limits_and_unsupported_caption_timing() {
    let mut tracks = track(1, "S_TEXT/UTF8", &[]);
    tracks.extend(track(1, "S_TEXT/UTF8", &[]));
    assert!(matches!(
        extract(container(&tracks, 1_000_000, &[])),
        Err(MediaError::InvalidTrackMetadata)
    ));
    let tracks: Vec<_> = (1..=65)
        .flat_map(|n| track(n, "S_TEXT/UTF8", &[]))
        .collect();
    assert!(matches!(
        extract(container(&tracks, 1_000_000, &[])),
        Err(MediaError::LimitExceeded("tracks"))
    ));
    for id in [ids::CODEC_DELAY, ids::SEEK_PRE_ROLL, ids::CODEC_PRIVATE] {
        let mut extra = Vec::new();
        write_uint(id, 1, &mut extra);
        assert!(matches!(
            extract(container(&track(1, "S_TEXT/UTF8", &extra), 1_000_000, &[])),
            Err(MediaError::UnsupportedTrack(1))
        ));
    }
}
