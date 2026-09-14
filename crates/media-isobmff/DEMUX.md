# Bounded non-fragmented MP4 demuxing

`media_isobmff::demux` is an additive `Read + Seek` API. Existing `parse` video/audio
readers and the encoder/HEIF/fragment APIs are unchanged. The new path avoids reading
a whole movie or its unselected payloads and validates sample expansion against
finite budgets. It deliberately supports a narrower table/edit subset than `parse`.

## Ownership

- `Demuxer::open` / `with_limits`: scan top-level boxes, retain bounded `moov`
  metadata, expose all track handlers, IDs, enabled/language declarations and opaque
  sample descriptions. Opening is **not** selected-table or payload validation.
- `into_samples(track_id)`: validate the selected table completely, including counts,
  media duration, chunk/description mapping, contained `mdat` extents and nonoverlap.
  Unsupported container constructs on another track do not prevent selection.
- `SampleReader::next_sample`: return opaque bytes, decode ticks/duration and optional
  edit-mapped movie nanoseconds. A read/timing failure is terminal. Consumers stopping
  early have not validated the remaining payloads. Exhaustion is `Ok(None)` only.
- `BoxBudget::read`: reusable child-box framing for consumer-owned codec configuration
  and sample modifier boxes, with its own cumulative 100,000-box budget.

There is **no subtitle decoding or evidence policy** here. In particular, `tx3g`
length/BOM/text/style/forced semantics, English eligibility, caption limits and
matching belong to tvmatch. Generic sample descriptions expose the format, data
reference index and bytes following the eight-byte SampleEntry header. This module
neither decodes codecs nor certifies that opaque payloads are unencrypted; consumers
must interpret/reject protected descriptions and formats themselves.

## Supported container subset

- MP4-compatible `ftyp`, one `moov` before or after media, 32-bit/extended/parent-end
  box sizes, version 0/1 `mvhd`, `mdhd`, `tkhd` and `elst`.
- One description and one self-contained `url ` data reference for a selected track.
  No external reference is opened. All handler types and opaque codec IDs are exposed.
- `stts`, `stsc`, fixed/variable `stsz`, `stco` or `co64`. `stss` and `free` are allowed
  but this API does not expose keyframe flags. Selected `ctts`, compact/protected or
  other auxiliary tables are unsupported, not guessed.
- No edit, or one positive-duration unit-rate media edit optionally after an empty
  delay. Integer-rational conversion clips samples to the edit. An excluded sample
  still returns its bytes with `presentation_ns: None`; codec validation is caller-owned.
- Fragmented `moof`/`mvex` files reject; this is not the fragmented writer's read-side
  equivalent or a universal MP4/QuickTime/HEIF reader.

## Bounds and validation

Fixed limits: 8 MiB `moov`, 64 tracks, 100,000 structural boxes (including selected
reparse), 100,000 samples/table entries and 1,024 `mdat` extents. Memory scales with
these bounded metadata/tables and one selected payload, never file length.
`Limits` defaults to 16 MiB cumulative charged reads and 16 MiB per sample. Consumers
may explicitly choose other read/sample budgets; tvmatch keeps 16 MiB reads and
lowers samples to 64 KiB. `bytes_read()` reports charged requested bytes (equal to
bytes read after successful operations). There is no wall-clock IO timeout.

Opening reads headers and metadata, not unselected media bytes. Table validation is
selected-track-local; shared/overlapping payloads across different tracks are not
certified. Sample bytes remain uninterpreted, including zero-size samples. A codec
consumer must validate the complete selected stream before claiming full extraction.

Unit tests in `src/demux/tests.rs` cover opaque video/audio/unknown formats, selection,
front/back metadata, 32/64-bit offsets, edits, excluded payloads, versions, bounds,
terminal errors, external references and a read guard over unselected video data.
Tvmatch's adapter tests additionally cover actual timed-text semantics, fixed-size
samples, late malformed captions and unchanged evidence matching. These are synthetic
fixtures; no real-file interoperability or broad codec coverage is claimed.

Run the crate suites from the repository root with
`cargo test --offline --locked --workspace --all-features`; the Cargo registry cache
must already contain the locked dependencies. The root
[MP4 source bridge](../../tests/mp4_demux.rs) also runs this unit module in app-only
tests, while adapter tests separately use the compiled local dependency. All source
paths resolve within this checkout. Platform and interoperability limits remain in
[the regression map](../../docs/REGRESSIONS.md).
