//! Built-in bounded subset for consumers that must not silently lose subtitle evidence.
use super::*;

#[cfg(test)]
mod tests;

/// Budgets are cumulative for one open + forward walk; seeking does not reset them.
/// Reads count underlying buffered I/O, including read-ahead. No wall-clock deadline.
#[derive(Debug, Clone, Copy)]
pub struct StreamingLimits {
    /// Explicit opt-out for forward-only consumers; default retains bounded Cues.
    pub skip_cues: bool,
    pub metadata_element_bytes: usize,
    pub total_metadata_bytes: usize,
    pub elements: usize,
    pub read_bytes: u64,
    pub io_operations: u64,
}
impl Default for StreamingLimits {
    fn default() -> Self {
        Self {
            skip_cues: false,
            metadata_element_bytes: 1_048_576,
            total_metadata_bytes: 2_097_152,
            elements: 100_000,
            read_bytes: 64 * 1024 * 1024,
            io_operations: 1_000_000,
        }
    }
}

pub(super) struct WalkBudget {
    elements: usize,
}
impl WalkBudget {
    pub(super) fn element(&mut self) -> Result<()> {
        self.elements = self.elements.checked_sub(1).ok_or(Error::Unsupported {
            what: "strict element budget exceeded",
        })?;
        Ok(())
    }
}

pub(super) struct BudgetReader<R> {
    inner: R,
    bytes: u64,
    operations: u64,
    /// Refill cap for filtered header walking; selected payload copies lift it.
    pub(super) max_read: usize,
}
impl<R> BudgetReader<R> {
    pub(super) fn new(inner: R, limits: StreamingLimits) -> Self {
        Self {
            inner,
            bytes: limits.read_bytes,
            operations: limits.io_operations,
            max_read: usize::MAX,
        }
    }
    fn operation(&mut self) -> std::io::Result<()> {
        self.operations = self
            .operations
            .checked_sub(1)
            .ok_or_else(|| std::io::Error::other("strict I/O operation budget exceeded"))?;
        Ok(())
    }
}
impl<R: Read> Read for BudgetReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.operation()?;
        if buf.is_empty() {
            return Ok(0);
        }
        if self.bytes == 0 {
            return Err(std::io::Error::other("strict read byte budget exceeded"));
        }
        let len = buf
            .len()
            .min(self.max_read)
            .min(usize::try_from(self.bytes).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..len])?;
        self.bytes -= n as u64;
        Ok(n)
    }
}
impl<R: Seek> Seek for BudgetReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.operation()?;
        self.inner.seek(pos)
    }
}

pub(super) fn checked_end(h: &IoHeader, parent_end: u64) -> Result<u64> {
    let end = h.payload_end.ok_or(Error::Unsupported {
        what: "strict stream requires known child sizes",
    })?;
    if end < h.payload_start || end > parent_end {
        return Err(Error::Malformed("element exceeds parent/file bounds"));
    }
    Ok(end)
}

/// Open the conservative strict subset using the same metadata parsers and frame walker
/// as `open_streaming`. Metadata is discovered across Segment children, including after Clusters. Segment may have unknown
/// size; all other elements must have known sizes. SeekHead/attachments/tags/chapters
/// are opaque and seek-skipped. Cues are bounded and parsed after final Info discovery
/// unless explicitly disabled with `skip_cues`. Only the explicitly
/// supported metadata fields below are accepted. In particular track transforms and
/// alternative timing are rejected on ALL tracks, not silently ignored. Video-only
/// BlockAdditionMapping configuration is structurally validated then ignored (not
/// projected or interpreted). This does not support per-frame BlockAdditions or
/// subtitle format extensions, nor promise complete video decoder configuration.
///
/// Caller must use a bounded writer for selected packets and discard partial output on
/// ANY error. Unknown-size clusters, selected lacing, malformed groups, negative or
/// overflowing times and truncated headers/payloads are errors. This is
/// not a complete Matroska validator (opaque unselected payloads and CRCs are not verified).
pub fn open_streaming_with_limits<R: Read + Seek>(
    reader: R,
    limits: StreamingLimits,
) -> Result<StreamingDemuxer<R>> {
    // Discovery jumps across distant Cluster boundaries. Read only requested
    // header/metadata bytes here, not a 64 KiB payload prefix at every seek.
    let mut reader = BudgetReader::new(reader, limits);
    let file_len = reader.seek(SeekFrom::End(0))?;
    // Existing Demuxer offsets are usize; relative seeks use i64.
    usize::try_from(file_len).map_err(|_| Error::SizeOverflow)?;
    i64::try_from(file_len).map_err(|_| Error::SizeOverflow)?;
    let mut state = WalkBudget {
        elements: limits.elements,
    };
    let mut metadata_left = limits.total_metadata_bytes;
    let ebml = read_element_header_at(&mut reader, 0)?;
    state.element()?;
    if ebml.id != ids::EBML {
        return Err(Error::NotMatroska);
    }
    let ebml_end = checked_end(&ebml, file_len)?;
    let bytes = metadata(
        &mut reader,
        &ebml,
        file_len,
        limits,
        &mut metadata_left,
        &mut state,
    )?;
    let (doc_type, doc_type_version) = parse_ebml_header(&mut Reader::new(&bytes))?;
    let doc_type = DocType::parse(&doc_type).ok_or(Error::NotMatroska)?;
    let mut pos = ebml_end;
    let segment = loop {
        let h = read_element_header_at(&mut reader, pos)?;
        state.element()?;
        if h.id == ids::SEGMENT {
            break h;
        }
        if !matches!(h.id, ids::VOID | ids::CRC32) {
            return Err(Error::NotMatroska);
        }
        pos = checked_end(&h, file_len)?;
    };
    let segment_end = match segment.payload_end {
        Some(_) => checked_end(&segment, file_len)?,
        None => file_len,
    };
    if segment_end != file_len {
        return Err(Error::Unsupported {
            what: "trailing data/multiple Segments",
        });
    }
    let mut demuxer = Demuxer {
        doc_type,
        doc_type_version,
        timestamp_scale_ns: 1_000_000,
        duration_ticks: None,
        title: None,
        muxing_app: None,
        writing_app: None,
        tracks: Vec::new(),
        clusters_offset: segment_end as usize,
        segment_end: segment_end as usize,
        segment_payload_start: segment.payload_start as usize,
        cues: Vec::new(),
    };
    let mut info_seen = false;
    let mut tracks_seen = false;
    let mut cues_location = None;
    let mut cluster_offsets = std::collections::BTreeSet::new();
    pos = segment.payload_start;
    while pos < segment_end {
        let h = read_element_header_at(&mut reader, pos)?;
        state.element()?;
        let end = checked_end(&h, segment_end)?;
        if h.id == ids::CLUSTER {
            cluster_offsets.insert(pos);
            if demuxer.clusters_offset == segment_end as usize {
                demuxer.clusters_offset = pos as usize;
            }
            pos = end;
            continue;
        }
        match h.id {
            ids::INFO | ids::TRACKS => {
                let seen = if h.id == ids::INFO {
                    &mut info_seen
                } else {
                    &mut tracks_seen
                };
                if *seen {
                    return Err(Error::Malformed("duplicate Info/Tracks"));
                }
                *seen = true;
                let bytes = metadata(
                    &mut reader,
                    &h,
                    segment_end,
                    limits,
                    &mut metadata_left,
                    &mut state,
                )?;
                let mut r = Reader::new(&bytes);
                if h.id == ids::INFO {
                    parse_info(
                        &mut r,
                        &mut demuxer.timestamp_scale_ns,
                        &mut demuxer.duration_ticks,
                        &mut demuxer.title,
                        &mut demuxer.muxing_app,
                        &mut demuxer.writing_app,
                    )?;
                } else {
                    parse_tracks(&mut r, &mut demuxer.tracks)?;
                }
            }
            ids::CUES => {
                if cues_location.is_some() {
                    return Err(Error::Malformed("duplicate Cues"));
                }
                cues_location = Some(h);
            }
            ids::SEEK_HEAD
            | ids::ATTACHMENTS
            | ids::CHAPTERS
            | ids::TAGS
            | ids::VOID
            | ids::CRC32 => {}
            _ => {
                return Err(Error::UnsupportedElement {
                    parent: ids::SEGMENT,
                    element: h.id,
                });
            }
        }
        pos = end;
    }
    if !tracks_seen || demuxer.timestamp_scale_ns == 0 {
        return Err(Error::Malformed("missing Tracks or zero TimestampScale"));
    }
    if !limits.skip_cues
        && let Some(h) = cues_location
    {
        let bytes = metadata(
            &mut reader,
            &h,
            segment_end,
            limits,
            &mut metadata_left,
            &mut state,
        )?;
        parse_cues(
            &mut Reader::new(&bytes),
            demuxer.timestamp_scale_ns,
            &mut demuxer.cues,
        )?;
        let track_numbers: std::collections::BTreeSet<_> =
            demuxer.tracks.iter().map(|t| t.number).collect();
        for cue in &demuxer.cues {
            let offset = segment
                .payload_start
                .checked_add(cue.cluster_position)
                .ok_or(Error::SizeOverflow)?;
            if !cluster_offsets.contains(&offset) || !track_numbers.contains(&cue.track_number) {
                return Err(Error::Malformed(
                    "Cue target is not a declared track/Cluster boundary",
                ));
            }
        }
    }
    let walk = WalkState {
        pos: demuxer.clusters_offset as u64,
        cluster: None,
    };
    Ok(StreamingDemuxer {
        demuxer,
        // No buffered bytes to discard; preserve the same reader and cumulative
        // budgets. The frame walker seeks to its absolute `walk.pos` on entry.
        reader: BufReader::with_capacity(64 * 1024, reader),
        file_len,
        walk,
        track_filter: None,
        budget: state,
    })
}

fn metadata<R: Read + Seek>(
    reader: &mut R,
    h: &IoHeader,
    parent_end: u64,
    limits: StreamingLimits,
    left: &mut usize,
    state: &mut WalkBudget,
) -> Result<Vec<u8>> {
    let len = usize::try_from(checked_end(h, parent_end)? - h.payload_start)
        .map_err(|_| Error::SizeOverflow)?;
    if len > limits.metadata_element_bytes || len > *left {
        return Err(Error::Unsupported {
            what: "strict metadata byte budget exceeded",
        });
    }
    *left -= len;
    let bytes = read_at(reader, h.payload_start, len as u64)?;
    validate_metadata(&bytes, h.id, state)?;
    Ok(bytes)
}

// Validation policy over the existing EBML Reader, not another EBML parser. Checking
// bounds before Reader's slice helpers also avoids their unchecked usize size cast.
fn validate_metadata(bytes: &[u8], parent: u64, state: &mut WalkBudget) -> Result<()> {
    let mut r = Reader::new(bytes);
    let mut seen = std::collections::BTreeSet::new();
    let mut track_type = None;
    let mut extended_track = false;
    while !r.eof() {
        state.element()?;
        let h = r.read_header()?;
        let end = h
            .size
            .and_then(|n| usize::try_from(n).ok())
            .and_then(|n| h.payload_start.checked_add(n))
            .filter(|&end| end <= bytes.len())
            .ok_or(Error::SizeOverflow)?;
        let id = h.id.value;
        let duplicate = !seen.insert(id);
        if !matches!(
            id,
            ids::TRACK_ENTRY
                | ids::BLOCK_ADDITION_MAPPING
                | ids::CUE_POINT
                | ids::CUE_TRACK_POSITIONS
                | ids::VOID
                | ids::CRC32
        ) && duplicate
        {
            return Err(Error::Malformed("duplicate strict metadata field"));
        }
        let allowed = match parent {
            ids::EBML => matches!(
                id,
                ids::EBML_VERSION
                    | ids::EBML_READ_VERSION
                    | ids::EBML_MAX_ID_LENGTH
                    | ids::EBML_MAX_SIZE_LENGTH
                    | ids::DOC_TYPE
                    | ids::DOC_TYPE_VERSION
                    | ids::DOC_TYPE_READ_VERSION
            ),
            ids::INFO => matches!(
                id,
                ids::TIMESTAMP_SCALE
                    | ids::DURATION
                    | ids::DATE_UTC
                    | ids::TITLE
                    | ids::MUXING_APP
                    | ids::WRITING_APP
                    | ids::SEGMENT_UID
            ),
            ids::TRACKS => id == ids::TRACK_ENTRY,
            ids::CUES => id == ids::CUE_POINT,
            ids::CUE_POINT => matches!(id, ids::CUE_TIME | ids::CUE_TRACK_POSITIONS),
            ids::CUE_TRACK_POSITIONS => matches!(
                id,
                ids::CUE_TRACK | ids::CUE_CLUSTER_POSITION | 0xF0 | 0xB2 | 0x5378
            ),
            ids::TRACK_ENTRY => matches!(
                id,
                ids::TRACK_NUMBER
                    | ids::TRACK_UID
                    | ids::TRACK_TYPE
                    | ids::FLAG_ENABLED
                    | ids::FLAG_DEFAULT
                    | ids::FLAG_FORCED
                    | ids::FLAG_LACING
                    | ids::MIN_CACHE
                    | ids::MAX_BLOCK_ADDITION_ID
                    | ids::BLOCK_ADDITION_MAPPING
                    | ids::DEFAULT_DURATION
                    | ids::NAME
                    | ids::LANGUAGE
                    | ids::LANGUAGE_IETF
                    | ids::CODEC_ID
                    | ids::CODEC_PRIVATE
                    | ids::CODEC_NAME
                    | ids::CODEC_DELAY
                    | ids::SEEK_PRE_ROLL
                    | ids::VIDEO
                    | ids::AUDIO
            ),
            ids::BLOCK_ADDITION_MAPPING => matches!(
                id,
                ids::BLOCK_ADD_ID_VALUE
                    | ids::BLOCK_ADD_ID_NAME
                    | ids::BLOCK_ADD_ID_TYPE
                    | ids::BLOCK_ADD_ID_EXTRA_DATA
            ),
            ids::VIDEO => matches!(
                id,
                ids::PIXEL_WIDTH
                    | ids::PIXEL_HEIGHT
                    | ids::DISPLAY_WIDTH
                    | ids::DISPLAY_HEIGHT
                    | ids::FRAME_RATE
                    | ids::FLAG_INTERLACED
            ),
            ids::AUDIO => matches!(
                id,
                ids::SAMPLING_FREQUENCY
                    | ids::OUTPUT_SAMPLING_FREQUENCY
                    | ids::CHANNELS
                    | ids::BIT_DEPTH
            ),
            _ => false,
        };
        if !allowed && !matches!(id, ids::VOID | ids::CRC32) {
            return Err(Error::UnsupportedElement {
                parent,
                element: id,
            });
        }
        if parent == ids::TRACK_ENTRY {
            match id {
                ids::TRACK_TYPE => track_type = Some(r.read_uint(&h)?),
                ids::BLOCK_ADDITION_MAPPING => extended_track = true,
                ids::MAX_BLOCK_ADDITION_ID => extended_track |= r.read_uint(&h)? != 0,
                _ => {}
            }
        }
        if parent == ids::BLOCK_ADDITION_MAPPING {
            match id {
                ids::BLOCK_ADD_ID_VALUE => {
                    if r.read_uint(&h)? < 2 {
                        return Err(Error::Malformed("BlockAddIDValue must be at least 2"));
                    }
                }
                ids::BLOCK_ADD_ID_TYPE => {
                    r.read_uint(&h)?;
                }
                // Matroska 'string' is ASCII, unlike the UTF-8 Track Name.
                ids::BLOCK_ADD_ID_NAME
                    if bytes[h.payload_start..end]
                        .iter()
                        .any(|b| !(0x20..=0x7e).contains(b)) =>
                {
                    return Err(Error::Malformed("BlockAddIDName must be printable ASCII"));
                }
                _ => {}
            }
        }
        if parent == ids::TRACK_ENTRY && id == ids::MIN_CACHE {
            // Historical Matroska MinCache is a uint/default 0 (RFC 8794 sections
            // 6.1, 7.2: empty through 8 bytes). Validate, but never size a cache
            // from this playback hint; packet demuxing does not use it.
            r.read_uint(&h).map_err(|_| {
                Error::Malformed(
                    "MinCache element 0x6DE7 in parent 0xAE: unsigned integer > 8 bytes",
                )
            })?;
        }
        if matches!(
            id,
            ids::TRACK_ENTRY
                | ids::BLOCK_ADDITION_MAPPING
                | ids::VIDEO
                | ids::AUDIO
                | ids::CUE_POINT
                | ids::CUE_TRACK_POSITIONS
        ) {
            validate_metadata(&bytes[h.payload_start..end], id, state)?;
        }
        if matches!(
            id,
            ids::DOC_TYPE
                | ids::TITLE
                | ids::MUXING_APP
                | ids::WRITING_APP
                | ids::NAME
                | ids::LANGUAGE
                | ids::LANGUAGE_IETF
                | ids::CODEC_ID
                | ids::CODEC_NAME
        ) {
            let text = std::str::from_utf8(&bytes[h.payload_start..end])
                .map_err(|_| Error::Malformed("strict metadata UTF-8"))?;
            if text.chars().any(char::is_control) {
                return Err(Error::Malformed("strict metadata control character"));
            }
        }
        if parent == ids::EBML && !matches!(id, ids::DOC_TYPE | ids::VOID | ids::CRC32) {
            let v = r.read_uint(&h)?;
            let max = match id {
                ids::EBML_MAX_ID_LENGTH => 4,
                ids::EBML_MAX_SIZE_LENGTH => 8,
                ids::DOC_TYPE_VERSION | ids::DOC_TYPE_READ_VERSION => 4,
                _ => 1,
            };
            if v == 0 || v > max {
                return Err(Error::Unsupported {
                    what: "strict EBML version/width",
                });
            }
        }
        r.skip_payload(&h)?;
    }
    // Check after the whole entry: EBML field order is not significant. Do not
    // let unimplemented subtitle/audio extensions change decoded evidence.
    if parent == ids::TRACK_ENTRY && extended_track && track_type != Some(1) {
        return Err(Error::Unsupported {
            what: "BlockAdditionMapping/MaxBlockAdditionID extensions are supported only as opaque video metadata",
        });
    }
    if (parent == ids::CUE_POINT
        && (!seen.contains(&ids::CUE_TIME) || !seen.contains(&ids::CUE_TRACK_POSITIONS)))
        || (parent == ids::CUE_TRACK_POSITIONS
            && (!seen.contains(&ids::CUE_TRACK) || !seen.contains(&ids::CUE_CLUSTER_POSITION)))
    {
        return Err(Error::Malformed("missing required Cue field"));
    }
    Ok(())
}
