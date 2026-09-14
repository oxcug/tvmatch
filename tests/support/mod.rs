#![allow(dead_code)]
use media_mkv_webm::{
    DocType, Muxer, TrackDescriptor, TrackKind,
    ebml::{schema::ids, writer::*},
};

pub const DIALOGUE: [&str; 3] = [
    "The copper telescope is growing tiny paper feathers.",
    "Please keep those moonlight jars beneath the staircase.",
    "Our patient comet has finally learned to whistle.",
];
pub fn muxed(tracks: &[(&str, TrackKind)], packets: &[(u64, u64, &[u8])]) -> Vec<u8> {
    let mut mux = Muxer::new(DocType::Matroska);
    for (codec, kind) in tracks {
        mux.register_track(TrackDescriptor {
            kind: *kind,
            codec_id: (*codec).into(),
            language: Some("eng".into()),
            name: Some("UNVERIFIED synthetic captions".into()),
            ..Default::default()
        })
        .unwrap();
    }
    for (track, ms, text) in packets {
        mux.append(*track, text, ms * 1_000_000, true).unwrap();
    }
    mux.finalize().unwrap()
}
pub fn track(number: u64, codec: &str, extra: &[u8]) -> Vec<u8> {
    track_kind(number, codec, 17, extra)
}
pub fn track_kind(number: u64, codec: &str, kind: u64, extra: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    write_uint(ids::TRACK_NUMBER, number, &mut body);
    write_uint(ids::TRACK_UID, number + 100, &mut body);
    write_uint(ids::TRACK_TYPE, kind, &mut body);
    write_ascii(ids::CODEC_ID, codec, &mut body);
    body.extend_from_slice(extra);
    let mut out = Vec::new();
    write_master(ids::TRACK_ENTRY, &body, &mut out);
    out
}
pub fn header() -> Vec<u8> {
    let mut body = Vec::new();
    write_ascii(ids::DOC_TYPE, "matroska", &mut body);
    let mut out = Vec::new();
    write_master(ids::EBML, &body, &mut out);
    open_master_unknown_size(ids::SEGMENT, &mut out);
    out
}
pub fn container(tracks: &[u8], scale: u64, tail: &[u8]) -> Vec<u8> {
    let mut out = header();
    let mut info = Vec::new();
    write_uint(ids::TIMESTAMP_SCALE, scale, &mut info);
    write_master(ids::INFO, &info, &mut out);
    write_master(ids::TRACKS, tracks, &mut out);
    out.extend_from_slice(tail);
    out
}
pub fn block(id: u64, track: u8, delta: i16, flags: u8, text: &[u8]) -> Vec<u8> {
    let mut body = vec![0x80 | track];
    body.extend_from_slice(&delta.to_be_bytes());
    body.push(flags);
    body.extend_from_slice(text);
    let mut out = Vec::new();
    write_element(id, &body, &mut out);
    out
}
pub fn cluster(ticks: u64, blocks: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    write_uint(ids::TIMESTAMP, ticks, &mut body);
    body.extend_from_slice(blocks);
    let mut out = Vec::new();
    write_master(ids::CLUSTER, &body, &mut out);
    out
}
pub fn group(blocks: &[u8], duration: Option<u64>) -> Vec<u8> {
    let mut body = blocks.to_vec();
    if let Some(duration) = duration {
        write_uint(ids::BLOCK_DURATION, duration, &mut body);
    }
    let mut out = Vec::new();
    write_master(ids::BLOCK_GROUP, &body, &mut out);
    out
}
