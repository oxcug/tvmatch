//! Cross-parse oracle: build synthetic `.webm` / `.mkv` byte buffers
//! and confirm that our `Demuxer` and the third-party
//! `matroska-demuxer` crate agree on the EBML header, segment info,
//! and track table.
//!
//! Feed both independent implementations the same bytes and compare
//! structured output. Synthetic fixtures only: the goal is parser parity,
//! not real-world decode.

use std::io::Cursor;

use media_mkv_webm::ebml::schema::codec_id;
use media_mkv_webm::{Demuxer, DocType, Frames, Muxer, TrackDescriptor, TrackKind};

// === EBML element builders =================================================
//
// Each helper emits a single element with a known-length size VINT
// short enough to fit in one byte (payload < 128). That's all the
// synthetic fixtures need; bigger payloads belong in real-fixture tests.

/// Write a size VINT wide enough to encode `n`. RFC 8794 §4.4 — the
/// payload width must be a multiple of 7 bits, so we pick the
/// smallest VINT width whose payload bits cover `n`.
fn write_size_vint(n: usize) -> Vec<u8> {
    let n = n as u64;
    // Find width w in 1..=8 such that n < 2^(7*w) - 1 (the all-ones
    // value at each width is reserved for "unknown size", so we leave
    // it free).
    for w in 1..=8u8 {
        let bits = 7 * w as u32;
        let limit = if bits == 64 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        };
        if n < limit {
            let mut buf = vec![0u8; w as usize];
            // Big-endian payload bytes.
            let mut x = n;
            for i in (0..w as usize).rev() {
                buf[i] = (x & 0xFF) as u8;
                x >>= 8;
            }
            // Set the width marker bit in the first byte.
            buf[0] |= 1 << (8 - w);
            return buf;
        }
    }
    panic!("size {n} too large for any VINT width");
}

fn elem(id_bytes: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut v = id_bytes.to_vec();
    v.extend_from_slice(&write_size_vint(payload.len()));
    v.extend_from_slice(payload);
    v
}

fn elem1(id: u8, payload: &[u8]) -> Vec<u8> {
    elem(&[id], payload)
}
fn elem_id2(id: u16, payload: &[u8]) -> Vec<u8> {
    elem(&id.to_be_bytes(), payload)
}
fn elem_id3(id_bytes: [u8; 3], payload: &[u8]) -> Vec<u8> {
    elem(&id_bytes, payload)
}
fn elem_id4(id: u32, payload: &[u8]) -> Vec<u8> {
    elem(&id.to_be_bytes(), payload)
}

fn uint_payload(v: u64) -> Vec<u8> {
    if v == 0 {
        return vec![0];
    }
    let mut buf = Vec::new();
    let mut started = false;
    for shift in (0..8).rev() {
        let b = ((v >> (shift * 8)) & 0xFF) as u8;
        if started || b != 0 {
            buf.push(b);
            started = true;
        }
    }
    buf
}

/// Build an EBML header with a given DocType and DocTypeVersion=4
/// (matches the version matroska-demuxer claims to support).
fn ebml_header(doc_type: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&elem_id2(0x4286, &uint_payload(1))); // EBMLVersion
    body.extend_from_slice(&elem_id2(0x42F7, &uint_payload(1))); // EBMLReadVersion
    body.extend_from_slice(&elem_id2(0x42F2, &uint_payload(4))); // EBMLMaxIDLength
    body.extend_from_slice(&elem_id2(0x42F3, &uint_payload(8))); // EBMLMaxSizeLength
    body.extend_from_slice(&elem_id2(0x4282, doc_type.as_bytes())); // DocType
    body.extend_from_slice(&elem_id2(0x4287, &uint_payload(4))); // DocTypeVersion
    body.extend_from_slice(&elem_id2(0x4285, &uint_payload(2))); // DocTypeReadVersion
    elem_id4(0x1A45DFA3, &body)
}

fn info_block(timestamp_scale: u64) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&elem_id3(
        [0x2A, 0xD7, 0xB1],
        &uint_payload(timestamp_scale),
    ));
    body.extend_from_slice(&elem_id2(0x4D80, b"media-mkv-webm test")); // MuxingApp
    body.extend_from_slice(&elem_id2(0x5741, b"media-mkv-webm test")); // WritingApp
    elem_id4(0x1549A966, &body)
}

fn audio_track_opus() -> Vec<u8> {
    let mut audio_payload = Vec::new();
    audio_payload.extend_from_slice(&elem1(0xB5, &48000.0f32.to_be_bytes())); // SamplingFrequency
    audio_payload.extend_from_slice(&elem1(0x9F, &uint_payload(2))); // Channels
    let audio = elem1(0xE1, &audio_payload);

    let mut entry = Vec::new();
    entry.extend_from_slice(&elem1(0xD7, &uint_payload(1))); // TrackNumber
    entry.extend_from_slice(&elem_id2(0x73C5, &uint_payload(0xDEADBEEF))); // TrackUID
    entry.extend_from_slice(&elem1(0x83, &uint_payload(2))); // TrackType=audio
    entry.extend_from_slice(&elem1(0x86, b"A_OPUS")); // CodecID
    entry.extend_from_slice(&audio);
    elem1(0xAE, &entry)
}

/// SimpleBlock body: 1-byte VINT track + i16 BE timestamp delta + 1
/// flags byte + frame data. Wrapped as a 0xA3 element.
fn simple_block(track: u8, delta: i16, flags: u8, data: &[u8]) -> Vec<u8> {
    assert!(track < 0x80, "use a multi-byte VINT helper for track ≥ 128");
    let mut body = vec![0x80 | track];
    body.extend_from_slice(&delta.to_be_bytes());
    body.push(flags);
    body.extend_from_slice(data);
    elem1(0xA3, &body)
}

/// Cluster with a Timestamp child (segment ticks) and an arbitrary
/// list of pre-built block elements (SimpleBlock or BlockGroup).
fn cluster_with_blocks(cluster_ts: u64, blocks: &[Vec<u8>]) -> Vec<u8> {
    let mut body = elem1(0xE7, &uint_payload(cluster_ts));
    for b in blocks {
        body.extend_from_slice(b);
    }
    elem_id4(0x1F43B675, &body)
}

fn build_file(doc_type: &str, timestamp_scale: u64, track_entries: &[Vec<u8>]) -> Vec<u8> {
    build_file_with_cluster(
        doc_type,
        timestamp_scale,
        track_entries,
        &cluster_with_blocks(0, &[]),
    )
}

fn build_file_with_cluster(
    doc_type: &str,
    timestamp_scale: u64,
    track_entries: &[Vec<u8>],
    cluster: &[u8],
) -> Vec<u8> {
    let mut segment_payload = Vec::new();
    segment_payload.extend_from_slice(&info_block(timestamp_scale));
    let mut tracks_payload = Vec::new();
    for t in track_entries {
        tracks_payload.extend_from_slice(t);
    }
    if !track_entries.is_empty() {
        segment_payload.extend_from_slice(&elem_id4(0x1654AE6B, &tracks_payload));
    }
    segment_payload.extend_from_slice(cluster);
    let segment = elem_id4(0x18538067, &segment_payload);

    let mut file = ebml_header(doc_type);
    file.extend_from_slice(&segment);
    file
}

// === Diff helpers ==========================================================

fn parse_both(bytes: &[u8]) -> (Demuxer, matroska_demuxer::MatroskaFile<Cursor<&[u8]>>) {
    let ours = Demuxer::parse(bytes).expect("our parser");
    let theirs = matroska_demuxer::MatroskaFile::open(Cursor::new(bytes)).expect("oracle parser");
    (ours, theirs)
}

// === Tests =================================================================

#[test]
fn webm_doctype_and_info_agree() {
    // Oracle requires a Tracks block with ≥1 entry to be considered
    // well-formed; reuse the audio track for the doctype-and-info diff.
    let bytes = build_file("webm", 1_000_000, &[audio_track_opus()]);
    let (ours, theirs) = parse_both(&bytes);

    assert_eq!(ours.doc_type, DocType::Webm);
    assert_eq!(theirs.ebml_header().doc_type(), "webm");
    assert_eq!(ours.doc_type.as_str(), theirs.ebml_header().doc_type());

    assert_eq!(
        ours.timestamp_scale_ns,
        theirs.info().timestamp_scale().get()
    );

    assert_eq!(ours.muxing_app.as_deref(), Some(theirs.info().muxing_app()));
    assert_eq!(
        ours.writing_app.as_deref(),
        Some(theirs.info().writing_app())
    );
}

#[test]
fn mkv_opus_track_agree() {
    let bytes = build_file("matroska", 1_000_000, &[audio_track_opus()]);
    let (ours, theirs) = parse_both(&bytes);

    assert_eq!(ours.doc_type.as_str(), theirs.ebml_header().doc_type());
    assert_eq!(ours.tracks.len(), theirs.tracks().len());
    assert_eq!(ours.tracks.len(), 1);

    let ours_t = &ours.tracks[0];
    let theirs_t = &theirs.tracks()[0];

    assert_eq!(ours_t.number, theirs_t.track_number().get());
    assert_eq!(ours_t.uid, theirs_t.track_uid().get());
    assert_eq!(ours_t.kind, TrackKind::Audio);
    assert_eq!(theirs_t.track_type(), matroska_demuxer::TrackType::Audio);
    assert_eq!(ours_t.codec_id.as_str(), theirs_t.codec_id());

    let ours_a = ours_t.audio.as_ref().unwrap();
    let theirs_a = theirs_t.audio().unwrap();
    assert!((ours_a.sampling_frequency - theirs_a.sampling_frequency()).abs() < 1e-3);
    assert_eq!(ours_a.channels as u64, theirs_a.channels().get());
}

#[test]
fn frames_match_oracle() {
    // Two SimpleBlocks on track 1 inside a Cluster at ts=10. The
    // second block has the keyframe bit clear and discardable bit set.
    let cluster = cluster_with_blocks(
        10,
        &[
            simple_block(1, 5, 0x80, b"frame-A"),
            simple_block(1, 25, 0x01, b"frame-B-longer"),
        ],
    );
    let bytes = build_file_with_cluster("matroska", 1_000_000, &[audio_track_opus()], &cluster);

    let ours = Demuxer::parse(&bytes).expect("our parser");
    let mut theirs = matroska_demuxer::MatroskaFile::open(Cursor::new(&bytes[..])).expect("oracle");

    let mut ours_iter = Frames::new(&bytes, &ours);
    let mut their_frame = matroska_demuxer::Frame::default();

    // Frame 1.
    let a = ours_iter.next_frame().unwrap().unwrap();
    let got = theirs.next_frame(&mut their_frame).unwrap();
    assert!(got);
    assert_eq!(a.track, their_frame.track);
    assert_eq!(a.data, &their_frame.data[..]);
    assert_eq!(a.is_keyframe, their_frame.is_keyframe);
    // Both parsers expose timestamps in their *native* unit: ours
    // promoted to ns, the oracle in segment ticks. Compare via the
    // shared timescale.
    assert_eq!(
        a.timestamp_ns / ours.timestamp_scale_ns,
        their_frame.timestamp
    );

    // Frame 2.
    let b = ours_iter.next_frame().unwrap().unwrap();
    let got = theirs.next_frame(&mut their_frame).unwrap();
    assert!(got);
    assert_eq!(b.track, their_frame.track);
    assert_eq!(b.data, &their_frame.data[..]);
    assert_eq!(b.is_keyframe, their_frame.is_keyframe);
    assert_eq!(b.is_discardable, their_frame.is_discardable);
    assert_eq!(
        b.timestamp_ns / ours.timestamp_scale_ns,
        their_frame.timestamp
    );

    // EOF.
    assert!(ours_iter.next_frame().unwrap().is_none());
    assert!(!theirs.next_frame(&mut their_frame).unwrap());
}

#[test]
fn muxer_output_decodes_in_oracle() {
    // Build a `.webm` from scratch with our Muxer and confirm the
    // third-party matroska-demuxer can read every track and frame.
    let mut m = Muxer::new(DocType::Webm);
    let opus_track = m
        .register_track(TrackDescriptor::audio(codec_id::A_OPUS, 48000.0, 2))
        .unwrap();
    let vp9_track = m
        .register_track(TrackDescriptor::video(codec_id::V_VP9, 1280, 720))
        .unwrap();
    m.append(opus_track, b"opus-pkt-1", 0, true).unwrap();
    m.append(vp9_track, b"vp9-keyframe", 0, true).unwrap();
    m.append(opus_track, b"opus-pkt-2", 20_000_000, true)
        .unwrap();
    m.append(vp9_track, b"vp9-p-frame", 33_000_000, false)
        .unwrap();
    let bytes = m.finalize().unwrap();

    let mut oracle = matroska_demuxer::MatroskaFile::open(Cursor::new(&bytes[..]))
        .expect("oracle must parse our mux output");
    assert_eq!(oracle.ebml_header().doc_type(), "webm");
    assert_eq!(oracle.tracks().len(), 2);

    // Pull everything we need out of `oracle.tracks()` before kicking off
    // `next_frame`, which borrows the oracle mutably.
    let (opus_track_num, vp9_track_num) = {
        let opus = oracle
            .tracks()
            .iter()
            .find(|t| t.codec_id() == "A_OPUS")
            .unwrap();
        let vp9 = oracle
            .tracks()
            .iter()
            .find(|t| t.codec_id() == "V_VP9")
            .unwrap();
        assert_eq!(opus.track_type(), matroska_demuxer::TrackType::Audio);
        assert_eq!(vp9.track_type(), matroska_demuxer::TrackType::Video);
        assert_eq!(opus.audio().unwrap().sampling_frequency() as u32, 48000);
        assert_eq!(opus.audio().unwrap().channels().get(), 2);
        assert_eq!(vp9.video().unwrap().pixel_width().get(), 1280);
        assert_eq!(vp9.video().unwrap().pixel_height().get(), 720);
        (opus.track_number().get(), vp9.track_number().get())
    };

    let mut frame = matroska_demuxer::Frame::default();
    let mut seen: Vec<(u64, Vec<u8>)> = Vec::new();
    while oracle.next_frame(&mut frame).unwrap() {
        seen.push((frame.track, frame.data.clone()));
    }
    assert!(
        seen.iter()
            .any(|(t, d)| *t == opus_track_num && d == b"opus-pkt-1")
    );
    assert!(
        seen.iter()
            .any(|(t, d)| *t == vp9_track_num && d == b"vp9-keyframe")
    );
    assert!(
        seen.iter()
            .any(|(t, d)| *t == opus_track_num && d == b"opus-pkt-2")
    );
    assert!(
        seen.iter()
            .any(|(t, d)| *t == vp9_track_num && d == b"vp9-p-frame")
    );
}

#[test]
fn cues_let_oracle_seek_to_keyframe() {
    // Mux three video keyframes (one cluster each). The Cues table our
    // muxer emits must let matroska-demuxer seek straight to the third
    // keyframe without scanning every cluster — proof the cluster
    // positions inside Cues are correct.
    let mut m = Muxer::new(DocType::Webm);
    let v = m
        .register_track(TrackDescriptor::video(codec_id::V_VP9, 320, 240))
        .unwrap();
    m.append(v, b"key-0", 0, true).unwrap();
    m.append(v, b"p-0-a", 33_000_000, false).unwrap();
    m.append(v, b"key-1", 66_000_000, true).unwrap();
    m.append(v, b"p-1-a", 100_000_000, false).unwrap();
    m.append(v, b"key-2", 200_000_000, true).unwrap();
    m.append(v, b"p-2-a", 233_000_000, false).unwrap();
    let bytes = m.finalize().unwrap();

    // Sanity: our Demuxer sees three cues.
    let ours = Demuxer::parse(&bytes).expect("our parser");
    assert_eq!(ours.cues.len(), 3);

    let mut oracle = matroska_demuxer::MatroskaFile::open(Cursor::new(&bytes[..])).unwrap();
    // Seek to a timestamp inside the third cluster (ticks, not ns).
    let seek_ticks = 200_000_000 / ours.timestamp_scale_ns;
    oracle.seek(seek_ticks).expect("Cues-driven seek");

    let mut frame = matroska_demuxer::Frame::default();
    let got = oracle.next_frame(&mut frame).unwrap();
    assert!(got, "expected at least one frame after seek");
    assert_eq!(
        &frame.data[..],
        b"key-2",
        "Cues-driven seek must land on the third keyframe"
    );
}

#[test]
fn streaming_mux_output_decodes_in_oracle() {
    // Streaming mode emits an unknown-size Segment with no SeekHead
    // and no Cues; the concatenated init + cluster chunks must still
    // parse cleanly through matroska-demuxer.
    let mut m = Muxer::new(DocType::Webm);
    m.enable_streaming();
    let opus = m
        .register_track(TrackDescriptor::audio(codec_id::A_OPUS, 48000.0, 2))
        .unwrap();
    let vp9 = m
        .register_track(TrackDescriptor::video(codec_id::V_VP9, 640, 360))
        .unwrap();

    let mut wire = m.begin_stream().unwrap();
    m.append(vp9, b"vp9-key-0", 0, true).unwrap();
    m.append(opus, b"opus-0", 0, true).unwrap();
    m.append(opus, b"opus-1", 20_000_000, true).unwrap();
    m.append(vp9, b"vp9-p-0", 33_000_000, false).unwrap();
    wire.extend_from_slice(&m.take_output());
    m.append(vp9, b"vp9-key-1", 66_000_000, true).unwrap();
    m.append(opus, b"opus-2", 60_000_000, true).unwrap();
    wire.extend_from_slice(&m.finish_stream().unwrap());

    let mut oracle = matroska_demuxer::MatroskaFile::open(Cursor::new(&wire[..])).unwrap();
    assert_eq!(oracle.ebml_header().doc_type(), "webm");
    let track_count = oracle.tracks().len();
    assert_eq!(track_count, 2, "oracle must see both registered tracks");

    let mut seen: Vec<(u64, Vec<u8>)> = Vec::new();
    let mut frame = matroska_demuxer::Frame::default();
    while oracle.next_frame(&mut frame).unwrap() {
        seen.push((frame.track, frame.data.clone()));
    }
    assert!(seen.iter().any(|(t, d)| *t == vp9 && d == b"vp9-key-0"));
    assert!(seen.iter().any(|(t, d)| *t == vp9 && d == b"vp9-key-1"));
    assert!(seen.iter().any(|(t, d)| *t == opus && d == b"opus-0"));
    assert!(seen.iter().any(|(t, d)| *t == opus && d == b"opus-2"));
}

#[test]
fn webm_doctype_string_round_trip() {
    // Preserve DocType exactly for callers applying a profile policy;
    // this test checks declaration parity, not demux codec enforcement.
    for dt in ["webm", "matroska"] {
        let bytes = build_file(dt, 500_000, &[audio_track_opus()]);
        let (ours, theirs) = parse_both(&bytes);
        assert_eq!(ours.doc_type.as_str(), theirs.ebml_header().doc_type());
        assert_eq!(ours.doc_type.as_str(), dt);
    }
}
