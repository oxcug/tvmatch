# Native PGS OCR

The default `ocr` feature implies `media` and uses pinned ocrs 0.13.0, RTen 0.26.0
and rten-imageproc 0.26.0. The CLI embeds the unmodified recognizer; no detector is
bundled and no consumer model setup or runtime download is needed. Text subtitle
tracks do not initialize OCR. No external OCR process, FFmpeg, Tesseract, Python,
GPU runtime, media upload or audio/video decoder is used.

The model is a separate licensed asset even when embedded in an executable. Preserve
[its attribution, immutable source, size/hash and CC-BY-SA-4.0 obligations](../assets/ocr/NOTICE.md).
Engine code licensing does not license weights or reference dialogue; see
[publication requirements](PUBLICATION.md).

## Library API and timing

[media::ocr](../src/media/ocr.rs) exposes:

- `LocalOcr::bundled()`: embedded recognition-only engine.
- `LocalOcr::load(optional_detection_path, recognition_path)`: trusted compatible
  local RTen models. A detector explicitly selects detector geometry/reading order;
  absence selects horizontal subtitle layout, with no automatic mode fallback.
- `extract_pgs_ocr`: explicit TrackNumber and optional maximum visible-frame sample.
- `extract_pgs_ocr_with_limits`: the same extraction with caller-selected
  media-core `StreamingLimits`.
- `extract_pgs_ocr_progressive_with_limits`: ordered sample points and a callback
  that reassesses all accumulated cues using one forward scan and collector.

`PgsOcr` returns a transcript, one-to-one raw-nanosecond `OcrTimestamp`s, track/scan/
completion metadata, attempted/empty/unchanged frame counts and cumulative input
pixels. The forward OCR path always skips Cues; other caller-provided container
limits are honored without changing PGS/OCR caps. These are library APIs, not CLI
model/extraction options. Direct UTF-8 extraction remains a separate API.

Changed effective rasters are recognized once. Unchanged commits extend the current
interval but are not re-recognized. Clear/replacement closes the active cue even if
the next OCR result is blank; repeated text across changed rasters is not merged.
The final end at EOF/stop is unknown. Starts floor to milliseconds; absent or
nonpositive rounded ends use an explicitly marked synthetic `start_ms + 1` solely
for the SRT duration invariant. Raw equal/sub-millisecond ends remain evidence.
END packets are not cue-end declarations. SRT alone loses these distinctions;
there is no guessed multi-second duration or reference-synchronized timing.

## CLI sampling and completion

The CLI starts at **64 visible frames**, widening to **128 then 192** only for
unresolved results and announcing each transition. Blank OCR counts toward sample
points; unchanged rasters do not. One demuxer/decoder/collector continues without
recognizing the prefix again. All accumulated cues are rematched; later repetitions
can remove support. Scores are not pooled and thresholds do not change.

The first `Identified` result stops. At the hard sample cap or earlier clean EOF,
perform the final assessment without forcing a match. Callback results are provisional
until the whole packet validates. A malformed tail in the stopping packet discards
the extraction. A deliberately unread suffix remains unvalidated.

CLI OCR uses cumulative 1,000,000-element / 4,000,000-I/O / 64 MiB-read container
limits. Probing and UTF-8 extraction retain canonical 100,000-element / 1,000,000-I/O
limits and may fail before OCR. The low-level default extraction function retains
canonical limits; caller-selected limits do not reset discovery/walk work, skip
validation or guarantee that arbitrary full tracks fit. `Complete` means clean
selected-stream EOF in the supported subset, not validated audio/video or CRCs.

## Preprocessing and layout

PGS supplies straight-alpha RGBA union crops. OCR composites onto black, converts
RGB using integer grayscale coefficients 77/150/29, inverts to dark glyphs on white
and adds a fixed 16-pixel white margin. Antialiasing is preserved. No recognition
thresholding, upscaling, morphological repair, dictionary/LLM correction or
reference-selected preprocessing is performed.

Crops up to 512 pixels high retain ordinary preprocessing. Taller source crops
within 3840×2160 may compact **fully white vertical gaps**: keep every foreground
row in order and gaps up to 12 pixels; larger gaps become 13 pixels. No foreground
pixel is scaled, thresholded or removed. Dense/tall content that still cannot fit
fails rather than dropping text. Full original padded source pixels count as work,
not the smaller packed raster.

Recognition-only horizontal layout groups non-white rows, merging gaps up to 12
rows to retain detached accents/punctuation. Each group has a checked clipped
4-pixel recognition margin and a full-line rectangle. All foreground components
remain; there is no low-ink filter. Output is top-to-bottom. Overlapping speakers/
columns can merge; rotated text, large detached accents and unfamiliar spacing
are not general-layout support. Nonempty visible crops without ink fail explicitly.
The model's default alphabet is retained. Names, contractions, italics and SDH can
be misrecognized; no calibrated OCR confidence or character-error rate is claimed.

## Bounds and model trust

| Resource | Limit |
|---|---:|
| Each local regular model file, before parsing | 16 MiB |
| Source crop axes | 3840×2160 |
| Ordinary/compacted recognition crop | 3840×512 |
| Padded recognition input | 2 Mi pixels |
| Cumulative full-source padded work / visible attempts | 128 Mi pixels / 4,096 |
| Detected words / lines | 256 / 16 |
| Nonempty cue / aggregate text | 4,096 bytes / 1 MiB |

Geometry/work is charged before preprocessing/inference, text before retention.
Processing is serial, one engine and one crop at a time, without an episode raster
queue. RTen uses a CPU-count-bounded pool; `RTEN_NUM_THREADS` can reduce it for a
process. Internal engine allocations are not controlled by application caps.
A byte-limited malicious model can cause disproportionate work/allocation: only
trusted compatible graphs are supported, not arbitrary models from media files.
No exact RSS, CPU-time deadline or clean-machine portability guarantee is supplied.

[Synthetic regressions](REGRESSIONS.md) cover glyph preservation, ordinary-input
stability, caps, raw timing, blank/dedup behavior, progressive sampling and terminal
failures. Model/layout observations are not broad OCR or episode-accuracy evidence.
