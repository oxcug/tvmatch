# Native MP4 subtitle evidence

The CLI enumerates `.mkv`, `.mp4` and `.m4v` case-insensitively, nonrecursively,
up to 32 files total. Container headers determine the reader; filenames never supply
episode identity. No FFmpeg, external OCR or media upload is used. The optional
`media` feature links media-core's `media-isobmff` path dependency alongside MKV.
Its `demux` API owns container boxes, metadata, sample tables, edit timing
and bounded payload reads. `src/media/mp4.rs` only adapts metadata and decodes `tx3g`;
there is no separate MP4 container parser in tvmatch.

## Supported subset

- Ordinary **non-fragmented ISO BMFF MP4**, with `moov` before or after `mdat`.
  MP4-compatible `.m4v` files use the same path; encrypted M4V is not supported.
- Embedded **`tx3g` timed text**: length-prefixed UTF-8, or UTF-16 with a big/little
  endian BOM. Decode strictly; normalize CRLF/CR line endings. Empty samples are
  clears, not invented captions. Structurally valid `styl` and `tbox` modifiers are
  accepted; presentation styling is not matching evidence.
- Version 0/1 movie/media/track headers; `stts`, `stsc`, fixed/variable `stsz`, and
  `stco`/`co64` sample mapping. Validate counts, media duration, description indices,
  contained `mdat` ranges and nonoverlapping subtitle samples before reading them.
- No edit list, or one unit-rate media edit optionally preceded by an empty delay.
  Apply the declared trim/delay and clip visibility at its boundaries. Integer
  rational arithmetic maps track/movie timescales into nanoseconds; transcript
  times floor to milliseconds. No floating-point time guesses or offset shopping.
- Track IDs, enabled flag, packed `mdhd` language and optional extended `elng`
  language. CLI uses enabled, non-forced **declared-English** tracks only. No default
  track favoritism, language inference, or pooling across tracks. All/some-forced
  flags conservatively exclude the track; unexpected per-sample forced modifiers
  reject the extraction. Some muxers disable optional tracks in their metadata;
  tvmatch does not silently enable them.

Text tracks are read completely within bounds before any matching result is
published, including validating late/blank/edit-excluded samples. They do not load
or run the OCR model. MKV PGS retains its independent 64→128→192 image sampling.
The same distinctive, separated, ordered-anchor matcher is used for both containers.

## Explicitly unsupported

Fragmented `moof`/`mvex` MP4, encrypted/protected subtitle descriptions or auxiliary
sample tables, external media references, composition-offset (`ctts`) subtitle
tracks, multiple sample descriptions, unsupported edit sequences/rates, and other
subtitle formats/modifiers fail rather than being interpreted as plain text.
Only the documented sample-table subset is admitted. Video/audio payloads and
unneeded opaque metadata are skipped, **not fully decoded or validated**.

No MP4 `wvtt` WebVTT, `stpp` TTML, legacy QuickTime text, CEA captions, subtitle
sidecars or burned-in-video text extraction. A video-only MP4 provides no
subtitle evidence. Unicode text decoding does not imply multilingual CLI matching:
reference acquisition still selects English, and the tokenizer/model have not been
validated as general multilingual episode identification.

## Bounds

- `moov` ≤8 MiB; ≤16 MiB cumulative actual reads per MP4 pass (plus a fixed 4-byte
  container sniff); no allocation proportional to video size.
- ≤64 total tracks, 100,000 container boxes and 100,000 samples/table entries,
  1024 `mdat` extents. A separate cumulative 100,000-box budget covers codec
  configuration/modifier framing through media-core's shared box walker.
- Selected sample ≤64 KiB including text/modifiers; caption ≤4096 UTF-8 bytes;
  aggregate decoded text ≤1 MiB, including skipped text; retained cues ≤4096.
- The 100-hour transcript time bound and matcher work limits remain unchanged.
- Size/range arithmetic is checked. Payloads outside media data, overlapping sample
  ranges, incomplete timing/chunk coverage, invalid Unicode, controls or a corrupt
  final sample never publish an earlier valid prefix as a successful extraction.

Rename previews preserve the source extension and its case. Synthetic MP4/M4V
swaps exercise consent, no-overwrite behavior, temporary names and byte preservation.

## Validation

[tests/mp4_demux.rs](../tests/mp4_demux.rs) bridges eight unit oracles from the
local crate's normal demux source module. Tests cover opaque audio/video/unknown codecs,
selection, sample tables/ranges, edits, payload limits, terminal read/EOF/seek failures
and guarded seek-skipping. Production adapter tests separately use the actual path
dependency. The workspace test command also runs both complete crate suites.

[src/media/mp4/tests.rs](../src/media/mp4/tests.rs) generates synthetic fixtures for
timing/Unicode, front/back `moov`, fixed/variable sizes, 32/64-bit offsets, edits,
languages/flags, invalid data and unchanged evidence matching. A guarded reader
rejects simulated video payload reads. Rename tests cover mixed extensions and swaps.
**Real-file MP4 interoperability is not established.** No universal compatibility
or whole-container validation claim follows from synthetic success.

See [contributing](CONTRIBUTING.md) for workspace and registry-cache prerequisites and commands,
and [regressions](REGRESSIONS.md) for source-named coverage.

Format references: [3GPP Timed Text overview](https://github.com/gpac/gpac/wiki/TTXT-Format-Documentation),
[Apple text display flags](https://developer.apple.com/documentation/coremedia/text-display-flags),
and ISO BMFF sample/edit tables. The local bounded API is documented in
[DEMUX.md](../crates/media-isobmff/DEMUX.md).
