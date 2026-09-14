# Built-in bounded streaming

`open_streaming(reader)` is the canonical **Read + Seek** API, with safe default resource budgets. `open_streaming_with_limits(reader, StreamingLimits { .. })` adjusts budgets, not safety checks. Both use the same EBML helpers, metadata parsers and frame walker. The slice-based `Demuxer::parse`/`Frames` APIs are unchanged apart from rejecting Cue timestamp multiplication overflow; the streaming hardening does not claim to harden those separate slice APIs.

## Discovery and supported subset

Open requires EBML at the file start, discovers the following Segment across optional Void/CRC32 elements, then walks **all Segment child boundaries**, seeking past clusters and opaque metadata. Info/Tracks may occur after clusters; no 1 MiB head assumption and no SeekHead-dependent jumps. One Segment occupying the rest of the file is supported, known or unknown sized. All child elements (including Clusters) must have known sizes. Files must fit both `usize` and `i64` offsets. Truncated or overflowing headers and declared child extents outside their parent/file are errors.

Discovery uses the unbuffered `BudgetReader` with existing EBML header/metadata helpers: distant Cluster boundary reads no longer pull in a 64 KiB payload prefix. Only after discovery is that same reader wrapped in a 64 KiB `BufReader` for frame walking, preserving cumulative byte/I/O budgets. The walker seeks to its recorded absolute position; no buffered bytes are discarded during the transition.

Metadata is a deliberately conservative allowlist, implemented in `bounded.rs::validate_metadata` over the existing EBML Reader:

- EBML version/read-version 1, ID width up to 4, size width up to 8; Matroska/WebM DocType version/read-version up to 4.
- Info: TimestampScale (nonzero), Duration, DateUTC, Title, MuxingApp, WritingApp, SegmentUID.
- Tracks: TrackEntry with number/UID/type, enabled/default/forced/lacing flags, DefaultDuration, name/language/LanguageIETF, CodecID/private/name/delay, SeekPreRoll, basic Video/Audio projection fields already handled by the shared parser.
- Video-only `BlockAdditionMapping` (`0x41E4`) is repeatable. Its `BlockAddIDValue` (`0x41F0`, uint >=2), `BlockAddIDName` (`0x41A4`, printable ASCII), `BlockAddIDType` (`0x41E7`, uint/default0) and opaque `BlockAddIDExtraData` (`0x41ED`) children are structurally checked: known-size bounds, singleton children, integer widths0..8 and the existing cumulative metadata/element budgets. Unknown children and wrong parents still fail. TrackType may appear before or after mappings; only actual numeric type1 (video) can carry these ignored format extensions. `MaxBlockAdditionID` (`0x55EE`) is width-checked; nonzero is likewise video-only, zero has no extension effect. Mapping metadata is **not projected into TrackInfo or interpreted**; the streaming subset is not a full video-decoder configuration API. Per-frame BlockAdditions remain unsupported. Subtitle/audio mapping extensions still fail rather than silently alter decoded subtitle evidence.
- Historical TrackEntry MinCache (`0x6DE7`) is accepted for compatibility only. The [official Matroska schema](https://raw.githubusercontent.com/ietf-wg-cellar/matroska-specification/master/ebml_matroska.xml) declares unsigned integer, default 0, maxOccurs 1, minver/maxver 0 (obsolete). It is a minimum playback frame-cache hint, not a transform or timestamp adjustment. [RFC 8794 sections 6.1 and 7.2](https://www.rfc-editor.org/rfc/rfc8794.html#section-7.2) permit 0–8 payload bytes: empty uses the declared default 0; all uint64 values are valid. The existing `Reader::read_uint` validates the width; duplicates, 9+ bytes and wrong parents are rejected. The hint is not projected or used to allocate a decoding cache. MaxCache is not newly admitted.
- Unknown metadata fields, track ContentEncodings (compression/encryption/header stripping), TrackTimestampScale, TrackOffset, TrackOperation/translation and other unsupported extensions are **rejected on all tracks**, even unselected ones. This will reject some valid production files. Unsupported metadata/Segment fields return `Error::UnsupportedElement { parent, element }` with hexadecimal IDs in Display (new enum variant; exhaustive downstream error matches may need updating). CodecDelay/SeekPreRoll are surfaced without adjustment; consumers must apply or reject them. Metadata strings must be UTF-8 without controls, not silently truncated at NUL. Duplicate nonrepeatable fields and malformed metadata suffixes are rejected.
- Cues are populated **by default**, after final TimestampScale discovery, with checked time multiplication and target offsets. Every target must match a discovered Cluster boundary and declared track. CueTime, CueTrack and CueClusterPosition are required; optional CueRelativePosition/Duration/BlockNumber are skipped. Unsupported Cue extensions are errors. `skip_cues: true` is an explicit forward-only opt-out: the Cues payload is then opaque, not validated. SeekHead is always opaque; it cannot cause an attacker-sized tail allocation.
- Cluster Timestamp must occur once before any blocks. SimpleBlock and single-Block BlockGroup are supported. BlockDuration and ReferenceBlock metadata are preserved; no duration is invented. Selected lacing, short block headers, multiple Blocks, malformed durations/references, negative/overflowing timestamps and unknown Cluster/BlockGroup extensions are errors. Unselected block data (including lacing) is not interpreted; large payloads are seek-skipped. Position/PrevSize, Void/CRC32 are skipped.

This is not a complete Matroska validator or decoder: CRCs, attachments, tags, chapters, SeekHead and unselected codec payloads are not verified. Buffered read-ahead (64 KiB) and inner block peeks can read a prefix of unselected payloads. Selected output may already have been written when a later BlockGroup error occurs: callers **must discard partial output and stop using the stream on any error**, never treat it as clean EOF. The input must remain stable; length changes are checked at walk end, but same-length concurrent modifications are not prevented. Blocking filesystem I/O has no timeout.

## Default cumulative budgets

| Resource | Default |
|---|---:|
| One buffered metadata element (EBML/Info/Tracks/Cues) | 1 MiB |
| Total buffered metadata payloads | 2 MiB |
| Charged element visits: metadata validation + open scan + frame walk | 100,000 |
| Underlying bytes read, including repeated read-ahead | 64 MiB |
| Underlying read/seek operations | 1,000,000 |

Metadata bounds are checked **before allocation**. Parser expansion and cluster/Cue bookkeeping are additionally bounded by charged-element counts. The shared metadata parser revisits the already validated headers once, so actual header parsing work can exceed the charged count by that bounded validation pass. Budgets never reset on seek. `next_frame_into` has no per-frame allocation and lets the caller impose a smaller write cap. `next_frame` retains its owned-Vec compatibility shape; its data length is bounded by the cumulative read budget (including at most a buffered prefix); Vec capacity may overallocate. Callers needing long video transcodes must deliberately set suitable finite limits; defaults are tuned for bounded extraction, not unrestricted movie decoding.

## Compatibility and validation

`open_streaming`, `set_track_filter`, `next_frame_into`, `next_frame`,
`seek_to_time` and `seek_to_byte` provide the streaming interface. Default Cues
select preceding keyframes; absent qualifying Cues rewind to the first Cluster.
Richer metadata, selected lacing or larger workloads require explicit supported
extensions/configuration, not an unsafe fallback.

The mapping schema is from the [official Matroska schema](https://raw.githubusercontent.com/ietf-wg-cellar/matroska-specification/master/ebml_matroska.xml).
Local `bounded/tests.rs` regressions cover repeated mappings, scalar widths, ordering,
track-type rejection, duplicate/unknown/wrong-parent fields, malformed boundaries,
cumulative caps and exact selected subtitle payload/timestamp preservation.

[Synthetic integration tests](../../../../tests/streaming_bounded.rs) cover default
and configured entry points, MinCache unsigned widths/duplicates, actionable field
IDs, distant clusters without open-scan read amplification, tail Cues/final-scale
seeking, malformed metadata, lacing/multiple Blocks, overflow, transforms, budgets
and skipped large payloads. Run the complete crate and app suites with the root
[workspace test command](../../../../docs/CONTRIBUTING.md#checks), including registry
cross-parser oracles. Ignored local probes are not part of normal validation.

## Filtered frame-walk read amplification refinement

Filtered walks cap underlying header refills to64 bytes, coalescing adjacent
EBML/block headers instead of pulling64KiB from each unwanted video/audio
packet. Selected payload copying temporarily lifts the cap, restoring it even
on a returned copy error; ordinary unfiltered consumers retain64KiB buffering.
Unselected payload skipping uses `BufReader::seek_relative`: advance within
already-paid buffer bytes, otherwise seek, never refill just because the skip
is smaller than64KiB. Parent extents remain checked and EOF length checks remain.
The same reader/byte/operation budgets survive filter changes and all seeks.
Cumulative limits and Cues/parser validation policy are unchanged by filtering.

The synthetic read-amplification regression interleaves 100 video/audio packets
with selected PGS-framed payloads and checks exact bytes/timestamps and clean EOF.
Additional tests cover filter changes, seek-back, injected read errors, selected
sink failures, truncation and cumulative read/operation budgets. These are bounded
mechanics checks, not a general all-tracks performance benchmark.
