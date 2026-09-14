# Bounded Matroska PGS images

`pgs::PgsDecoder::new(PgsLimits)` accepts **headerless** Matroska S_HDMV/PGS packets through `push_packet(timestamp_ns, bytes, callback)`. Framing is type:u8 + length:u16be + exact body, not SUP's PG/timestamp wrapper. Complete segments may span a display set across packets; individual split segment bodies are explicitly rejected. `finish()` checks clean EOF. All errors poison the instance. A callback before an eventual error is partial evidence, never whole-stream success.

This std-only implementation originates in media-core. Container parsing uses the
crate's streaming API; OCR and evidence policy remain caller-owned. No external
process, video decoding or runtime downloads are involved.

## Supported state and rendering

- PCS begins each display set; END (empty body) commits, not clears. Nested PCS/orphan segments, partial display/fragment EOF, unknown segments/reserved flags and unsupported states are explicit errors.
- Epoch state0x80 resets object/palette/window caches. Normal state0x00 can reuse them. Acquisition state0x40 now supports a self-contained refresh **within the same epoch**: release object/palette/window caches and require fresh referenced resources, but reject canvas changes without a true epoch start. Retain the previous rendered display only for interval/dedup and old+new raster accounting; cumulative work budgets do not reset. It is not normal cached-resource reuse or permission to discard an incomplete display set. Frame-rate byte is preserved, not used to invent a clock or decode duration.
- ODS validates first/continuation/last flags, ID/version matching, declared u24 length INCLUDING4 dimension bytes, exact completion and per-object/live aggregate budgets. Multiple pending objects are supported. Completed replacements are installed only after exact bounded RLE decoding; old+pending+temporary state is charged. No cache eviction changes semantics.
- RLE supports literal, short/14-bit transparent and colored runs and explicit EOL. This initial subset requires every row to be full followed by EOL; early EOL/implicit padding, overlong runs, zero-length runs, missing EOL, extra/truncated data are errors, never padded or clipped. This policy is not a claim all valid PGS encoders require full explicit rows.
- PDS retains original Y/Cr/Cb/alpha slots and merges per-entry updates for a palette ID. Duplicate slots in one PDS and bad lengths are rejected. Within a declared palette, undefined entries are transparent. Index255 is always transparent, even if declared otherwise. Missing whole selected palette ID remains an error.
- Palette-update flag0x80 with zero PCS objects reuses only the preceding successfully committed same-epoch composition (otherwise missing-state error). New palette ID/time are honored. Flag0 with zero objects is an explicit clear; it does not require an irrelevant palette. Zero-object epoch/acquisition palette reuse is rejected. Retained composition is changed only by a successful END.
- WDS replaces the window list. Window IDs must be unique and rectangles nonempty/in-canvas. Composition references require existing objects/windows. Source crops must be nonempty and inside the object. Crop x/y selects source pixels; PCS x/y positions that crop. WDS/canvas clips the result. Multiple objects are painted in declared order with deterministic straight-alpha source-over.
- Output is cropped union RGBA8 with canvas-space x/y and original canvas size; wholly transparent/clipped-out output is an effective clear. Object metadata includes forced/crop/window fields. No full video raster or episode-sized list is built.
- **Color assumption:** studio-range BT.709 fixed-point conversion for this HD subtitle visualization path. This is not source-verified matrix metadata or exact color conformance. The associated video controls the actual palette matrix; raw slots remain in decoder state. Alpha must be deliberately composited by OCR consumers. WhiteY235/blackY16 neutral chroma and clamp/alpha cases are tested.
- Monotonic packet timestamps are checked at original nanoseconds (equal accepted). A committed display starts at its PCS packet timestamp even if ODS/END arrive in later packets. Each emitted event closes the preceding commit at that start; END's packet time and BlockDuration are not substituted as cue ends. EOF leaves the final end unknown, with no synthetic +5seconds. Every commit is emitted with previous timestamp and `unchanged` raster/canvas flag; downstream may deduplicate while retaining event provenance.

## Default bounds (engineering subset, not format maxima)

| Resource | Limit |
|---|---:|
| Packet / display-set bytes | 4MiB each |
| Individual compressed object | 4MiB |
| Pending declared RLE + cached indices + temporary new indices | 16MiB |
| Previous + new RGBA buffers together | 64MiB |
| Canvas/object axes; individual pixels | 3840x2160; 8,294,400 |
| Live object IDs / palette IDs / WDS windows / composed objects | 64 / 8 / 8 / 8 |
| Segments per display set | 1024 |
| Total packets / END commits | 100,000 / 20,000 |
| Total selected packet bytes | 64MiB |
| Total cropped pixels rendered (includes duplicate commits) | 268,435,456 |
| Maximum source timestamp | 24hours in ns |

Bounds precede reserve/extend/index decode/raster allocation. Object accounting includes declared reserved fragment storage even when only part is filled, and old cached indices during replacement. RGBA includes old+new during dedup; no unbounded hashing cache. Fixed palette arrays/count-bounded metadata are separate from object-byte accounting. Rust allocator overhead/capacity rounding, the caller's input/callback allocations and container buffers are not represented as byte-exact RSS. Render work is also bounded by composition count (up to8 traversals per charged union pixel). Limits remain separate from container read/I/O/element budgets and tvmatch's4KiB text limit. No wall-clock deadline or fuzzing/security certification.

## Evidence and sources

Synthetic integration tests in [tests/pgs_bounded.rs](../../../../tests/pgs_bounded.rs)
exercise the compiled local dependency; no copyrighted media fixtures are bundled.

Implementation-semantics research/approval, not a normative Blu-ray certification:

- Headerless framing, PCS/ODS fields and RLE forms: published libpgs0.6.0 source https://docs.rs/crate/libpgs/0.6.0/source/src/pgs/segment.rs and payload.rs / rle.rs in that directory.
- Retained palettes/composition, source-crop placement, WDS and normal/epoch handling: inspected https://raw.githubusercontent.com/OxideAV/oxideav-sub-image/master/src/pgs.rs . Its acquisition behavior retains caches. Our now-supported acquisition subset instead requires self-contained resource refresh, so missing fresh resources cannot silently use stale images.
- Reserved transparency: libbluray overlay API documentation (src/libbluray/decoders/overlay.h), https://www.mail-archive.com/libbluray-devel%40videolan.org/msg03227.html explicitly says entry0xff always transparent and color matrix follows associated video.
- Undefined palette slots and256th entry transparent: BDSup2SubPlusPlus Color_Palettes documentation https://raw.githubusercontent.com/amichaelt/BDSup2SubPlusPlus/master/src/help.htm . A missing whole palette still fails.

- Acquisition comparison: [FFmpeg n8.0 pgssubdec.c](https://raw.githubusercontent.com/FFmpeg/FFmpeg/n8.0/libavcodec/pgssubdec.c) documents that acquisition/epoch states can release previous objects and palettes and flushes them for nonnormal states. This was read as behavioral evidence, not copied or executed. We still reject reserved states/lower bits and require WDS resources for our clipping renderer.

Four local `pgs/tests.rs` regressions cover cold/repeated acquisition,
epoch→acquisition→normal reuse, exact pixels/intervals, missing refreshed resources,
canvas changes, incomplete prior sets, poisoned tails and cumulative budgets.
The root integration suite also covers Matroska→PGS acquisition. Run workspace tests
as described in [contributing](../../../../docs/CONTRIBUTING.md#checks). Stopping
samples do not validate unread suffixes or establish whole-episode identity.

No source bodies were copied. These are evidence references, not automatic runtime fetches or transitive licenses.
