# Native MKV subtitle evidence

The `media` feature enables native container libraries with default features disabled;
`ocr` adds PGS-to-text processing. Default tvmatch features include both OCR and
OpenSubtitles. Build prerequisites are in [contributing](CONTRIBUTING.md).

[media::probe_subtitle_tracks](../src/media.rs) and `media::extract_subtitles`
accept `Read + Seek`. Probing validates bounded metadata, not Cluster payloads.
Direct extraction returns `EmbeddedSubtitles`: a checked transcript, selected track
and one-to-one raw timestamp records. Library selection uses an explicit TrackNumber,
or requires exactly one subtitle track, counting unsupported/disabled tracks too.
The CLI instead tries eligible English tracks as described in [README](../README.md).

All track IDs, codec/language/name/flags and durations are unverified declarations.
Matching subtitles does not verify the corresponding audio/video. LanguageIETF takes
precedence over legacy Language regardless of element order. `supported` on a probed
track means direct text extraction support, not bitmap/OCR support.

## Supported subset

| Input | Handling |
|---|---|
| Matroska `S_TEXT/UTF8`, unlaced SimpleBlock | Direct Unicode text |
| Single-Block BlockGroup with optional BlockDuration | Supported, retaining declared zero |
| `S_HDMV/PGS` | Separate image callback API and optional OCR; not direct text extraction |
| ASS/SSA, WebVTT and other subtitle codecs | Unsupported extraction |
| Selected nonempty CodecPrivate, nonzero CodecDelay/SeekPreRoll, Audio/Video parameters | Rejected |
| TrackEntry MinCache `0x6DE7` | Validated unsigned 0–8 byte hint, ignored for allocation |
| Video BlockAdditionMapping `0x41E4` / MaxBlockAdditionID | Bounded typed structure, repeated mapping masters; opaque video configuration |
| Track transforms, ContentEncodings, alternate timing/operations | Rejected on all tracks |
| Unknown-size Segment | Bounded by captured file length |
| Unknown-size Cluster, selected lacing, multiple Blocks per group | Rejected |

Mapping children reject duplicates, unknown fields, invalid widths/bounds and wrong
track types regardless of TrackType order. Subtitle/audio extensions and per-frame
BlockAdditions remain unsupported. Valid but richer metadata on unselected tracks
can therefore reject a file. This is a conservative subset, not universal MKV/WebM
validation. [MP4](MP4.md) uses its own native container path.

media-core owns bounded `open_streaming` and `open_streaming_with_limits`. Discovery
walks Segment boundaries, including metadata after clusters, without fixed-head
assumptions or payload read-ahead at every distant boundary. Default Cues are bounded,
parsed after TimestampScale discovery and checked against actual Cluster boundaries.
Forward tvmatch extraction sets `skip_cues=true`: Cues payloads are opaque, not validated.
Filtered walking seek-skips unwanted payloads and bounds header read-ahead without
resetting cumulative work on filtering or seeking.

For exact allowlists and format sources, consult the local crate's
[streaming documentation](../crates/media-mkv-webm/src/streaming/README.md) and
[bounded validator](../crates/media-mkv-webm/src/streaming/bounded.rs); for image
semantics, see [PGS](../crates/media-mkv-webm/src/pgs/README.md).

## Text timing and bounds

Packet starts retain checked nanoseconds (cluster time + signed block delta, then
scale). Selected starts must be nondecreasing before millisecond flooring; equal
starts are allowed. BlockDuration wins over DefaultDuration even when zero. Checked
declared ends remain optional. Absent/zero/sub-millisecond duration uses synthetic
`start_ms + 1` only to satisfy the transcript invariant, explicitly flagged; the
matcher uses starts. No next-cue or guessed display duration is manufactured.

Timestamps stay in the SRT 00–99 hour range. UTF-8 packets must be nonblank and
control-safe: LF/tab and CRLF normalized to LF are allowed, lone CR and other
controls are rejected. Markup remains literal. This packet policy is distinct from
[SRT file line-ending compatibility](SRT_COMPATIBILITY.md).

| Resource | Canonical limit |
|---|---:|
| One metadata payload / aggregate buffered metadata | 1 MiB / 2 MiB |
| Metadata/discovery/walk element visits | 100,000 |
| Actual read bytes / read+seek operations | 64 MiB / 1,000,000 |
| Tracks / codec-name-language bytes per field | 64 / 1,024 |
| Selected text packet / packets and cues | 4,096 bytes / 4,096 |
| Aggregate selected text | 1 MiB |

Selected text is returned only after a successful complete bounded walk. Errors
never publish a valid prefix. Unselected bytes are seek-skipped, although bounded
buffer/header peeks can read prefixes. CRCs, opaque metadata and unselected payload
contents are not validated. Inputs must stay stable; length checks alone cannot
prevent same-length concurrent mutation. Blocking I/O has no timeout; these caps
are not process RSS limits. The CLI uses a separate finite [OCR profile](OCR.md).

## PGS images and completion

`media::pgs::scan_pgs(reader, track_number, StreamingLimits, PgsLimits, callback)`
uses media-core's `PgsDecoder`. Callbacks borrow one union-cropped straight-alpha
RGBA `PgsDisplay`, retaining canvas offsets, PCS nanosecond timestamp, composition
state/number, palette/window/object/crop metadata and unchanged/clear status.
Replacement or clear closes the preceding display at its PCS start; END is not a
cue-end declaration and the final end is unknown. Container durations are not
substituted for composition transitions.

`Continue` reaches `Complete` only after clean EOF and decoder `finish()`.
`Break` suppresses remaining callbacks but validates the entire stopping packet,
returning `Stopped`, not suffix validity. A malformed same-packet tail still fails;
previous callback results must not become complete evidence. Process/drop images
rather than retaining all episode rasters.

Supported: complete headerless segments, display sets across packets, exact ODS
fragmentation, palette deltas, normal `0x00`, epoch `0x80`, self-contained acquisition
`0x40`, crop/window placement, alpha compositing and retained-composition palette
updates distinct from clears. Acquisition clears decoding caches but preserves
prior-display accounting and cumulative budgets; no same-epoch canvas change or
stale resource/composition fallback. Reserved flags, missing resources, split raw
segments, implicit RLE padding and incomplete state fail. Undefined slots in an
existing palette and index 255 are transparent. Rendering assumes limited-range
BT.709 for HD crops, not source-verified color metadata.

Independent PGS defaults: 4 MiB packet/display-set/compressed object; 16 MiB pending
RLE+cached+temporary indices; 64 MiB old+new RGBA; 3840×2160 axes / 8,294,400 pixels;
64 live objects; 8 palettes/windows/composed objects; 1024 segments/display;
100,000 packets; 20,000 commits; 64 MiB cumulative selected bytes; 268,435,456 rendered
union pixels; 24-hour source times. Major allocations, including replacement peaks,
are charged first. Caller memory and allocator overhead remain separate.

Synthetic tests cover timing/LanguageIETF, mapping families, Cues, seek-skipping,
I/O failures, PGS composition/budgets and actual stop-tail poisoning. See
[regressions](REGRESSIONS.md); bounded tests are not fuzzing or conformance certification.
