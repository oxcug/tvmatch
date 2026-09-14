use super::*;
use std::io::{self, Cursor};
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
struct Fixture {
    handler: [u8; 4],
    format: [u8; 4],
    data: Vec<Vec<u8>>,
    chunks: Vec<u32>,
    external: bool,
    ctts: bool,
    edit: Vec<(u32, i32)>,
}
impl Fixture {
    fn new(handler: [u8; 4], format: [u8; 4], data: &[&[u8]]) -> Self {
        Self {
            handler,
            format,
            data: data.iter().map(|d| d.to_vec()).collect(),
            chunks: vec![data.len() as u32],
            external: false,
            ctts: false,
            edit: vec![],
        }
    }
}
fn movie(tracks: &[Fixture], front: bool, wide: bool, padding: usize) -> Vec<u8> {
    let ftyp = atom(b"ftyp", b"isom\0\0\0\0isommp42");
    let mut payload = vec![0; padding];
    let mut relative = Vec::new();
    for t in tracks {
        relative.push(payload.len());
        for p in &t.data {
            payload.extend(p);
        }
    }
    let build = |base: usize| {
        let mut mvhd = vec![0; 100];
        mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
        let mut moov = atom(b"mvhd", &mvhd);
        for (i, t) in tracks.iter().enumerate() {
            let mut tkhd = vec![0; 84];
            tkhd[3] = 3;
            tkhd[12..16].copy_from_slice(&(i as u32 + 1).to_be_bytes());
            let mut trak = atom(b"tkhd", &tkhd);
            if !t.edit.is_empty() {
                let mut rows = Vec::new();
                for (d, time) in &t.edit {
                    rows.extend(d.to_be_bytes());
                    rows.extend(time.to_be_bytes());
                    rows.extend(0x10000u32.to_be_bytes());
                }
                trak.extend(atom(b"edts", &list(b"elst", &[t.edit.len() as u32], &rows)));
            }
            let mut mdhd = vec![0; 24];
            mdhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
            mdhd[16..20].copy_from_slice(&(t.data.len() as u32 * 1000).to_be_bytes());
            mdhd[20..22].copy_from_slice(&(5u16 << 10 | 14 << 5 | 7).to_be_bytes());
            let mut mdia = atom(b"mdhd", &mdhd);
            let mut handler = vec![0; 24];
            handler[8..12].copy_from_slice(&t.handler);
            mdia.extend(atom(b"hdlr", &handler));
            // Only the eight-byte SampleEntry header is container-owned. This
            // deliberately nonstandard codec config must remain uninterpreted.
            let mut desc = vec![0; 8];
            desc[7] = 1;
            desc.extend([0xff, 0xab, 0xfe]);
            let mut stbl = list(b"stsd", &[1], &atom(&t.format, &desc));
            let n = t.data.len() as u32;
            let rows = [n.to_be_bytes(), 1000u32.to_be_bytes()].concat();
            stbl.extend(list(b"stts", &[1], &rows));
            let rows = t
                .chunks
                .iter()
                .enumerate()
                .flat_map(|(i, n)| {
                    [
                        (i as u32 + 1).to_be_bytes(),
                        n.to_be_bytes(),
                        1u32.to_be_bytes(),
                    ]
                    .concat()
                })
                .collect::<Vec<_>>();
            stbl.extend(list(b"stsc", &[t.chunks.len() as u32], &rows));
            let sizes = t
                .data
                .iter()
                .flat_map(|d| (d.len() as u32).to_be_bytes())
                .collect::<Vec<_>>();
            stbl.extend(list(b"stsz", &[0, n], &sizes));
            let mut offset = (base + relative[i]) as u64;
            let mut bytes = Vec::new();
            let mut sample = 0;
            for n in &t.chunks {
                bytes.extend(if wide {
                    offset.to_be_bytes().to_vec()
                } else {
                    (offset as u32).to_be_bytes().to_vec()
                });
                for _ in 0..*n {
                    offset += t.data[sample].len() as u64;
                    sample += 1;
                }
            }
            assert_eq!(sample, t.data.len());
            stbl.extend(list(
                if wide { b"co64" } else { b"stco" },
                &[t.chunks.len() as u32],
                &bytes,
            ));
            if t.ctts {
                stbl.extend(list(b"ctts", &[0], &[]));
            }
            let reference = atom(
                b"url ",
                if t.external {
                    &[0, 0, 0, 0]
                } else {
                    &[0, 0, 0, 1]
                },
            );
            let mut minf = atom(b"dinf", &list(b"dref", &[1], &reference));
            minf.extend(atom(b"stbl", &stbl));
            mdia.extend(atom(b"minf", &minf));
            trak.extend(atom(b"mdia", &mdia));
            moov.extend(atom(b"trak", &trak));
        }
        atom(b"moov", &moov)
    };
    let base = ftyp.len() + 8 + if front { build(0).len() } else { 0 };
    let moov = build(base);
    let mut b = ftyp;
    if front {
        b.extend(&moov);
    }
    b.extend(atom(b"mdat", &payload));
    if !front {
        b.extend(moov);
    }
    b
}
fn location(b: &[u8], k: &[u8; 4]) -> usize {
    b.windows(4).position(|s| s == k).unwrap() + 4
}
#[test]
fn generic_video_audio_and_unknown_codecs_are_opaque_and_selectable() {
    for front in [false, true] {
        for wide in [false, true] {
            let fixture = [
                Fixture::new(*b"vide", *b"avc1", &[&[0xff, 0, 0x80]]),
                Fixture::new(*b"soun", *b"mp4a", &[b"audio one", b"audio two"]),
                Fixture::new(*b"meta", *b"zzzz", &[&[1, 2, 3, 4], b""]),
            ];
            let b = movie(&fixture, front, wide, 0);
            let d = Demuxer::open(Cursor::new(&b)).unwrap();
            assert_eq!(d.tracks().len(), 3);
            assert_eq!(d.tracks()[0].handler, *b"vide");
            assert_eq!(
                d.tracks()[0].descriptions[0].configuration,
                [0xff, 0xab, 0xfe]
            );
            assert_eq!(d.tracks()[1].language.as_deref(), Some("eng"));
            for id in 1..=3 {
                let mut s = Demuxer::open(Cursor::new(&b))
                    .unwrap()
                    .into_samples(id)
                    .unwrap();
                assert_eq!(s.track().id, id);
                for (i, expected) in fixture[id as usize - 1].data.iter().enumerate() {
                    let p = s.next_sample().unwrap().unwrap();
                    assert_eq!(&p.data, expected);
                    assert_eq!(p.decode_time, i as u64 * 1000);
                    assert_eq!(
                        p.presentation_ns,
                        Some((i as u64 * 1_000_000_000, (i as u64 + 1) * 1_000_000_000))
                    );
                }
                assert!(s.next_sample().unwrap().is_none());
            }
        }
    }
}
#[test]
fn edits_clip_movie_timeline_but_still_return_excluded_payloads() {
    let mut t = Fixture::new(*b"soun", *b"mp4a", &[b"before", b"visible", b"after"]);
    t.edit = vec![(500, -1), (1000, 1000)];
    let mut s = Demuxer::open(Cursor::new(movie(&[t], false, false, 0)))
        .unwrap()
        .into_samples(1)
        .unwrap();
    let before = s.next_sample().unwrap().unwrap();
    assert_eq!(before.data, b"before");
    assert_eq!(before.presentation_ns, None);
    assert_eq!(
        s.next_sample().unwrap().unwrap().presentation_ns,
        Some((500_000_000, 1_500_000_000))
    );
    assert_eq!(s.next_sample().unwrap().unwrap().presentation_ns, None);
    assert_eq!(
        edited_times(2, 1, 3, 2, (0, 1, 10)).unwrap(),
        Some((333_333_333, 666_666_666))
    );
}
#[test]
fn unsupported_container_constructs_do_not_poison_other_tracks() {
    for external in [false, true] {
        let mut bad = Fixture::new(*b"vide", *b"avc1", &[b"video"]);
        bad.external = external;
        bad.ctts = !external;
        let b = movie(
            &[bad, Fixture::new(*b"soun", *b"mp4a", &[b"audio"])],
            false,
            false,
            0,
        );
        let d = Demuxer::open(Cursor::new(&b)).unwrap();
        assert!(d.tracks()[0].unsupported_reason.is_some());
        assert!(d.into_samples(1).is_err());
        assert!(
            Demuxer::open(Cursor::new(&b))
                .unwrap()
                .into_samples(2)
                .unwrap()
                .next_sample()
                .unwrap()
                .is_some()
        );
    }
    let mut b = movie(
        &[Fixture::new(*b"soun", *b"raw ", &[b"x"])],
        false,
        false,
        0,
    );
    b.extend(atom(b"moof", &[]));
    assert!(Demuxer::open(Cursor::new(b)).is_err());
}
#[test]
fn counts_extents_and_duplicate_tables_are_validated_before_payloads() {
    // Vary samples/chunk, then corrupt a chunk to overlap or overflow. These
    // failures must occur at selection, before any sample bytes can be returned.
    for wide in [false, true] {
        let mut t = Fixture::new(*b"soun", *b"raw ", &[b"one", b"two", b"three"]);
        t.chunks = vec![1, 2];
        let b = movie(&[t], false, wide, 0);
        let mut s = Demuxer::open(Cursor::new(&b))
            .unwrap()
            .into_samples(1)
            .unwrap();
        for expected in [b"one".as_slice(), b"two", b"three"] {
            assert_eq!(s.next_sample().unwrap().unwrap().data, expected);
        }
        for offset in [
            location(&b, b"mdat") as u64 + 2,
            if wide { u64::MAX } else { u32::MAX as u64 },
        ] {
            let mut corrupt = b.clone();
            let p = location(&corrupt, if wide { b"co64" } else { b"stco" })
                + 8
                + if wide { 8 } else { 4 };
            let bytes = if wide {
                offset.to_be_bytes().to_vec()
            } else {
                (offset as u32).to_be_bytes().to_vec()
            };
            corrupt[p..p + bytes.len()].copy_from_slice(&bytes);
            assert!(
                Demuxer::open(Cursor::new(corrupt))
                    .unwrap()
                    .into_samples(1)
                    .is_err()
            );
        }
    }
    let source = movie(
        &[Fixture::new(*b"vide", *b"raw ", &[b"one", b"two"])],
        false,
        false,
        0,
    );
    for (kind, offset, value) in [
        (*b"stco", 8, 0u32),
        (*b"stsz", 8, 100_001),
        (*b"stts", 8, 100_001),
        (*b"stsc", 8, 0),
        (*b"stsc", 12, 3),
        (*b"stsc", 16, 2),
        (*b"stts", 12, 999),
    ] {
        let mut b = source.clone();
        let p = location(&b, &kind);
        b[p + offset..p + offset + 4].copy_from_slice(&value.to_be_bytes());
        assert!(
            Demuxer::open(Cursor::new(b))
                .unwrap()
                .into_samples(1)
                .is_err()
        );
    }
    let mut b = source.clone();
    let p = location(&b, b"stco");
    b[p - 4..p].copy_from_slice(b"stsz");
    assert!(
        Demuxer::open(Cursor::new(b))
            .unwrap()
            .into_samples(1)
            .is_err()
    );
    let mut b = source;
    b.extend([1, 2, 3]);
    assert!(Demuxer::open(Cursor::new(b)).is_err());
}
#[test]
fn selected_sample_read_eof_and_seek_failures_are_terminal_not_successful_prefixes() {
    use std::{cell::Cell, rc::Rc};
    struct FaultReader {
        inner: Cursor<Vec<u8>>,
        armed: Rc<Cell<bool>>,
        calls: Rc<Cell<usize>>,
        mode: u8,
    }
    impl Read for FaultReader {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.calls.set(self.calls.get() + 1);
            if self.armed.get() {
                match self.mode {
                    0 => return Err(io::Error::other("synthetic read failure")),
                    1 => return Ok(0),
                    _ => {}
                }
            }
            self.inner.read(out)
        }
    }
    impl Seek for FaultReader {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.calls.set(self.calls.get() + 1);
            if self.armed.get() && self.mode == 2 {
                return Err(io::Error::other("synthetic seek failure"));
            }
            self.inner.seek(position)
        }
    }
    for front in [false, true] {
        for mode in 0..3 {
            let armed = Rc::new(Cell::new(false));
            let calls = Rc::new(Cell::new(0));
            let bytes = movie(
                &[Fixture::new(*b"text", *b"tx3g", &[b"first", b"second"])],
                front,
                false,
                0,
            );
            let reader = FaultReader {
                inner: Cursor::new(bytes),
                armed: armed.clone(),
                calls: calls.clone(),
                mode,
            };
            let mut samples = Demuxer::open(reader).unwrap().into_samples(1).unwrap();
            assert_eq!(samples.next_sample().unwrap().unwrap().data, b"first");
            armed.set(true);
            assert!(samples.next_sample().is_err());
            armed.set(false);
            let after = calls.get();
            assert!(samples.next_sample().is_err());
            assert!(samples.next_sample().is_err());
            assert_eq!(
                calls.get(),
                after,
                "poisoned reader must not retry I/O or publish a prefix"
            );
        }
    }
}
#[test]
fn payload_budgets_are_configurable_cumulative_and_failure_is_terminal() {
    let b = movie(
        &[Fixture::new(*b"vide", *b"raw ", &[b"first", b"second"])],
        false,
        false,
        0,
    );
    let metadata = Demuxer::open(Cursor::new(&b)).unwrap().bytes_read();
    let mut s = Demuxer::with_limits(
        Cursor::new(&b),
        Limits {
            read_bytes: metadata + 5,
            sample_bytes: 8,
        },
    )
    .unwrap()
    .into_samples(1)
    .unwrap();
    assert_eq!(s.next_sample().unwrap().unwrap().data, b"first");
    assert!(s.next_sample().is_err());
    assert!(s.next_sample().is_err());
    assert_eq!(s.bytes_read(), metadata + 5);
    assert!(
        Demuxer::with_limits(
            Cursor::new(&b),
            Limits {
                read_bytes: 1_000_000,
                sample_bytes: 4
            }
        )
        .unwrap()
        .into_samples(1)
        .is_err()
    );
    assert!(
        Demuxer::with_limits(
            Cursor::new(&b),
            Limits {
                read_bytes: 0,
                sample_bytes: 4
            }
        )
        .is_err()
    );
}
#[test]
fn box_versions_extended_sizes_and_global_budgets_are_bounded() {
    for movie in [false, true] {
        let mut h = vec![0; if movie { 112 } else { 36 }];
        h[0] = 1;
        h[20..24].copy_from_slice(&1000u32.to_be_bytes());
        h[24..32].copy_from_slice(&9000u64.to_be_bytes());
        assert_eq!(header_time(&h, movie).unwrap(), (1000, 9000, 32));
        h[0] = 2;
        assert!(header_time(&h, movie).is_err());
    }
    let mut e = vec![1, 0, 0, 0];
    e.extend(1u32.to_be_bytes());
    e.extend(1000u64.to_be_bytes());
    e.extend(0i64.to_be_bytes());
    e.extend(0x10000u32.to_be_bytes());
    assert_eq!(
        edit_list(Some(&atom(b"elst", &e)), &mut 0).unwrap(),
        Some((0, 0, 1000))
    );
    let mut b = 1u32.to_be_bytes().to_vec();
    b.extend(b"free");
    b.extend(20u64.to_be_bytes());
    b.extend([0; 4]);
    assert_eq!(BoxBudget::default().read(&b).unwrap()[0].data.len(), 4);
    b[8..16].copy_from_slice(&15u64.to_be_bytes());
    assert!(BoxBudget::default().read(&b).is_err());
    let mut budget = BoxBudget { used: MAX_BOXES };
    assert!(budget.read(&atom(b"free", &[])).is_err());
}
#[test]
fn unselected_payload_is_seek_skipped() {
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
                return Err(io::Error::other("unselected payload was read"));
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
    let b = movie(
        &[
            Fixture::new(*b"vide", *b"raw ", &[&vec![0; 2 * 1024 * 1024]]),
            Fixture::new(*b"soun", *b"raw ", &[b"chosen"]),
        ],
        false,
        false,
        0,
    );
    let start = location(&b, b"mdat") as u64;
    let mut g = Guard {
        inner: Cursor::new(b),
        start,
        end: start + 2 * 1024 * 1024,
        read: 0,
    };
    let mut s = Demuxer::open(&mut g).unwrap().into_samples(2).unwrap();
    assert_eq!(s.next_sample().unwrap().unwrap().data, b"chosen");
    assert!(s.next_sample().unwrap().is_none());
    drop(s);
    assert!(g.read < 4096);
}
