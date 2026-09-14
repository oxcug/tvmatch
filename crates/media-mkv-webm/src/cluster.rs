//! Cluster + Block walking between metadata demux and caller-owned codec decoding.
//!
//! Lacing mode 0 carries one frame after the Block header. Modes 1/2/3
//! (Xiph / fixed-size / EBML lacing) return [`Error::Unsupported`], including
//! otherwise valid files that use lacing.

use crate::ebml::element::Reader;
use crate::ebml::schema::ids;
use crate::ebml::varint::read_vint_raw;
use crate::{Demuxer, Error, Result};

/// One demuxed frame, with timestamp already promoted to nanoseconds.
///
/// `data` borrows from the original input slice — no copies. Holding
/// a [`Frame`] keeps the input alive.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    pub track: u64,
    pub timestamp_ns: u64,
    /// Keyframe flag. For SimpleBlock this is the explicit header bit;
    /// for BlockGroup it's inferred from `ReferenceBlock` absence
    /// (Matroska §10: a Block with no ReferenceBlock is a keyframe).
    pub is_keyframe: Option<bool>,
    pub is_invisible: bool,
    /// SimpleBlock discardable flag. `None` for plain `Block` frames.
    pub is_discardable: Option<bool>,
    pub data: &'a [u8],
}

/// Iterator that walks Clusters/Blocks from the byte offset where
/// [`Demuxer::parse`] left off. Returns one [`Frame`] per call to
/// [`Frames::next`].
pub struct Frames<'a> {
    input: &'a [u8],
    pos: usize,
    segment_end: usize,
    timestamp_scale_ns: u64,
    /// Cluster timestamp ticks (segment ticks). Multiplied by
    /// `timestamp_scale_ns` for the per-frame absolute ts.
    cluster_ts_ticks: u64,
    /// When set, we're partway through walking a Cluster — `pos` is
    /// inside that cluster's payload and `cluster_end` bounds it.
    cluster_end: Option<usize>,
}

impl<'a> Frames<'a> {
    /// Open a frame iterator at the cluster offset recorded by the
    /// metadata demuxer.
    pub fn new(input: &'a [u8], demuxer: &Demuxer) -> Self {
        Self {
            input,
            pos: demuxer.clusters_offset,
            segment_end: demuxer.segment_end,
            timestamp_scale_ns: demuxer.timestamp_scale_ns,
            cluster_ts_ticks: 0,
            cluster_end: None,
        }
    }

    /// Read the next frame, or `None` at end of segment.
    pub fn next_frame(&mut self) -> Result<Option<Frame<'a>>> {
        loop {
            // If we're outside a cluster, look for the next one.
            if self.cluster_end.is_none() {
                if self.pos >= self.segment_end {
                    return Ok(None);
                }
                let mut r = Reader::new_at(self.input, self.pos);
                let hdr = match r.read_header() {
                    Ok(h) => h,
                    Err(Error::UnexpectedEof(_)) => return Ok(None),
                    Err(e) => return Err(e),
                };
                self.pos = r.position();
                match hdr.id.value {
                    ids::CLUSTER => {
                        let end = hdr.payload_end().unwrap_or(self.segment_end);
                        self.cluster_end = Some(end);
                        self.cluster_ts_ticks = 0;
                        continue;
                    }
                    ids::CUES
                    | ids::ATTACHMENTS
                    | ids::CHAPTERS
                    | ids::TAGS
                    | ids::SEEK_HEAD
                    | ids::VOID
                    | ids::CRC32 => {
                        // Acknowledge sibling-level elements that may
                        // appear after Cluster regions. Skip them.
                        self.pos = match hdr.payload_end() {
                            Some(e) => e,
                            None => return Ok(None),
                        };
                        continue;
                    }
                    _ => {
                        // Unknown sibling — skip if we can, else bail.
                        match hdr.payload_end() {
                            Some(e) => {
                                self.pos = e;
                                continue;
                            }
                            None => return Ok(None),
                        }
                    }
                }
            }

            // Inside a cluster. Walk its children.
            let cluster_end = self.cluster_end.unwrap();
            if self.pos >= cluster_end {
                self.cluster_end = None;
                continue;
            }

            let mut r = Reader::new_at(self.input, self.pos);
            let hdr = r.read_header()?;
            self.pos = r.position();
            match hdr.id.value {
                ids::TIMESTAMP => {
                    self.cluster_ts_ticks = read_uint_payload(self.input, &hdr)?;
                    self.pos = hdr
                        .payload_end()
                        .ok_or(Error::Malformed("Cluster Timestamp"))?;
                }
                ids::SIMPLE_BLOCK => {
                    let payload = self.payload_slice(&hdr)?;
                    let frame = decode_block_header(
                        payload,
                        true,
                        self.cluster_ts(),
                        self.timestamp_scale_ns,
                    )?;
                    self.pos = hdr.payload_end().unwrap();
                    return Ok(Some(frame));
                }
                ids::BLOCK_GROUP => {
                    // Walk children to find the embedded Block. Track
                    // ReferenceBlock presence so we can infer keyframe
                    // status: a Block with no ReferenceBlock children is
                    // a keyframe (Matroska §10).
                    let group_end = hdr
                        .payload_end()
                        .ok_or(Error::Malformed("BlockGroup size"))?;
                    let mut frame: Option<Frame<'a>> = None;
                    let mut has_reference = false;
                    let mut gp = self.pos;
                    while gp < group_end {
                        let mut gr = Reader::new_at(self.input, gp);
                        let ch = gr.read_header()?;
                        match ch.id.value {
                            ids::BLOCK => {
                                let payload = slice_payload(self.input, &ch)?;
                                frame = Some(decode_block_header(
                                    payload,
                                    false,
                                    self.cluster_ts(),
                                    self.timestamp_scale_ns,
                                )?);
                            }
                            ids::REFERENCE_BLOCK => {
                                has_reference = true;
                            }
                            _ => {}
                        }
                        gp = ch
                            .payload_end()
                            .ok_or(Error::Malformed("BlockGroup child size"))?;
                    }
                    self.pos = group_end;
                    if let Some(mut f) = frame {
                        f.is_keyframe = Some(!has_reference);
                        return Ok(Some(f));
                    }
                    // No Block inside BlockGroup — odd but not fatal.
                    continue;
                }
                ids::VOID | ids::CRC32 => {
                    self.pos = hdr.payload_end().ok_or(Error::Malformed("Void/CRC"))?;
                }
                _ => {
                    // Unrecognised cluster child (PrevSize, Position, …).
                    self.pos = hdr.payload_end().ok_or(Error::Malformed("Cluster child"))?;
                }
            }
        }
    }

    fn payload_slice(&self, hdr: &crate::ebml::element::ElementHeader) -> Result<&'a [u8]> {
        slice_payload(self.input, hdr)
    }

    fn cluster_ts(&self) -> u64 {
        self.cluster_ts_ticks
            .saturating_mul(self.timestamp_scale_ns)
    }
}

impl<'a> Iterator for Frames<'a> {
    type Item = Result<Frame<'a>>;
    fn next(&mut self) -> Option<Self::Item> {
        match self.next_frame() {
            Ok(Some(f)) => Some(Ok(f)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

fn slice_payload<'a>(
    input: &'a [u8],
    hdr: &crate::ebml::element::ElementHeader,
) -> Result<&'a [u8]> {
    let end = hdr.payload_end().ok_or(Error::Malformed(
        "payload requested for unknown-size element",
    ))?;
    input.get(hdr.payload_start..end).ok_or(Error::SizeOverflow)
}

fn read_uint_payload(input: &[u8], hdr: &crate::ebml::element::ElementHeader) -> Result<u64> {
    let bytes = slice_payload(input, hdr)?;
    if bytes.len() > 8 {
        return Err(Error::Malformed("uint > 8 bytes"));
    }
    let mut v = 0u64;
    for &b in bytes {
        v = (v << 8) | b as u64;
    }
    Ok(v)
}

/// Parse the Block / SimpleBlock header at the start of `payload` and
/// return a Frame referencing the frame bytes.
///
/// Block header layout (Matroska §10):
/// - VINT track number
/// - i16 BE timestamp delta (signed offset from Cluster timestamp)
/// - 1 byte flags
///   - SimpleBlock: bit 7 = keyframe, bits 3-1 = lacing, bit 0 = discardable
///   - Block:       bits 3-1 = lacing only; other bits reserved
/// - Frame data (lacing-dependent layout)
fn decode_block_header<'a>(
    payload: &'a [u8],
    is_simple: bool,
    cluster_ts_ns: u64,
    timestamp_scale_ns: u64,
) -> Result<Frame<'a>> {
    let mut pos = 0;
    let (track, _, _) = read_vint_raw(payload, &mut pos)?;
    if pos + 3 > payload.len() {
        return Err(Error::UnexpectedEof("Block header"));
    }
    let ts_delta = i16::from_be_bytes([payload[pos], payload[pos + 1]]);
    pos += 2;
    let flags = payload[pos];
    pos += 1;

    let lacing = (flags >> 1) & 0b11;
    if lacing != 0 {
        return Err(Error::Unsupported {
            what: "lacing modes 1/2/3",
        });
    }

    let invisible = (flags & 0b0000_1000) != 0;
    let is_keyframe = if is_simple {
        Some((flags & 0b1000_0000) != 0)
    } else {
        None
    };
    let is_discardable = if is_simple {
        Some((flags & 0b0000_0001) != 0)
    } else {
        None
    };

    let data = payload.get(pos..).ok_or(Error::SizeOverflow)?;

    // Cluster ts is already in ns (scaled). Block delta is in segment
    // ticks → scale to ns. Use signed arithmetic to permit negative
    // deltas (rare but legal for B-frames in MKV).
    let delta_ns = (ts_delta as i64).saturating_mul(timestamp_scale_ns as i64);
    let timestamp_ns = (cluster_ts_ns as i64).saturating_add(delta_ns).max(0) as u64;

    Ok(Frame {
        track,
        timestamp_ns,
        is_keyframe,
        is_invisible: invisible,
        is_discardable,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elem1(id: u8, payload: &[u8]) -> Vec<u8> {
        assert!(payload.len() < 0x80);
        let mut v = vec![id, 0x80 | payload.len() as u8];
        v.extend_from_slice(payload);
        v
    }
    fn elem_id2(id: u16, payload: &[u8]) -> Vec<u8> {
        let mut v = id.to_be_bytes().to_vec();
        v.push(0x80 | payload.len() as u8);
        v.extend_from_slice(payload);
        v
    }
    fn elem_id4(id: u32, payload: &[u8]) -> Vec<u8> {
        let mut v = id.to_be_bytes().to_vec();
        v.push(0x80 | payload.len() as u8);
        v.extend_from_slice(payload);
        v
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

    /// Build a SimpleBlock body: vint(track) + i16(delta) + flags + frame_bytes.
    fn simple_block(track: u8, delta: i16, flags: u8, data: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(0x80 | track); // 1-byte VINT
        body.extend_from_slice(&delta.to_be_bytes());
        body.push(flags);
        body.extend_from_slice(data);
        elem1(0xA3, &body)
    }

    #[test]
    fn simple_block_round_trip() {
        // EBML header
        let mut ebml_body = Vec::new();
        ebml_body.extend_from_slice(&elem_id2(0x4282, b"matroska"));
        let ebml = elem_id4(0x1A45DFA3, &ebml_body);

        // Info { TimestampScale = 1_000_000 }
        let info_body = {
            let mut v = vec![0x2A, 0xD7, 0xB1, 0x83];
            v.extend_from_slice(&[0x0F, 0x42, 0x40]);
            v
        };
        let info = elem_id4(0x1549A966, &info_body);

        // Tracks { TrackEntry: number=1, type=audio, codec=A_OPUS }
        let mut entry = Vec::new();
        entry.extend_from_slice(&elem1(0xD7, &uint_payload(1)));
        entry.extend_from_slice(&elem1(0x83, &uint_payload(2)));
        entry.extend_from_slice(&elem1(0x86, b"A_OPUS"));
        let tracks = elem_id4(0x1654AE6B, &elem1(0xAE, &entry));

        // Cluster { Timestamp=10, SimpleBlock(track=1, +5, key, "hello"),
        //           SimpleBlock(track=1, +20, not-key, "world!") }
        let mut cluster_body = Vec::new();
        cluster_body.extend_from_slice(&elem1(0xE7, &uint_payload(10)));
        cluster_body.extend_from_slice(&simple_block(1, 5, 0x80, b"hello"));
        cluster_body.extend_from_slice(&simple_block(1, 20, 0x00, b"world!"));
        let cluster = elem_id4(0x1F43B675, &cluster_body);

        let mut segment_payload = Vec::new();
        segment_payload.extend_from_slice(&info);
        segment_payload.extend_from_slice(&tracks);
        segment_payload.extend_from_slice(&cluster);
        let segment = elem_id4(0x18538067, &segment_payload);

        let mut file = ebml;
        file.extend_from_slice(&segment);

        let d = Demuxer::parse(&file).unwrap();
        let mut frames = Frames::new(&file, &d);

        let f1 = frames.next_frame().unwrap().unwrap();
        assert_eq!(f1.track, 1);
        assert_eq!(f1.timestamp_ns, (10 + 5) * 1_000_000);
        assert_eq!(f1.is_keyframe, Some(true));
        assert_eq!(f1.data, b"hello");

        let f2 = frames.next_frame().unwrap().unwrap();
        assert_eq!(f2.track, 1);
        assert_eq!(f2.timestamp_ns, (10 + 20) * 1_000_000);
        assert_eq!(f2.is_keyframe, Some(false));
        assert_eq!(f2.data, b"world!");

        assert!(frames.next_frame().unwrap().is_none());
    }

    /// Build a Block body (same layout as SimpleBlock, but the flag
    /// keyframe/discardable bits are reserved): vint(track) + i16(delta)
    /// + flags + frame_bytes, wrapped in element id 0xA1.
    fn block(track: u8, delta: i16, flags: u8, data: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(0x80 | track);
        body.extend_from_slice(&delta.to_be_bytes());
        body.push(flags);
        body.extend_from_slice(data);
        elem1(0xA1, &body)
    }

    /// ReferenceBlock element (id 0xFB) carrying a signed-int payload.
    fn reference_block(delta: i8) -> Vec<u8> {
        elem1(0xFB, &[delta as u8])
    }

    /// Wrap children in a BlockGroup (id 0xA0).
    fn block_group(children: &[u8]) -> Vec<u8> {
        elem1(0xA0, children)
    }

    fn build_segment(cluster_body: &[u8]) -> Vec<u8> {
        let mut ebml_body = Vec::new();
        ebml_body.extend_from_slice(&elem_id2(0x4282, b"matroska"));
        let ebml = elem_id4(0x1A45DFA3, &ebml_body);

        let info_body = {
            let mut v = vec![0x2A, 0xD7, 0xB1, 0x83];
            v.extend_from_slice(&[0x0F, 0x42, 0x40]);
            v
        };
        let info = elem_id4(0x1549A966, &info_body);

        let mut entry = Vec::new();
        entry.extend_from_slice(&elem1(0xD7, &uint_payload(1)));
        entry.extend_from_slice(&elem1(0x83, &uint_payload(1))); // video
        entry.extend_from_slice(&elem1(0x86, b"V_VP9"));
        let tracks = elem_id4(0x1654AE6B, &elem1(0xAE, &entry));

        let cluster = elem_id4(0x1F43B675, cluster_body);

        let mut segment_payload = Vec::new();
        segment_payload.extend_from_slice(&info);
        segment_payload.extend_from_slice(&tracks);
        segment_payload.extend_from_slice(&cluster);
        let segment = elem_id4(0x18538067, &segment_payload);

        let mut file = ebml;
        file.extend_from_slice(&segment);
        file
    }

    #[test]
    fn block_group_keyframe_inferred_from_reference_block_absence() {
        let mut cluster_body = Vec::new();
        cluster_body.extend_from_slice(&elem1(0xE7, &uint_payload(0)));
        // Keyframe: BlockGroup containing only a Block.
        cluster_body.extend_from_slice(&block_group(&block(1, 0, 0x00, b"key")));
        // Non-keyframe: BlockGroup containing Block + ReferenceBlock.
        let mut children = Vec::new();
        children.extend_from_slice(&block(1, 10, 0x00, b"delta"));
        children.extend_from_slice(&reference_block(-10));
        cluster_body.extend_from_slice(&block_group(&children));

        let file = build_segment(&cluster_body);
        let d = Demuxer::parse(&file).unwrap();
        let mut frames = Frames::new(&file, &d);

        let f1 = frames.next_frame().unwrap().unwrap();
        assert_eq!(f1.data, b"key");
        assert_eq!(f1.is_keyframe, Some(true));
        assert_eq!(f1.is_discardable, None);

        let f2 = frames.next_frame().unwrap().unwrap();
        assert_eq!(f2.data, b"delta");
        assert_eq!(f2.is_keyframe, Some(false));
        assert_eq!(f2.is_discardable, None);

        assert!(frames.next_frame().unwrap().is_none());
    }

    #[test]
    fn lacing_unsupported() {
        let mut body = Vec::new();
        body.push(0x81); // track 1
        body.extend_from_slice(&0i16.to_be_bytes());
        body.push(0x02); // lacing mode = Xiph
        body.push(0xAA);
        let block = elem1(0xA3, &body);

        // Smallest possible test fixture: just decode the SimpleBlock payload.
        let payload = &block[2..]; // skip id+size
        let err = decode_block_header(payload, true, 0, 1_000_000).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }));
    }
}
