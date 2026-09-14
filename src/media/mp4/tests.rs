use super::*;
use media_isobmff::demux::MAX_MOOV;
use std::io::{self, Cursor, SeekFrom};
fn atom(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    b.extend(kind);
    b.extend(body);
    b
}
fn list(kind: &[u8; 4], fields: &[u32], rows: &[u8]) -> Vec<u8> {
    let mut b = vec![0; 4];
    for n in fields {
        b.extend(n.to_be_bytes());
    }
    b.extend(rows);
    atom(kind, &b)
}
fn packet(text: &[u8]) -> Vec<u8> {
    let mut b = (text.len() as u16).to_be_bytes().to_vec();
    b.extend(text);
    b
}
#[derive(Default)]
struct Options {
    wide: bool,
    fixed_size: bool,
    front: bool,
    forced: bool,
    external: bool,
    ctts: bool,
    elng: Option<&'static str>,
    edits: Vec<(u32, i32)>,
    tracks: usize,
    padding: usize,
}
fn movie(packets: &[Vec<u8>], o: Options) -> Vec<u8> {
    let ftyp = atom(b"ftyp", b"isom\0\0\0\0isommp42");
    let mut payload = vec![0; o.padding];
    for p in packets {
        payload.extend(p);
    }
    let make_moov = |offset: u64| {
        let duration = packets.len() as u32 * 1000;
        let mut mvhd = vec![0; 100];
        mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
        mvhd[16..20].copy_from_slice(&duration.to_be_bytes());
        let mut moov = atom(b"mvhd", &mvhd);
        for track in 0..o.tracks.max(1) {
            let mut tkhd = vec![0; 84];
            tkhd[3] = 3;
            tkhd[12..16].copy_from_slice(&(track as u32 + 1).to_be_bytes());
            let mut trak = atom(b"tkhd", &tkhd);
            if !o.edits.is_empty() {
                let mut edits = Vec::new();
                for (d, t) in &o.edits {
                    edits.extend(d.to_be_bytes());
                    edits.extend(t.to_be_bytes());
                    edits.extend(0x10000u32.to_be_bytes());
                }
                trak.extend(atom(
                    b"edts",
                    &list(b"elst", &[o.edits.len() as u32], &edits),
                ));
            }
            let mut mdhd = vec![0; 24];
            mdhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
            mdhd[16..20].copy_from_slice(&duration.to_be_bytes());
            let language = if track == 0 {
                [5u16, 14, 7]
            } else {
                [6, 18, 1]
            };
            let lang = language[0] << 10 | language[1] << 5 | language[2];
            mdhd[20..22].copy_from_slice(&lang.to_be_bytes());
            let mut mdia = atom(b"mdhd", &mdhd);
            let mut hdlr = vec![0; 24];
            hdlr[8..12].copy_from_slice(b"sbtl");
            mdia.extend(atom(b"hdlr", &hdlr));
            if let Some(s) = o.elng {
                let mut b = vec![0; 4];
                b.extend(s.as_bytes());
                b.push(0);
                mdia.extend(atom(b"elng", &b));
            }
            let mut desc = vec![0; 38];
            desc[7] = 1;
            if o.forced {
                desc[8..12].copy_from_slice(&0xc0000000u32.to_be_bytes());
            }
            let mut stbl = list(b"stsd", &[1], &atom(b"tx3g", &desc));
            let mut stts = Vec::new();
            if !packets.is_empty() {
                stts.extend((packets.len() as u32).to_be_bytes());
                stts.extend(1000u32.to_be_bytes());
            }
            stbl.extend(list(b"stts", &[u32::from(!packets.is_empty())], &stts));
            let mut stsc = Vec::new();
            if !packets.is_empty() {
                for n in [1, packets.len() as u32, 1] {
                    stsc.extend(n.to_be_bytes());
                }
            }
            stbl.extend(list(b"stsc", &[u32::from(!packets.is_empty())], &stsc));
            let sizes = packets
                .iter()
                .flat_map(|p| (p.len() as u32).to_be_bytes())
                .collect::<Vec<_>>();
            let constant = if o.fixed_size {
                let n = packets.first().map_or(0, Vec::len);
                assert!(packets.iter().all(|p| p.len() == n));
                n as u32
            } else {
                0
            };
            stbl.extend(list(
                b"stsz",
                &[constant, packets.len() as u32],
                if o.fixed_size { &[] } else { &sizes },
            ));
            let offsets = if packets.is_empty() {
                vec![]
            } else if o.wide {
                offset.to_be_bytes().to_vec()
            } else {
                (offset as u32).to_be_bytes().to_vec()
            };
            stbl.extend(list(
                if o.wide { b"co64" } else { b"stco" },
                &[u32::from(!packets.is_empty())],
                &offsets,
            ));
            if o.ctts {
                stbl.extend(list(b"ctts", &[0], &[]));
            }
            let url = atom(
                b"url ",
                if o.external {
                    &[0, 0, 0, 0]
                } else {
                    &[0, 0, 0, 1]
                },
            );
            let mut minf = atom(b"dinf", &list(b"dref", &[1], &url));
            minf.extend(atom(b"stbl", &stbl));
            mdia.extend(atom(b"minf", &minf));
            trak.extend(atom(b"mdia", &mdia));
            moov.extend(atom(b"trak", &trak));
        }
        atom(b"moov", &moov)
    };
    let initial = make_moov(0);
    let offset = ftyp.len() + 8 + o.padding + if o.front { initial.len() } else { 0 };
    let moov = make_moov(offset as u64);
    let mut out = ftyp;
    if o.front {
        out.extend(moov.clone());
    }
    out.extend(atom(b"mdat", &payload));
    if !o.front {
        out.extend(moov);
    }
    out
}
fn location(b: &[u8], kind: &[u8; 4]) -> usize {
    b.windows(4).position(|w| w == kind).unwrap() + 4
}
#[test]
fn mp4_auto_dispatch_text_unicode_empty_samples_styles_and_both_moov_positions() {
    let mut utf16 = vec![0xfe, 0xff];
    for c in "Unicode café 日本".encode_utf16() {
        utf16.extend(c.to_be_bytes());
    }
    let mut styled = packet(b"Last\r\ncaption.");
    styled.extend(atom(b"styl", &[0, 0]));
    styled.extend(atom(b"tbox", &[0; 8]));
    let packets = vec![
        packet(b""),
        packet(b"First original caption."),
        packet(&utf16),
        styled,
    ];
    for front in [false, true] {
        for wide in [false, true] {
            let bytes = movie(
                &packets,
                Options {
                    front,
                    wide,
                    ..Default::default()
                },
            );
            let tracks = crate::media::probe_subtitle_tracks(Cursor::new(&bytes)).unwrap();
            assert_eq!(tracks.len(), 1);
            assert!(tracks[0].supported);
            assert_eq!(tracks[0].language.as_deref(), Some("eng"));
            assert_eq!(tracks[0].codec_id, "tx3g");
            let output = crate::media::extract_subtitles(Cursor::new(&bytes), None).unwrap();
            assert_eq!(
                output
                    .transcript
                    .cues()
                    .iter()
                    .map(|c| (c.start_ms, c.end_ms, c.text.as_str()))
                    .collect::<Vec<_>>(),
                [
                    (1000, 2000, "First original caption."),
                    (2000, 3000, "Unicode café 日本"),
                    (3000, 4000, "Last\ncaption.")
                ]
            );
            assert_eq!(output.timestamps[0].block_duration_ns, Some(1_000_000_000));
            assert!(!output.timestamps[0].transcript_end_synthetic);
        }
    }
    let fixed = movie(
        &[packet(b"One"), packet(b"Two")],
        Options {
            fixed_size: true,
            ..Default::default()
        },
    );
    assert_eq!(
        extract(Cursor::new(fixed), None)
            .unwrap()
            .transcript
            .cues()
            .len(),
        2
    );
    let little = packet(&[0xff, 0xfe, b'A', 0]);
    assert_eq!(text(&little, 1, &mut BoxBudget::default()).unwrap(), "A");
}
#[test]
fn edit_lists_shift_and_clip_exact_declared_times_without_float_rounding() {
    let bytes = movie(
        &[packet(b"First"), packet(b"Second"), packet(b"Third")],
        Options {
            edits: vec![(500, -1), (1500, 500)],
            ..Default::default()
        },
    );
    let out = extract(Cursor::new(bytes), None).unwrap();
    assert_eq!(
        out.transcript
            .cues()
            .iter()
            .map(|c| (c.start_ms, c.end_ms, c.text.as_str()))
            .collect::<Vec<_>>(),
        [(500, 1000, "First"), (1000, 2000, "Second")]
    );
    let mut b = movie(
        &[packet(b"First")],
        Options {
            edits: vec![(1000, 0)],
            ..Default::default()
        },
    );
    let p = location(&b, b"elst");
    b[p + 16..p + 20].copy_from_slice(&0x20000u32.to_be_bytes());
    assert!(!probe(Cursor::new(&b)).unwrap()[0].supported);
    assert!(extract(Cursor::new(&b), None).is_err());
}
#[test]
fn metadata_languages_multiple_tracks_unsupported_forced_and_external_references() {
    let packets = [packet(b"Original")];
    let b = movie(
        &packets,
        Options {
            tracks: 2,
            ..Default::default()
        },
    );
    let t = probe(Cursor::new(&b)).unwrap();
    assert_eq!(t[1].language.as_deref(), Some("fra"));
    assert!(matches!(
        extract(Cursor::new(&b), None),
        Err(MediaError::AmbiguousSubtitleTracks(_))
    ));
    assert!(extract(Cursor::new(&b), Some(2)).is_ok());
    assert!(matches!(
        extract(Cursor::new(&b), Some(3)),
        Err(MediaError::TrackNotFound(3))
    ));
    for o in [
        Options {
            forced: true,
            ..Default::default()
        },
        Options {
            external: true,
            ..Default::default()
        },
        Options {
            ctts: true,
            ..Default::default()
        },
    ] {
        let b = movie(&packets, o);
        assert!(extract(Cursor::new(b), None).is_err());
    }
    let b = movie(
        &packets,
        Options {
            elng: Some("en-GB"),
            ..Default::default()
        },
    );
    assert_eq!(
        probe(Cursor::new(b)).unwrap()[0].language.as_deref(),
        Some("en-GB")
    );
    let mut b = movie(&packets, Options::default());
    let p = location(&b, b"tx3g");
    b[p - 4..p].copy_from_slice(b"wvtt");
    assert!(!probe(Cursor::new(&b)).unwrap()[0].supported);
    assert!(extract(Cursor::new(b), None).is_err());
}
#[test]
fn malformed_late_samples_and_suffixes_never_publish_a_valid_prefix() {
    for bad_sample in [
        vec![0, 8, b'x'],
        packet(&[0xff]),
        packet(&[0xfe, 0xff, 0]),
        packet(&[0xff, 0xfe, 0, 0xd8]),
        packet(b"bad\0caption"),
        {
            let mut b = packet(b"caption");
            b.extend([0, 0, 0, 7, b's', b't', b'y', b'l']);
            b
        },
        {
            let mut b = packet(b"caption");
            b.extend(atom(b"frcd", &[]));
            b
        },
    ] {
        let b = movie(
            &[packet(b"Valid earlier caption"), bad_sample],
            Options::default(),
        );
        assert!(extract(Cursor::new(b), None).is_err());
    }
    for tail in [vec![1, 2, 3], atom(b"moof", &[])] {
        let mut b = movie(&[packet(b"Valid")], Options::default());
        b.extend(tail);
        assert!(extract(Cursor::new(b), None).is_err());
    }
}
#[test]
fn sample_table_counts_ranges_duplicates_and_work_caps_fail_closed() {
    let source = movie(&[packet(b"One"), packet(b"Two")], Options::default());
    for (kind, offset, value) in [
        (*b"stco", 8, 0u32),
        (*b"stsz", 8, 100_001),
        (*b"stts", 8, 100_001),
        (*b"stsc", 8, 0),
        (*b"stsc", 12, 3),
        (*b"stsc", 16, 2),
        (*b"stsz", 12, 65537),
        (*b"stts", 12, 999),
    ] {
        let mut b = source.clone();
        let p = location(&b, &kind);
        b[p + offset..p + offset + 4].copy_from_slice(&value.to_be_bytes());
        assert!(extract(Cursor::new(b), None).is_err());
    }
    let mut b = source.clone();
    let p = location(&b, b"stco");
    b[p - 4..p].copy_from_slice(b"stsz");
    assert!(extract(Cursor::new(b), None).is_err());
    assert!(extract(Cursor::new(movie(&[], Options::default())), None).is_err());
    let b = movie(&vec![packet(b"x"); MAX_CUES + 1], Options::default());
    assert!(matches!(
        extract(Cursor::new(b), None),
        Err(MediaError::LimitExceeded("MP4 cues"))
    ));
    let mut huge = atom(b"ftyp", b"isom\0\0\0\0isom");
    huge.extend(((MAX_MOOV + 9) as u32).to_be_bytes());
    huge.extend(b"moov");
    huge.resize(huge.len() + MAX_MOOV + 1, 0);
    assert!(probe(Cursor::new(huge)).is_err());
}
#[test]
fn extracted_mp4_text_identifies_with_the_unchanged_independent_evidence_engine() {
    let dialogue = [
        "The copper telescope is growing tiny paper feathers",
        "Please keep those moonlight jars beneath the staircase",
        "Our patient comet has finally learned to whistle",
    ];
    let mut packets = vec![packet(b""); 19];
    for (i, line) in dialogue.iter().enumerate() {
        packets[i * 9] = packet(line.as_bytes());
    }
    let query = extract(Cursor::new(movie(&packets, Options::default())), None)
        .unwrap()
        .transcript;
    let reference = Transcript::from_cues(
        dialogue
            .iter()
            .enumerate()
            .map(|(i, line)| Cue {
                start_ms: 1000 + i as u64 * 9000,
                end_ms: 2000 + i as u64 * 9000,
                text: line.to_string(),
            })
            .collect(),
    )
    .unwrap();
    let r = crate::Reference::new(
        crate::ReferenceId::new("synthetic", "1").unwrap(),
        "S01E01 Original episode",
        "original synthetic fixture",
        reference,
    )
    .unwrap();
    let index = crate::Index::build(vec![r]).unwrap();
    assert!(matches!(
        index.match_query(&query).unwrap(),
        crate::MatchOutcome::Identified { .. }
    ));
}
#[test]
fn video_payload_is_seek_skipped_not_loaded() {
    struct Guard {
        inner: Cursor<Vec<u8>>,
        start: u64,
        end: u64,
        read: usize,
    }
    impl Read for Guard {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            let p = self.inner.position();
            if p < self.end && p + b.len() as u64 > self.start {
                return Err(io::Error::other("video payload was read"));
            }
            let n = self.inner.read(b)?;
            self.read += n;
            Ok(n)
        }
    }
    impl Seek for Guard {
        fn seek(&mut self, p: SeekFrom) -> io::Result<u64> {
            self.inner.seek(p)
        }
    }
    let padding = 2 * 1024 * 1024;
    let b = movie(
        &[packet(b"Only subtitle payload")],
        Options {
            padding,
            ..Default::default()
        },
    );
    let start = location(&b, b"mdat") as u64;
    let mut g = Guard {
        inner: Cursor::new(b),
        start,
        end: start + padding as u64,
        read: 0,
    };
    let out = extract(&mut g, None).unwrap();
    assert_eq!(out.transcript.cues().len(), 1);
    assert!(g.read < 4096);
}
