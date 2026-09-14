use super::*;
use crate::ebml::writer::*;
use std::io::Cursor;
fn mapping(kind: u64, extra: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    write_uint(ids::BLOCK_ADD_ID_TYPE, kind, &mut body);
    write_element(ids::BLOCK_ADD_ID_EXTRA_DATA, extra, &mut body);
    let mut out = Vec::new();
    write_master(ids::BLOCK_ADDITION_MAPPING, &body, &mut out);
    out
}
fn entry(kind: u64, extra: &[u8]) -> Vec<u8> {
    let mut out = extra.to_vec();
    write_uint(ids::TRACK_TYPE, kind, &mut out);
    out
}
fn check(bytes: &[u8], parent: u64) -> Result<()> {
    validate_metadata(bytes, parent, &mut WalkBudget { elements: 1000 })
}
#[test]
fn repeated_video_mapping_family_is_order_independent_but_subtitle_extensions_fail() {
    let mut maps = mapping(u32::from_be_bytes(*b"dvcC") as u64, &[0; 24]);
    maps.extend(mapping(u32::from_be_bytes(*b"hvcE") as u64, &[0; 187]));
    assert!(check(&entry(1, &maps), ids::TRACK_ENTRY).is_ok());
    let mut before = entry(1, &[]);
    before.extend(&maps);
    assert!(check(&before, ids::TRACK_ENTRY).is_ok());
    for kind in [2, 17, 0, 257] {
        assert!(check(&entry(kind, &maps), ids::TRACK_ENTRY).is_err());
    }
    assert!(check(&maps, ids::TRACK_ENTRY).is_err());
    for value in [0, 1, u64::MAX] {
        let mut b = Vec::new();
        write_uint(ids::MAX_BLOCK_ADDITION_ID, value, &mut b);
        assert!(check(&entry(1, &b), ids::TRACK_ENTRY).is_ok());
        assert_eq!(check(&entry(17, &b), ids::TRACK_ENTRY).is_ok(), value == 0);
    }
}
#[test]
fn mapping_scalar_widths_names_and_duplicate_children_are_validated() {
    for width in 0..=9 {
        let mut b = Vec::new();
        write_element(ids::BLOCK_ADD_ID_TYPE, &vec![0xff; width], &mut b);
        assert_eq!(check(&b, ids::BLOCK_ADDITION_MAPPING).is_ok(), width <= 8);
        let mut max = Vec::new();
        write_element(ids::MAX_BLOCK_ADDITION_ID, &vec![0xff; width], &mut max);
        assert_eq!(check(&entry(1, &max), ids::TRACK_ENTRY).is_ok(), width <= 8);
    }
    for value in [0, 1, 2, u64::MAX] {
        let mut b = Vec::new();
        write_uint(ids::BLOCK_ADD_ID_VALUE, value, &mut b);
        assert_eq!(check(&b, ids::BLOCK_ADDITION_MAPPING).is_ok(), value >= 2);
    }
    let mut b = Vec::new();
    write_element(ids::BLOCK_ADD_ID_VALUE, &[1; 9], &mut b);
    assert!(check(&b, ids::BLOCK_ADDITION_MAPPING).is_err());
    for name in [
        b"HDR config".as_slice(),
        b"",
        b"bad\nname",
        b"\xff",
        b"\x7f",
    ] {
        let mut b = Vec::new();
        write_element(ids::BLOCK_ADD_ID_NAME, name, &mut b);
        assert_eq!(
            check(&b, ids::BLOCK_ADDITION_MAPPING).is_ok(),
            name.iter().all(|b| (0x20..=0x7e).contains(b))
        );
    }
    for id in [
        ids::BLOCK_ADD_ID_VALUE,
        ids::BLOCK_ADD_ID_TYPE,
        ids::BLOCK_ADD_ID_NAME,
        ids::BLOCK_ADD_ID_EXTRA_DATA,
    ] {
        let mut b = Vec::new();
        write_element(id, &[65], &mut b);
        write_element(id, &[65], &mut b);
        assert!(check(&b, ids::BLOCK_ADDITION_MAPPING).is_err());
    }
}
#[test]
fn mapping_unknown_children_wrong_parents_and_malformed_boundaries_still_fail() {
    let maps = mapping(1, b"opaque");
    for parent in [
        ids::INFO,
        ids::VIDEO,
        ids::AUDIO,
        ids::BLOCK_ADDITION_MAPPING,
    ] {
        assert!(check(&maps, parent).is_err());
    }
    let mut child = Vec::new();
    write_uint(ids::BLOCK_ADD_ID_TYPE, 1, &mut child);
    assert!(check(&entry(1, &child), ids::TRACK_ENTRY).is_err());
    for body in [
        vec![0x41],
        vec![0x41, 0xed, 0x85, 0],
        vec![0x41, 0xed, 0xff],
        vec![0x41, 0xee, 0x80],
    ] {
        let mut b = Vec::new();
        write_master(ids::BLOCK_ADDITION_MAPPING, &body, &mut b);
        assert!(check(&entry(1, &b), ids::TRACK_ENTRY).is_err());
    }
    // Binary configuration is opaque, not another EBML tree to follow.
    assert!(
        check(
            &entry(1, &mapping(1, &[0x41, 0xed, 0xff])),
            ids::TRACK_ENTRY
        )
        .is_ok()
    );
}
fn file(extra: &[u8]) -> Vec<u8> {
    let mut header = Vec::new();
    write_utf8(ids::DOC_TYPE, "matroska", &mut header);
    let mut bytes = Vec::new();
    write_master(ids::EBML, &header, &mut bytes);
    open_master_unknown_size(ids::SEGMENT, &mut bytes);
    let mut info = Vec::new();
    write_uint(ids::TIMESTAMP_SCALE, 1_000_000, &mut info);
    write_master(ids::INFO, &info, &mut bytes);
    let mut tracks = Vec::new();
    for (number, kind, codec) in [(1, 1, "V_MPEGH/ISO/HEVC"), (2, 17, "S_HDMV/PGS")] {
        let mut t = Vec::new();
        write_uint(ids::TRACK_NUMBER, number, &mut t);
        write_uint(ids::TRACK_UID, number, &mut t);
        if kind == 1 {
            t.extend(extra);
        }
        write_uint(ids::TRACK_TYPE, kind, &mut t);
        write_ascii(ids::CODEC_ID, codec, &mut t);
        write_ascii(ids::LANGUAGE, "eng", &mut t);
        write_master(ids::TRACK_ENTRY, &t, &mut tracks);
    }
    write_master(ids::TRACKS, &tracks, &mut bytes);
    let mut cluster = Vec::new();
    write_uint(ids::TIMESTAMP, 100, &mut cluster);
    write_element(
        ids::SIMPLE_BLOCK,
        &[0x81, 0, 0, 0x80, 9, 8, 7],
        &mut cluster,
    );
    write_element(
        ids::SIMPLE_BLOCK,
        &[0x82, 0, 5, 0, 1, 2, 3, 4],
        &mut cluster,
    );
    write_master(ids::CLUSTER, &cluster, &mut bytes);
    bytes
}
#[test]
fn actual_streaming_parser_skips_video_and_preserves_selected_subtitle_packet() {
    let mut extra = mapping(u32::from_be_bytes(*b"dvcC") as u64, &[0; 24]);
    extra.extend(mapping(u32::from_be_bytes(*b"hvcE") as u64, &[0; 187]));
    let mut stream =
        open_streaming_with_limits(Cursor::new(file(&extra)), StreamingLimits::default()).unwrap();
    assert_eq!(stream.demuxer.tracks.len(), 2);
    stream.set_track_filter(Some(vec![2]));
    let mut out = Vec::new();
    let frame = stream.next_frame_into(&mut out).unwrap().unwrap();
    assert_eq!((frame.track, frame.timestamp_ns), (2, 105_000_000));
    assert_eq!(out, [1, 2, 3, 4]);
    assert!(stream.next_frame_into(&mut out).unwrap().is_none());
    let mut malformed = Vec::new();
    write_element(ids::BLOCK_ADD_ID_TYPE, &[0; 9], &mut malformed);
    let mut extra = Vec::new();
    write_master(ids::BLOCK_ADDITION_MAPPING, &malformed, &mut extra);
    assert!(
        open_streaming_with_limits(Cursor::new(file(&extra)), StreamingLimits::default()).is_err()
    );
}
#[test]
fn mapping_family_uses_existing_cumulative_element_and_metadata_limits() {
    let b = entry(1, &mapping(1, &[0; 32]));
    assert!(validate_metadata(&b, ids::TRACK_ENTRY, &mut WalkBudget { elements: 3 }).is_err());
    assert!(validate_metadata(&b, ids::TRACK_ENTRY, &mut WalkBudget { elements: 4 }).is_ok());
    let bytes = file(&mapping(1, &[0; 512]));
    let limits = StreamingLimits {
        metadata_element_bytes: 256,
        ..Default::default()
    };
    assert!(open_streaming_with_limits(Cursor::new(bytes), limits).is_err());
    let bytes = file(&mapping(1, &[0; 128]));
    let limits = StreamingLimits {
        total_metadata_bytes: 64,
        ..Default::default()
    };
    assert!(open_streaming_with_limits(Cursor::new(bytes), limits).is_err());
}
#[test]
#[ignore = "opt-in bounded local PGS scan; counts only, no OCR or caption output"]
fn inspect_local_pgs_after_video_mapping() {
    use crate::pgs::{PgsDecoder, PgsLimits};
    let Some(folder) = std::env::var_os("TVMATCH_TEST_MAPPING_FOLDER") else {
        return;
    };
    let mut paths = std::fs::read_dir(folder)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("mkv")))
        .collect::<Vec<_>>();
    assert!(paths.len() <= 32);
    paths.sort();
    struct Packet(Vec<u8>);
    impl std::io::Write for Packet {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if b.len() > PgsLimits::default().packet_bytes - self.0.len() {
                return Err(std::io::Error::other("packet cap"));
            }
            self.0.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for path in &paths {
        let before = std::fs::metadata(path).unwrap();
        let limits = StreamingLimits {
            skip_cues: true,
            elements: 1_000_000,
            io_operations: 4_000_000,
            ..Default::default()
        };
        let mut stream =
            open_streaming_with_limits(std::fs::File::open(path).unwrap(), limits).unwrap();
        let track = stream
            .demuxer
            .tracks
            .iter()
            .find(|t| {
                t.kind == crate::TrackKind::Subtitle
                    && t.codec_id == "S_HDMV/PGS"
                    && t.flag_enabled
                    && !t.flag_forced
                    && t.language.as_deref().is_some_and(|l| {
                        l.eq_ignore_ascii_case("eng") || l.eq_ignore_ascii_case("en")
                    })
            })
            .unwrap()
            .number;
        stream.set_track_filter(Some(vec![track]));
        let mut decoder = PgsDecoder::new(PgsLimits::default());
        let mut visible = 0;
        let mut packets = 0;
        let mut eof = false;
        while visible < 64 {
            let mut packet = Packet(Vec::new());
            let Some(frame) = stream.next_frame_into(&mut packet).unwrap() else {
                decoder.finish().unwrap();
                eof = true;
                break;
            };
            assert!(!frame.is_invisible && frame.is_discardable != Some(true));
            packets += 1;
            decoder
                .push_packet(frame.timestamp_ns, &packet.0, |display| {
                    visible += usize::from(display.image.is_some());
                })
                .unwrap();
        }
        println!(
            "track={track} packets={packets} visible={visible} eof={eof}; stopped suffix is not validated"
        );
        let after = std::fs::metadata(path).unwrap();
        assert_eq!(before.len(), after.len());
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    }
    println!(
        "files={} PGS scans succeeded; no OCR/downloads/renames",
        paths.len()
    );
}
#[test]
#[ignore = "opt-in structural-only local MKV metadata probe"]
fn inspect_local_video_mapping_tracks() {
    let Some(folder) = std::env::var_os("TVMATCH_TEST_MAPPING_FOLDER") else {
        return;
    };
    let mut paths = std::fs::read_dir(folder)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("mkv")))
        .collect::<Vec<_>>();
    assert!(paths.len() <= 32);
    paths.sort();
    for path in &paths {
        let before = std::fs::metadata(path).unwrap();
        let reader = std::fs::File::open(path).unwrap();
        let stream = open_streaming_with_limits(reader, StreamingLimits::default()).unwrap();
        let subtitles = stream
            .demuxer
            .tracks
            .iter()
            .filter(|t| t.kind == crate::TrackKind::Subtitle)
            .count();
        println!(
            "tracks={} subtitle_tracks={}",
            stream.demuxer.tracks.len(),
            subtitles
        );
        let after = std::fs::metadata(path).unwrap();
        assert_eq!(before.len(), after.len());
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    }
    println!(
        "files={} metadata probes succeeded; no subtitle payloads/OCR/downloads/renames",
        paths.len()
    );
}
