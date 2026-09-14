#![cfg(feature = "media")]
mod support;
use media_mkv_webm::{
    TrackKind,
    ebml::{schema::ids, writer::*},
    streaming::{StreamingLimits, open_streaming, open_streaming_with_limits},
};
use std::io::Cursor;
use support::*;

fn mapping_fixture(extra: &[u8], kind: u64, type_first: bool) -> Vec<u8> {
    let mut video = Vec::new();
    write_uint(ids::TRACK_NUMBER, 1, &mut video);
    write_uint(ids::TRACK_UID, 101, &mut video);
    if type_first {
        write_uint(ids::TRACK_TYPE, kind, &mut video);
    }
    video.extend_from_slice(extra);
    if !type_first {
        write_uint(ids::TRACK_TYPE, kind, &mut video);
    }
    write_ascii(ids::CODEC_ID, "V_MPEGH/ISO/HEVC", &mut video);
    let mut tracks = Vec::new();
    write_master(ids::TRACK_ENTRY, &video, &mut tracks);
    tracks.extend(track(2, "S_HDMV/PGS", &[]));
    let mut packets = block(ids::SIMPLE_BLOCK, 1, 0, 0, &[9, 8, 7]);
    packets.extend(block(ids::SIMPLE_BLOCK, 2, 5, 0, &[1, 2, 3, 4]));
    container(&tracks, 1_000_000, &cluster(100, &packets))
}
fn video_mapping() -> Vec<u8> {
    let mut extra = Vec::new();
    for (name, len) in [(b"dvcC", 24), (b"hvcE", 187)] {
        let mut body = Vec::new();
        write_uint(
            ids::BLOCK_ADD_ID_TYPE,
            u32::from_be_bytes(*name) as u64,
            &mut body,
        );
        write_element(ids::BLOCK_ADD_ID_EXTRA_DATA, &vec![0; len], &mut body);
        write_master(ids::BLOCK_ADDITION_MAPPING, &body, &mut extra);
    }
    extra
}
#[test]
fn video_mapping_family_preserves_selected_packet_and_track_type_order() {
    for first in [false, true] {
        let bytes = mapping_fixture(&video_mapping(), 1, first);
        let mut stream = open_streaming(Cursor::new(bytes)).unwrap();
        stream.set_track_filter(Some(vec![2]));
        let mut payload = Vec::new();
        let frame = stream.next_frame_into(&mut payload).unwrap().unwrap();
        assert_eq!((frame.track, frame.timestamp_ns), (2, 105_000_000));
        assert_eq!(payload, [1, 2, 3, 4]);
        assert!(stream.next_frame_into(&mut payload).unwrap().is_none());
    }
}
#[test]
fn video_mapping_does_not_allow_other_track_transforms_or_escape_caps() {
    for kind in [2, 17, 257] {
        assert!(
            open_streaming(Cursor::new(mapping_fixture(&video_mapping(), kind, true))).is_err()
        );
    }
    let mut malformed = Vec::new();
    write_element(ids::BLOCK_ADD_ID_TYPE, &[0; 9], &mut malformed);
    let mut extra = Vec::new();
    write_master(ids::BLOCK_ADDITION_MAPPING, &malformed, &mut extra);
    assert!(open_streaming(Cursor::new(mapping_fixture(&extra, 1, false))).is_err());
    for limits in [
        StreamingLimits {
            metadata_element_bytes: 128,
            ..Default::default()
        },
        StreamingLimits {
            total_metadata_bytes: 64,
            ..Default::default()
        },
        StreamingLimits {
            elements: 3,
            ..Default::default()
        },
    ] {
        assert!(
            open_streaming_with_limits(
                Cursor::new(mapping_fixture(&video_mapping(), 1, true)),
                limits
            )
            .is_err()
        );
    }
}
fn walk(bytes: Vec<u8>) -> media_mkv_webm::Result<usize> {
    let mut stream = open_streaming(Cursor::new(bytes))?;
    stream.set_track_filter(Some(vec![1]));
    let mut count = 0;
    while stream.next_frame_into(&mut std::io::sink())?.is_some() {
        count += 1;
    }
    Ok(count)
}
#[test]
fn default_and_configured_streaming_share_original_mux_fixtures() {
    let bytes = muxed(
        &[("S_TEXT/UTF8", TrackKind::Subtitle)],
        &[(1, 400, DIALOGUE[0].as_bytes())],
    );
    let mut default = open_streaming(Cursor::new(bytes.clone())).unwrap();
    assert_eq!(
        default.next_frame().unwrap().unwrap().timestamp_ns,
        400_000_000
    );
    let mut strict =
        open_streaming_with_limits(Cursor::new(bytes), StreamingLimits::default()).unwrap();
    let mut out = Vec::new();
    let h = strict.next_frame_into(&mut out).unwrap().unwrap();
    assert_eq!(h.timestamp_ns, 400_000_000);
    assert_eq!(out, DIALOGUE[0].as_bytes());
    assert!(strict.next_frame_into(&mut out).unwrap().is_none());
}
#[test]
fn huge_declared_metadata_and_unknown_sizes_fail_before_allocation() {
    for id in [ids::TRACKS, ids::INFO, ids::CUES, ids::SEEK_HEAD] {
        let mut bytes = header();
        bytes.extend_from_slice(&(id as u32).to_be_bytes());
        write_size_vint(1 << 40, &mut bytes);
        assert!(walk(bytes).is_err());
    }
    let mut bytes = header();
    open_master_unknown_size(ids::TRACKS, &mut bytes);
    assert!(walk(bytes).is_err());
    let tracks = track(1, "S_TEXT/UTF8", &[]);
    let bytes = container(&tracks, 1_000_000, &[]);
    let limits = StreamingLimits {
        metadata_element_bytes: 8,
        ..Default::default()
    };
    assert!(open_streaming_with_limits(Cursor::new(bytes), limits).is_err());
}
#[test]
fn malformed_suffix_and_parent_bounds_are_not_clean_eof() {
    let tracks = track(1, "S_TEXT/UTF8", &[]);
    let good = cluster(0, &block(ids::SIMPLE_BLOCK, 1, 0, 0, b"valid"));
    for suffix in [
        vec![0x1f],
        vec![0x1f, 0x43, 0xb6, 0x75, 0x88, 0xe7],
        vec![0xa3, 0x85, 0x81],
    ] {
        let mut tail = good.clone();
        tail.extend(suffix);
        assert!(walk(container(&tracks, 1_000_000, &tail)).is_err());
    }
    let malformed = cluster(0, &[0xa3, 0x8f, 0x81, 0, 0, 0]);
    assert!(walk(container(&tracks, 1_000_000, &malformed)).is_err());
    let mut extra = tracks;
    extra.push(0xae);
    assert!(walk(container(&extra, 1_000_000, &good)).is_err());
}
#[test]
fn selected_lacing_short_and_multiple_blocks_are_errors() {
    let tracks = track(1, "S_TEXT/UTF8", &[]);
    for flags in [2, 4, 6] {
        assert!(
            walk(container(
                &tracks,
                1_000_000,
                &cluster(0, &block(ids::SIMPLE_BLOCK, 1, 0, flags, b"lace"))
            ))
            .is_err()
        );
    }
    assert!(
        walk(container(
            &tracks,
            1_000_000,
            &cluster(0, &[0xa3, 0x81, 0x81])
        ))
        .is_err()
    );
    let mut blocks = block(ids::BLOCK, 1, 0, 0, b"first");
    blocks.extend(block(ids::BLOCK, 1, 0, 0, b"second"));
    assert!(
        walk(container(
            &tracks,
            1_000_000,
            &cluster(0, &group(&blocks, Some(20)))
        ))
        .is_err()
    );
}
#[test]
fn timestamp_overflow_negative_and_duration_overflow_rejected() {
    let tracks = track(1, "S_TEXT/UTF8", &[]);
    for (ticks, scale, delta) in [
        (u64::MAX, 1_000_000, 0),
        (0, 1_000_000, -1),
        (1, u64::MAX, 1),
        (0, 0, 0),
    ] {
        assert!(
            walk(container(
                &tracks,
                scale,
                &cluster(ticks, &block(ids::SIMPLE_BLOCK, 1, delta, 0, b"text"))
            ))
            .is_err()
        );
    }
    let grouped = group(&block(ids::BLOCK, 1, 0, 0, b"text"), Some(u64::MAX));
    assert!(walk(container(&tracks, 1_000_000, &cluster(0, &grouped))).is_err());
    // Valid ns above i64::MAX must not wrap/clamp through a signed cast.
    let bytes = container(
        &tracks,
        1,
        &cluster(
            i64::MAX as u64 + 1,
            &block(ids::SIMPLE_BLOCK, 1, 0, 0, b"text"),
        ),
    );
    let mut stream = open_streaming_with_limits(Cursor::new(bytes), Default::default()).unwrap();
    assert_eq!(
        stream
            .next_frame_into(&mut std::io::sink())
            .unwrap()
            .unwrap()
            .timestamp_ns,
        i64::MAX as u64 + 1
    );
}
#[test]
fn transforms_alternative_timing_and_metadata_controls_rejected() {
    for id in [0x6d80, 0x23314f, 0x537f, 0xe2] {
        let mut extra = Vec::new();
        write_element(id, &[], &mut extra);
        assert!(walk(container(&track(1, "S_TEXT/UTF8", &extra), 1_000_000, &[])).is_err());
    }
    let mut extra = Vec::new();
    write_element(ids::NAME, b"bad\0hidden", &mut extra);
    assert!(walk(container(&track(1, "S_TEXT/UTF8", &extra), 1_000_000, &[])).is_err());
}
// Historical MinCache: official Matroska schema (ebml_matroska.xml), ID 0x6DE7,
// unsigned/default 0; RFC 8794 sections 6.1 and 7.2 allow empty through 8 bytes.
const MIN_CACHE: u64 = 0x6DE7;

fn video_min_cache_and_pgs(payload: &[u8]) -> Vec<u8> {
    let mut extra = Vec::new();
    write_element(MIN_CACHE, payload, &mut extra);
    let mut tracks = track_kind(1, "V_MPEG4/ISO/AVC", 1, &extra);
    let mut language = Vec::new();
    write_ascii(ids::LANGUAGE, "eng", &mut language);
    tracks.extend(track(8, "S_HDMV/PGS", &language));
    container(&tracks, 1_000_000, &[])
}

#[test]
fn historical_min_cache_unsigned_metadata_allows_probe_but_not_pgs_extraction() {
    use tvmatch::media::{MediaError, extract_subtitles, probe_subtitle_tracks};
    for payload in [
        vec![],
        vec![0],
        vec![1],
        vec![255],
        vec![0, 0, 0, 0, 0, 0, 0, 1],
        vec![255; 8],
    ] {
        let bytes = video_min_cache_and_pgs(&payload);
        let stream = open_streaming(Cursor::new(bytes.clone())).unwrap();
        assert_eq!(stream.demuxer.tracks.len(), 2);
        let tracks = probe_subtitle_tracks(Cursor::new(bytes.clone())).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].number, 8);
        assert_eq!(tracks[0].codec_id, "S_HDMV/PGS");
        assert_eq!(tracks[0].language.as_deref(), Some("eng"));
        assert!(!tracks[0].supported);
        assert!(matches!(
            extract_subtitles(Cursor::new(bytes), Some(8)),
            Err(MediaError::UnsupportedTrack(8))
        ));
    }
}

#[test]
fn historical_min_cache_rejects_invalid_unsigned_width_and_duplicates() {
    let err = match open_streaming(Cursor::new(video_min_cache_and_pgs(&[0; 9]))) {
        Ok(_) => panic!("accepted 9-byte MinCache"),
        Err(err) => err.to_string(),
    };
    assert!(err.contains("0x6DE7"), "{err}");
    assert!(err.contains("0xAE"), "{err}");
    let mut extra = Vec::new();
    write_uint(MIN_CACHE, 1, &mut extra);
    write_uint(MIN_CACHE, 2, &mut extra);
    assert!(
        walk(container(
            &track_kind(1, "V_MPEG4/ISO/AVC", 1, &extra),
            1,
            &[]
        ))
        .is_err()
    );
}

#[test]
fn unsupported_metadata_diagnostic_identifies_element_and_parent() {
    for (parent, id) in [
        (ids::TRACK_ENTRY, 0x6D80),   // ContentEncodings
        (ids::TRACK_ENTRY, 0x23314F), // TrackTimestampScale
        (ids::TRACK_ENTRY, 0x537F),   // TrackOffset
        (ids::TRACK_ENTRY, 0xE2),     // TrackOperation
        (ids::TRACK_ENTRY, 0x6DF8),   // MaxCache is NOT part of this extension
        (ids::VIDEO, MIN_CACHE),      // MinCache is only valid under TrackEntry
        (ids::SEGMENT, MIN_CACHE),
    ] {
        let mut extra = Vec::new();
        write_element(id, &[], &mut extra);
        let bytes = if parent == ids::SEGMENT {
            container(&track(1, "S_TEXT/UTF8", &[]), 1, &extra)
        } else {
            if parent == ids::VIDEO {
                let mut video = Vec::new();
                write_master(ids::VIDEO, &extra, &mut video);
                extra = video;
            }
            container(&track_kind(1, "V_MPEG4/ISO/AVC", 1, &extra), 1, &[])
        };
        let err = match tvmatch::media::probe_subtitle_tracks(Cursor::new(bytes)) {
            Ok(_) => panic!("accepted unsupported metadata"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains(&format!("element 0x{id:X}")), "{err}");
        assert!(err.contains(&format!("parent 0x{parent:X}")), "{err}");
    }
}

#[test]
fn read_walk_metadata_and_io_budgets_are_cumulative() {
    let bytes = muxed(&[("S_TEXT/UTF8", TrackKind::Subtitle)], &[(1, 0, b"text")]);
    for limits in [
        StreamingLimits {
            elements: 2,
            ..Default::default()
        },
        StreamingLimits {
            read_bytes: 1,
            ..Default::default()
        },
        StreamingLimits {
            io_operations: 1,
            ..Default::default()
        },
        StreamingLimits {
            total_metadata_bytes: 20,
            ..Default::default()
        },
    ] {
        assert!(open_streaming_with_limits(Cursor::new(bytes.clone()), limits).is_err());
    }
    let mut tail = Vec::new();
    for _ in 0..100 {
        write_element(ids::VOID, &[], &mut tail);
    }
    let bytes = container(&track(1, "S_TEXT/UTF8", &[]), 1_000_000, &cluster(0, &tail));
    let mut s = open_streaming_with_limits(
        Cursor::new(bytes),
        StreamingLimits {
            elements: 30,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(s.next_frame_into(&mut std::io::sink()).is_err());
}
#[test]
fn large_unselected_payload_is_seek_skipped_and_head_boundary_is_not_parsed() {
    let mut tracks = track(1, "S_TEXT/UTF8", &[]);
    tracks.extend(track_kind(2, "V_VP9", 1, &[]));
    let mut blocks = block(ids::SIMPLE_BLOCK, 2, 0, 2, &vec![0; 4 * 1024 * 1024]);
    blocks.extend(block(ids::SIMPLE_BLOCK, 1, 0, 0, b"selected"));
    let mut tail = Vec::new();
    write_element(ids::VOID, &vec![0; 2 * 1024 * 1024], &mut tail);
    tail.extend(cluster(0, &blocks));
    // Budget smaller than skipped payload proves it cannot be read in full.
    let bytes = container(&tracks, 1_000_000, &tail);
    let mut s = open_streaming_with_limits(
        Cursor::new(bytes),
        StreamingLimits {
            read_bytes: 1024 * 1024,
            ..Default::default()
        },
    )
    .unwrap();
    s.set_track_filter(Some(vec![1]));
    let mut out = Vec::new();
    s.next_frame_into(&mut out).unwrap().unwrap();
    assert_eq!(out, b"selected");
    assert!(s.next_frame_into(&mut out).unwrap().is_none());
}

#[test]
fn default_open_discovers_segment_and_metadata_after_cluster() {
    let mut ebml = Vec::new();
    write_ascii(ids::DOC_TYPE, "matroska", &mut ebml);
    let mut bytes = Vec::new();
    write_master(ids::EBML, &ebml, &mut bytes);
    write_element(ids::VOID, b"original padding before Segment", &mut bytes);
    let mut segment = cluster(123, &block(ids::SIMPLE_BLOCK, 1, 0, 0, b"late metadata"));
    write_master(ids::TRACKS, &track(1, "S_TEXT/UTF8", &[]), &mut segment);
    let mut info = Vec::new();
    write_uint(ids::TIMESTAMP_SCALE, 1000, &mut info);
    write_master(ids::INFO, &info, &mut segment);
    write_master(ids::SEGMENT, &segment, &mut bytes);
    let mut s = open_streaming(Cursor::new(bytes)).unwrap();
    assert_eq!(s.demuxer.tracks[0].number, 1);
    assert_eq!(
        s.next_frame_into(&mut std::io::sink())
            .unwrap()
            .unwrap()
            .timestamp_ns,
        123_000
    );
    assert!(s.next_frame_into(&mut std::io::sink()).unwrap().is_none());
}

#[test]
fn many_distant_clusters_open_without_payload_readahead_and_keep_walk_budget() {
    use std::{
        cell::Cell,
        io::{Read, Seek, SeekFrom},
        rc::Rc,
    };
    struct CountedReader {
        data: Cursor<Vec<u8>>,
        bytes: Rc<Cell<u64>>,
    }
    impl Read for CountedReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let n = self.data.read(out)?;
            self.bytes.set(self.bytes.get() + n as u64);
            Ok(n)
        }
    }
    impl Seek for CountedReader {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.data.seek(pos)
        }
    }
    let mut tracks = track(1, "S_TEXT/UTF8", &[]);
    tracks.extend(track_kind(2, "V_VP9", 1, &[]));
    let mut tail = Vec::new();
    let mut offsets = Vec::new();
    for ticks in 0..32 {
        offsets.push(tail.len());
        let mut blocks = block(ids::SIMPLE_BLOCK, 2, 0, 0, &vec![0; 128 * 1024]);
        blocks.extend(block(ids::SIMPLE_BLOCK, 1, 0, 0, b"caption"));
        tail.extend(cluster(ticks, &blocks));
    }
    let bytes = container(&tracks, 1_000_000, &tail);
    let first_cluster = bytes.len() - tail.len();
    let read_bytes = Rc::new(Cell::new(0));
    let mut stream = open_streaming(CountedReader {
        data: Cursor::new(bytes.clone()),
        bytes: read_bytes.clone(),
    })
    .unwrap();
    let open_bytes = read_bytes.get();
    assert!(open_bytes < 4096, "open read {open_bytes} bytes");
    assert_eq!(stream.demuxer.tracks.len(), 2);
    assert_eq!(stream.demuxer.clusters_offset, first_cluster);
    stream.set_track_filter(Some(vec![1]));
    // Opening leaves the raw reader near EOF; the buffered walk must seek correctly.
    assert_eq!(stream.next_frame().unwrap().unwrap().data, b"caption");
    stream
        .seek_to_byte((first_cluster + offsets[31]) as u64)
        .unwrap();
    assert_eq!(
        stream.next_frame().unwrap().unwrap().timestamp_ns,
        31_000_000
    );
    assert!(stream.next_frame().unwrap().is_none());

    // Exactly the measured open allowance must not be reset when wrapping the
    // same BudgetReader in a BufReader for the subsequent frame walk.
    let mut tight = open_streaming_with_limits(
        Cursor::new(bytes),
        StreamingLimits {
            read_bytes: open_bytes,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(tight.demuxer.clusters_offset, first_cluster);
    assert!(matches!(
        tight.next_frame(),
        Err(media_mkv_webm::Error::Io(_))
    ));
}

#[test]
fn explicit_forward_only_mode_skips_large_opaque_cues_without_allocating_them() {
    let tracks = track(1, "S_TEXT/UTF8", &[]);
    let mut tail = cluster(0, &block(ids::SIMPLE_BLOCK, 1, 0, 0, b"caption"));
    write_element(ids::CUES, &vec![0; 3 * 1024 * 1024], &mut tail);
    let bytes = container(&tracks, 1_000_000, &tail);
    let mut stream = open_streaming_with_limits(
        Cursor::new(bytes),
        StreamingLimits {
            skip_cues: true,
            read_bytes: 1024 * 1024,
            total_metadata_bytes: 1024,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(stream.demuxer.cues.is_empty());
    assert!(
        stream
            .next_frame_into(&mut std::io::sink())
            .unwrap()
            .is_some()
    );
    assert!(
        stream
            .next_frame_into(&mut std::io::sink())
            .unwrap()
            .is_none()
    );
}

fn cue(ticks: u64, offset: u64) -> Vec<u8> {
    let mut position = Vec::new();
    write_uint(ids::CUE_TRACK, 1, &mut position);
    write_uint(ids::CUE_CLUSTER_POSITION, offset, &mut position);
    let mut point = Vec::new();
    write_uint(ids::CUE_TIME, ticks, &mut point);
    write_master(ids::CUE_TRACK_POSITIONS, &position, &mut point);
    let mut out = Vec::new();
    write_master(ids::CUE_POINT, &point, &mut out);
    out
}
#[test]
fn default_tail_cues_use_final_scale_and_seek_to_preceding_cue() {
    let first = cluster(10, &block(ids::SIMPLE_BLOCK, 1, 0, 0, b"first"));
    let second = cluster(20, &block(ids::SIMPLE_BLOCK, 1, 0, 0, b"second"));
    let mut segment = first.clone();
    segment.extend(second);
    let mut cues = cue(10, 0);
    cues.extend(cue(20, first.len() as u64));
    write_master(ids::CUES, &cues, &mut segment);
    write_master(ids::TRACKS, &track(1, "S_TEXT/UTF8", &[]), &mut segment);
    let mut info = Vec::new();
    write_uint(ids::TIMESTAMP_SCALE, 2_000_000, &mut info);
    write_master(ids::INFO, &info, &mut segment); // Info deliberately after Cues.
    let mut bytes = header();
    bytes.extend(segment);
    let mut s = open_streaming(Cursor::new(bytes)).unwrap();
    assert_eq!(s.demuxer.cues.len(), 2);
    assert_eq!(s.demuxer.cues[1].ts_ns, 40_000_000);
    s.seek_to_time(45_000_000, Some(1)).unwrap();
    assert_eq!(s.next_frame().unwrap().unwrap().data, b"second");
    s.seek_to_time(35_000_000, Some(1)).unwrap();
    assert_eq!(s.next_frame().unwrap().unwrap().data, b"first");
    s.seek_to_time(0, Some(1)).unwrap();
    assert_eq!(s.next_frame().unwrap().unwrap().data, b"first");
    s.seek_to_time(50_000_000, Some(99)).unwrap();
    assert_eq!(s.next_frame().unwrap().unwrap().data, b"first");
    s.demuxer.cues.clear();
    s.seek_to_time(0, None).unwrap();
    assert_eq!(s.next_frame().unwrap().unwrap().data, b"first");
}
#[test]
fn default_cues_are_bounded_checked_and_never_saturate_or_seek_into_payload() {
    let mut segment = cluster(0, &block(ids::SIMPLE_BLOCK, 1, 0, 0, b"caption"));
    write_master(ids::TRACKS, &track(1, "S_TEXT/UTF8", &[]), &mut segment);
    for cues in [
        cue(1, 1),
        cue(1, u64::MAX),
        cue(u64::MAX, 0),
        vec![0xbb],
        vec![0xbb, 0x80],
        vec![0; 1_048_577],
    ] {
        let mut bytes = header();
        bytes.extend_from_slice(&segment);
        write_master(ids::CUES, &cues, &mut bytes);
        assert!(open_streaming(Cursor::new(bytes)).is_err());
    }
    let mut bytes = header();
    bytes.extend_from_slice(&segment);
    write_master(ids::CUES, &cue(0, 0), &mut bytes);
    // Enough for EBML and Tracks, not their sum plus Cues.
    assert!(
        open_streaming_with_limits(
            Cursor::new(bytes),
            StreamingLimits {
                total_metadata_bytes: 11 + track(1, "S_TEXT/UTF8", &[]).len(),
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn underlying_io_errors_after_successful_open_are_not_eof() {
    use std::{
        cell::Cell,
        io::{Read, Seek, SeekFrom},
        rc::Rc,
    };
    struct FailingReader {
        data: Cursor<Vec<u8>>,
        fail: Rc<Cell<bool>>,
    }
    impl Read for FailingReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.fail.get() {
                return Err(std::io::Error::other("injected read error"));
            }
            self.data.read(out)
        }
    }
    impl Seek for FailingReader {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.data.seek(pos)
        }
    }
    let bytes = single_packet_for_io();
    let fail = Rc::new(Cell::new(false));
    let mut s = open_streaming(FailingReader {
        data: Cursor::new(bytes),
        fail: fail.clone(),
    })
    .unwrap();
    assert!(s.next_frame_into(&mut std::io::sink()).unwrap().is_some());
    s.seek_to_time(0, None).unwrap();
    fail.set(true);
    assert!(matches!(
        s.next_frame_into(&mut std::io::sink()),
        Err(media_mkv_webm::Error::Io(_))
    ));
}
fn single_packet_for_io() -> Vec<u8> {
    container(
        &track(1, "S_TEXT/UTF8", &[]),
        1_000_000,
        &cluster(0, &block(ids::SIMPLE_BLOCK, 1, 0, 0, b"valid caption")),
    )
}

#[test]
fn filtered_walk_skips_interleaved_payloads_without_readahead_amplification() {
    use std::{
        cell::Cell,
        io::{Read, Seek, SeekFrom},
        rc::Rc,
    };
    struct Counted {
        data: Cursor<Vec<u8>>,
        bytes: Rc<Cell<u64>>,
        calls: Rc<Cell<u64>>,
    }
    impl Read for Counted {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            self.calls.set(self.calls.get() + 1);
            let n = self.data.read(b)?;
            self.bytes.set(self.bytes.get() + n as u64);
            Ok(n)
        }
    }
    impl Seek for Counted {
        fn seek(&mut self, p: SeekFrom) -> std::io::Result<u64> {
            self.data.seek(p)
        }
    }
    let tracks = [
        track_kind(1, "V_TEST", 1, &[]),
        track_kind(2, "A_TEST", 2, &[]),
        track(3, "S_HDMV/PGS", &[]),
    ]
    .concat();
    let mut blocks = Vec::new();
    for i in 0..100i16 {
        blocks.extend(block(ids::SIMPLE_BLOCK, 1, i, 0x80, &vec![0; 96 * 1024]));
        blocks.extend(block(ids::SIMPLE_BLOCK, 2, i, 0x80, &vec![0; 8 * 1024]));
        blocks.extend(block(ids::SIMPLE_BLOCK, 3, i, 0x80, &[0x80, 0, 0])); // original END framing, demux only
    }
    let bytes = container(&tracks, 1_000_000, &cluster(0, &blocks));
    let read = Rc::new(Cell::new(0));
    let calls = Rc::new(Cell::new(0));
    let mut stream = open_streaming(Counted {
        data: Cursor::new(bytes),
        bytes: read.clone(),
        calls: calls.clone(),
    })
    .unwrap();
    stream.set_track_filter(Some(vec![3]));
    for i in 0..100 {
        let f = stream.next_frame().unwrap().unwrap();
        assert_eq!(f.track, 3);
        assert_eq!(f.timestamp_ns, i * 1_000_000);
        assert_eq!(f.data, [0x80, 0, 0]);
    }
    assert!(stream.next_frame().unwrap().is_none());
    eprintln!(
        "filtered underlying bytes={} read_calls={}",
        read.get(),
        calls.get()
    );
    assert!(read.get() < 32 * 1024, "underlying bytes={}", read.get());
    assert!(calls.get() < 500, "read calls={}", calls.get());
    stream.seek_to_time(0, Some(3)).unwrap();
    stream.set_track_filter(None);
    assert_eq!(stream.next_frame().unwrap().unwrap().track, 1);
    stream.set_track_filter(Some(vec![3]));
    assert_eq!(stream.next_frame().unwrap().unwrap().track, 3);
    stream.set_track_filter(Some(vec![2]));
    assert_eq!(stream.next_frame().unwrap().unwrap().track, 2);
}

#[test]
fn filtered_walk_retains_io_byte_operation_and_selected_writer_failures() {
    use std::{
        cell::Cell,
        io::{Read, Seek, SeekFrom, Write},
        rc::Rc,
    };
    struct Fault {
        data: Cursor<Vec<u8>>,
        fail: Rc<Cell<bool>>,
    }
    impl Read for Fault {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            if self.fail.get() {
                Err(std::io::Error::other("injected filtered read"))
            } else {
                self.data.read(b)
            }
        }
    }
    impl Seek for Fault {
        fn seek(&mut self, p: SeekFrom) -> std::io::Result<u64> {
            self.data.seek(p)
        }
    }
    let tracks = [track_kind(1, "V_TEST", 1, &[]), track(2, "S_HDMV/PGS", &[])].concat();
    let blocks = [
        block(ids::SIMPLE_BLOCK, 1, 0, 0x80, &vec![0; 128 * 1024]),
        block(ids::SIMPLE_BLOCK, 2, 0, 0x80, &vec![42; 128 * 1024]),
    ]
    .concat();
    let bytes = container(&tracks, 1_000_000, &cluster(0, &blocks));
    let fail = Rc::new(Cell::new(false));
    let mut s = open_streaming(Fault {
        data: Cursor::new(bytes.clone()),
        fail: fail.clone(),
    })
    .unwrap();
    s.set_track_filter(Some(vec![2]));
    fail.set(true);
    assert!(
        s.next_frame()
            .unwrap_err()
            .to_string()
            .contains("injected filtered read")
    );
    for limits in [
        StreamingLimits {
            elements: 1_000_000,
            io_operations: 4_000_000,
            read_bytes: 4096,
            ..Default::default()
        },
        StreamingLimits {
            elements: 1_000_000,
            io_operations: 50,
            ..Default::default()
        },
        StreamingLimits {
            elements: 100,
            io_operations: 4_000_000,
            ..Default::default()
        },
    ] {
        let mut s = open_streaming_with_limits(Cursor::new(bytes.clone()), limits).unwrap();
        s.set_track_filter(Some(vec![2]));
        let mut exhausted = false;
        for i in 0..100 {
            // Both filtered and all-track walking spend the same cumulative
            // budget; changing policy and seeking must never replenish it.
            s.set_track_filter(if i % 2 == 0 { Some(vec![2]) } else { None });
            if s.next_frame_into(&mut std::io::sink()).is_err()
                || s.seek_to_time(0, Some(2)).is_err()
            {
                exhausted = true;
                break;
            }
        }
        assert!(exhausted, "filter/seek must not reset cumulative budgets");
    }
    struct Reject;
    impl Write for Reject {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("selected sink cap"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut s = open_streaming(Cursor::new(bytes.clone())).unwrap();
    s.set_track_filter(Some(vec![2]));
    assert!(
        s.next_frame_into(&mut Reject)
            .unwrap_err()
            .to_string()
            .contains("selected sink cap")
    );
    for end in [bytes.len() - 1, bytes.len() - 128 * 1024] {
        assert!(open_streaming(Cursor::new(bytes[..end].to_vec())).is_err());
    }
}
