# Provider SRT import

## Bounded source syntax and encoding

The provider accepts UTF-8, or explicit UTF-16LE/BE BOMs; no unmarked legacy-encoding
guesses, lossy surrogate replacement or external parser fallback. Raw input and decoded
UTF-8 each retain the 1 MiB cap. LF/CRLF/CR may be mixed. Literal `-->` permits surrounding
whitespace; fractional seconds are a 1–3 digit decimal fraction, comma or dot. Optional
`X1:n X2:n Y1:n Y2:n` placement must be complete, ordered, unsigned u32, non-reversed,
and free of extra settings. It affects rendering only, not cue text or timestamps.

Optional `source_layout` uses `provider-srt-source-layout-v1`: encoding label,
original timed-record count, bare-CR count, positioned-cue count, and `layout_sha256`.
The digest binds encoding, counts, bare-CR offsets in decoded UTF-8 (including an
initial BOM), and original record ordinal/timing-line/coordinate tuples. The receipt
still binds exact raw bytes. It is present only for UTF-16, bare CR or placement;
existing UTF-8 LF/CRLF provenance remains unchanged. Format
`srt-raw-with-source-layout-v1` takes precedence when composed with other policies.
All fields are recomputed on reopen; omission, mutation or unknown fields fail closed.
Receipt-verified all-empty UTF-16 still requires separate fallback consent, never a
successful empty transcript or automatic replacement download.

Shared line indexing uses one u32 endpoint per physical line; body caps precede
caption joins and blank-position collection. Native positive/negative, decoded-size,
maximum-newline, raw-retention and provenance-tamper regressions live in
`src/srt.rs` and `src/opensubtitles/tests/compatibility.rs`. See the
[compatibility chart](SRT_COMPATIBILITY.md) for deliberately unsupported cases.

## Interior caption paragraphs

Provider-only `provider-caption-paragraphs-v1` removes interior blank physical lines
only when a nonempty caption prefix and the next conventionally separated, consecutive
integer cue header bound the text, with no intervening timing record. Orphan prefixes,
EOF paragraphs, unindexed/fractional/gapped boundary guesses, numeric/header-like
fragments and malformed clocks remain errors. The framer retains blank lines in its
record; the caller applies normalization. All nonblank lines keep exact text/order,
with unchanged timestamps; source/cue/file bounds apply before normalization.

Optional `caption_paragraphs` provenance records original/affected cue counts, removed
blank-line count and a domain-separated original-source layout digest, recomputed on
load. Format `srt-utf8-raw-with-caption-paragraphs-v1` leaves raw bytes authoritative;
legacy policies remain unchanged when absent. Public SRT parsing stays strict.

## Frame once, validate, then derive

The public `Transcript::parse` remains strict. Provider import is a separate,
bounded interface, not a try-every-parser fallback and not a general damaged-text
salvager. Raw downloaded bytes and their verified receipt/hash remain authoritative.

## Pipeline

1. Decode only UTF-8 or BOM-marked UTF-16 with independent 1 MiB raw/decoded bounds.
   Split physical LF/CRLF/CR lines. Lex timing rows and reject forbidden
   controls/malformed clock-shaped arrows (empty fields, 4+ digit fractions,
   hours>99, minutes/seconds>59). At most 4096 timed records, including
   zero-duration and missing-text records that will not become matching evidence.
2. **Frame all records once** (`records/parser.rs`). Consume every nonblank line
   as a label, timing row, or caption. A valid standalone timing row starts a record;
   a numeric label immediately before it is structural metadata. Missing labels or
   blank separators do not require a second parser to reinterpret the document.
   The framer returns a typed per-record `RecordError::MissingText` with source
   metadata for empty records. It does not silently turn that error into a caption;
   the provider importer is the caller that chooses to omit it.
3. Apply the existing C1 placeholder policy only to classified caption lines—not
   index/timing roles guessed from blank-line counts. Validate each full caption,
   including nonempty zero-duration records, against 4096-byte/control/nonempty bounds.
4. Record and skip `MissingText` outcomes, then omit fully validated start==end
   nonempty records. Generate sequential derivative indices and stably order
   positive-duration cues if necessary. Preserve duplicates,
   equal-start ordering, retained caption text and numeric timestamp values. The
   only caption substitutions remain the explicitly recorded U+0092/U+009D policies.
5. Strictly parse the bounded canonical derivative and compare **every retained
   caption, start and end** against the framed records in stable chronological order.
   No partial/prefix success is published if any input or roundtrip check fails.

## Explicit grammar, not guesses

- Clock hours/minutes/seconds are bounded integer fields (1–6 ASCII digits), with
  fixed units and the normal 99/59/59 limits. Redundant zero padding is insignificant:
  `00:01:011,000` has the same numeric value as `00:01:11,000`.
- Fractional seconds are a decimal fraction with **1–3 digits** (tenths, hundredths,
  thousandths); comma or dot is accepted. `00:00:2,00` is 2.00 seconds, not 2 ms.
  Never left-pad as integer milliseconds, carry an overflowing field, invent duration,
  or repair a negative interval. Four-or-more-digit fractions remain errors. Arrow
  spelling is literal `-->`; surrounding spaces are optional. Noncanonical accepted
  spellings are counted in provenance.
- Explicit labels must start numerically at 1 and strictly increase. Integer parts
  fit u32; optional decimal fractions have at most 9 digits and use exact integer
  comparison, not floating point. Gaps do not invent absent captions.
- A numeric label immediately before a timing row is structural metadata even without
  a blank separator, provided it strictly increases. Unrelated numbers that are not
  followed by a timing row remain caption text. Thus `19.5` between labels 19 and 20
  is a header when a timing row follows; a dialogue number is not stripped.
- Consecutive or final unindexed timing records are permitted. Empty records with
  valid timing/framing return `MissingText`; the provider importer skips them, even
  at EOF or with zero duration. The strict public SRT API still rejects missing text.
  Arbitrary inter-block text, malformed clocks, negative durations, controls,
  overflowing/duplicate/decreasing labels and ambiguous numeric boundaries still
  fail. Skipping every record also fails; no evidence is invented.

This follows a documented SRT record grammar; it cannot prove an author's intent
when a literal standalone timing range was meant as dialogue. It is deliberately
not fuzzy OCR correction or unrestricted “best effort” parsing. Repairs are visible
and auditable rather than claimed to establish episode identity or author intent.

## Provenance and compatibility

Existing ordinary, ordering, placeholder, gapped-numbering, zero-duration and narrow
unindexed-cue provenance stays byte-compatible. `boundaries.rs` computes compatible
provenance from the same framed records without reparsing the input.

Additional layouts carry `record_framing` with policy
`provider-srt-record-framing-v2`: original record count, unindexed/missing-separator/
fractional-label/normalized-timing counts and a domain-separated source-span/label
mapping digest. Format is `srt-utf8-raw-with-record-framing-v2`. Omission, ordering
and caption replacements remain separately recorded. Counts and mappings are
recomputed from the raw body on every cache load; missing/changed/unknown metadata
fails closed without fetching a replacement.

Missing-text omission adds optional `missing_text` metadata with policy
`provider-skip-missing-text-v1`: original/skipped/retained counts and a domain-separated
per-original-record selection digest. Format becomes
`srt-utf8-raw-with-missing-text-derivative-v1`. Framing/numbering still describe all
original records; the zero-duration policy describes the nonempty records entering
zero-duration processing. Raw bytes stay untouched and every field is recomputed on
load. Older provenance without missing-text omissions remains exactly compatible.

## Composable derivative policies

- `caption-u009d-to-ufffd-v1` substitutes U+FFFD for U+009D in nonempty caption text.
  `caption-c1-placeholders-v2` also permits U+0092 with per-codepoint counts; it is
  an explicit unknown-character placeholder, not a guessed apostrophe. Ambiguous
  numeric/timing-like lines and all other forbidden controls fail.
- `provider-unindexed-cue-v1` preserves compatibility for one embedded unindexed
  timed cue surrounded by intact sequential indexed cues. More general accepted
  framing uses the record-framing policy; neither invents missing captions/times.
- `provider-gapped-cue-numbering-v1` records original count, changed-header count
  and original-index digest; labels start at 1 and increase strictly.
- `provider-skip-zero-duration-v1` records original/skipped/retained counts and a
  domain-separated ordinal/keep-mask digest. Only fully validated nonempty
  start==end records are omitted; negative timing or malformed text is not skipped.
- `provider-stable-cue-order-v1` stably sorts complete cues by start, preserving
  equal-start order and duplicates. Cue/movement counts and an original-index
  permutation digest are rechecked; ordering never changes text or timestamp values.

Each policy is independently recorded and recomputed from authoritative raw bytes.
All input bounds apply before omission; retained semantics must exactly roundtrip
through the strict public parser. A derivative with no evidence is not a reference.

## Validation

`src/opensubtitles/tests/community.rs` covers typed API errors vs caller omission, zero-duration/order/
numbering composition, exact retained duplicates/text/times, fatal errors/bounds,
and offline provenance tampering with no redownload.

Synthetic `src/opensubtitles/tests/framing.rs` covers 64 combinations of independent defects, the
combined fractional-label/padded-clock/unindexed/zero-duration case, exact semantic
preservation, clock bounds/precision, numeric caption ambiguity, C1 controls,
consecutive/EOF records and offline metadata tampering. Existing strict public,
legacy recovery, input-cap and no-redownload tests remain in the normal suite.

[src/opensubtitles/tests/paragraphs.rs](../src/opensubtitles/tests/paragraphs.rs)
checks anchored blank-line normalization, exact nonblank text/times, ambiguity,
policy composition and tampered offline reopening. The 48-case public and 36-case
provider layout corpora, receipt/encoding/cap tests are mapped in
[REGRESSIONS.md](REGRESSIONS.md).
