//! Explicit selected-track PGS images. No OCR, transcript or identification.
use super::{MediaError, SubtitleTrack, enabled};
pub use media_mkv_webm::pgs::{
    CompositionObject, PgsDecoder, PgsDisplay, PgsError, PgsImage, PgsLimits, Rect,
};
use media_mkv_webm::streaming::{StreamingLimits, open_streaming_with_limits};
use std::{
    io::{self, Read, Seek, Write},
    ops::ControlFlow,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanCompletion {
    /// Selected stream reached clean EOF and no display/fragment was pending.
    Complete,
    /// Caller deliberately stopped at a packet boundary; suffix NOT validated.
    Stopped,
}
#[derive(Debug)]
pub struct PgsScanSummary {
    pub track: SubtitleTrack,
    pub completion: ScanCompletion,
    pub packets: usize,
    pub displays: usize,
    pub last_timestamp_ns: Option<u64>,
}
struct Packet {
    bytes: Vec<u8>,
    limit: usize,
    overflow: bool,
}
impl Write for Packet {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit - self.bytes.len() {
            self.overflow = true;
            return Err(io::Error::other("PGS packet byte limit exceeded"));
        }
        self.bytes.try_reserve_exact(bytes.len()).map_err(|_| {
            self.overflow = true;
            io::Error::other("PGS packet allocation failed")
        })?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
/// Streams selected PGS only using the canonical media-core parser. Container
/// budgets are explicit and cumulative, separate from PGS limits (and 4KiB text).
/// Callback images are UNVERIFIED subtitle evidence. A later error means partial
/// extraction; never promote callbacks to whole-file success without Complete.
/// Break suppresses subsequent callbacks and stops after validating this packet;
/// it deliberately does not call finish or assert validity of unread data.
/// Each replacement timestamp closes the preceding image; final end is unknown.
pub fn scan_pgs<R: Read + Seek>(
    reader: R,
    track_number: u64,
    container_limits: StreamingLimits,
    pgs_limits: PgsLimits,
    mut emit: impl FnMut(&PgsDisplay) -> ControlFlow<()>,
) -> Result<PgsScanSummary, MediaError> {
    let mut stream = open_streaming_with_limits(reader, container_limits)
        .map_err(|e| MediaError::Container(e.to_string()))?;
    let candidates = enabled::tracks(&stream)?;
    let track = candidates
        .into_iter()
        .find(|t| t.number == track_number)
        .ok_or(MediaError::TrackNotFound(track_number))?;
    let raw = stream
        .demuxer
        .tracks
        .iter()
        .find(|t| t.number == track_number)
        .unwrap();
    if track.codec_id != "S_HDMV/PGS"
        || raw.codec_delay_ns != 0
        || raw.seek_pre_roll_ns != 0
        || raw.codec_private.as_ref().is_some_and(|v| !v.is_empty())
        || raw.audio.is_some()
        || raw.video.is_some()
    {
        return Err(MediaError::UnsupportedTrack(track_number));
    }
    stream.set_track_filter(Some(vec![track_number]));
    let mut decoder = PgsDecoder::new(pgs_limits);
    let mut summary = PgsScanSummary {
        track,
        completion: ScanCompletion::Complete,
        packets: 0,
        displays: 0,
        last_timestamp_ns: None,
    };
    loop {
        let mut packet = Packet {
            bytes: Vec::new(),
            limit: pgs_limits.packet_bytes,
            overflow: false,
        };
        let result = stream.next_frame_into(&mut packet);
        if packet.overflow {
            return Err(MediaError::Pgs(PgsError::BudgetExceeded(
                "packet bytes/allocation",
            )));
        }
        let Some(header) = result.map_err(|e| MediaError::Container(e.to_string()))? else {
            decoder.finish().map_err(MediaError::Pgs)?;
            return Ok(summary);
        };
        if header.is_invisible || header.is_discardable == Some(true) {
            return Err(MediaError::Pgs(PgsError::Malformed(
                "invisible/discardable subtitle block unsupported",
            )));
        }
        summary.packets += 1;
        let mut stop = false;
        decoder
            .push_packet(header.timestamp_ns, &packet.bytes, |display| {
                summary.displays += 1;
                summary.last_timestamp_ns = Some(display.timestamp_ns);
                if !stop && emit(display).is_break() {
                    stop = true;
                }
            })
            .map_err(MediaError::Pgs)?;
        if stop {
            summary.completion = ScanCompletion::Stopped;
            return Ok(summary);
        }
    }
}
