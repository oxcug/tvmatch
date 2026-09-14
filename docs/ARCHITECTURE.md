# Architecture and evidence rules

## Boundaries

- [Library core](../src/lib.rs): labeled references, exact dialogue shingles,
  bounded alignment and typed outcomes. The minimal library uses only `std`.
- [SRT](../src/srt.rs): checked transcripts and bounded text/byte parsing.
- [Media adapter](../src/media.rs): native container evidence and raw timestamps.
  MKV streaming/PGS and MP4 container parsing live in the local media-core-derived crates under `crates/`;
  tvmatch owns `tx3g` decoding, OCR policy and matching, not a second container parser.
- [OpenSubtitles](../src/opensubtitles.rs): independent catalog selection, immutable
  raw receipts, recomputed derivative provenance and coordinated private caching.
- [Folder runner](../src/folder.rs): eligible-track attempts and progressive OCR.
- [Rename planner](../src/rename.rs): same-run preview, consent, source revalidation
  and atomic no-replace moves. This is not a transactional media editor.

No filename or OCR summary supplies canonical identity. Provider and container
labels remain declarations, not verified language, rights or audio content. A
mismuxed or maliciously attached subtitle track can match unrelated video.
Metadata names recognized evidence; it cannot supply missing reference dialogue.

## Library example

```rust
use tvmatch::{Index, MatchOutcome, Reference, ReferenceId, srt::Transcript};

fn identify(reference_srt: &str, query_srt: &str)
    -> Result<MatchOutcome, Box<dyn std::error::Error>>
{
    let reference = Reference::new(
        ReferenceId::new("local", "episode-edition-v1")?,
        "User-supplied display label",
        "User-supplied origin and rights assertion, pack v1",
        Transcript::parse(reference_srt)?,
    )?;
    let index = Index::build(vec![reference])?;
    Ok(index.match_query(&Transcript::parse(query_srt)?)?)
}
```

`Candidate` records expose the supplied ID/display name/provenance, timestamp
pairs, distinct shingle support, all-query-cue coverage, offset/spread, spans and
rejection reasons. `Identified` includes weaker competitors; `Ambiguous` and
`Unknown` retain available candidates and reasons. Candidates lacking a usable
two-shingle cue anchor are omitted. `Index::stats()` reports distinctive/suppressed
shingles. Parse, metadata and resource failures are errors, not truncated matches
or silently converted `Unknown` results.

The [original synthetic fixtures](../fixtures/README.md) provide labeled local
references and queries for library experiments; they are not a real-TV catalog.

## Deterministic matching

1. Split on non-alphanumeric Unicode characters and lowercase. Build exact
   **three-word shingles within each caption**, across its text lines but never
   across caption boundaries.
2. Index shingles by reference ID and cue time. Discard shingles repeated within
   a reference or found in more than one reference; discard repeated query shingles
   too. Each retained shingle has one reference posting. Adding references can
   remove distinctive evidence. Identical references under different IDs yield
   `Unknown`, not an arbitrary winner.
3. A cue pair needs **two distinct retained shingles**. Overlapping shingles in one
   caption still contribute only one anchor.
4. Find the longest ordered anchor chain separated by **at least 8 seconds in both
   timelines**, with reference-minus-query offset spread **at most 2 seconds**.
   Search windows start at each observed offset, not fixed bins. Within a window,
   dynamic programming maximizes anchor count then shingle support with deterministic
   ties. This tolerates constant offsets and limited jitter, not rate changes.
5. Require **three separated anchors**, at least six shingles spanning at least
   16 seconds in both timelines. Score is the anchor count, not a probability.
   Coverage is selected anchors / all query cues; it is reported, not an acceptance
   threshold. A matching excerpt can identify despite unrelated query portions;
   this does not classify entire or multi-episode files.
6. Insufficient support gives `Unknown`. Another qualifying candidate within one
   anchor **or** at least 80% of the best support gives `Ambiguous`; otherwise return
   `Identified`. Namespace/ID lexicographic tie ordering never resolves qualifying
   competitors into identification.

These are engineering defaults, not empirically calibrated thresholds. Shared
content absent from the reference pack cannot be suppressed as shared. Synthetic
or adversarial content containing enough matching regions can satisfy these rules;
there is no global uniqueness or semantic-verification claim.

## Resource limits

| Library resource | Limit |
|---|---:|
| Raw SRT input / decoded UTF-8, each | 1,048,576 bytes |
| Captions per transcript | 4,096 |
| Caption text, including joined newlines | 4,096 bytes |
| Normalized words per transcript | 32,768 |
| Normalized word | 128 bytes |
| References per ordinary index | 32 |
| Distinct indexed keys before suppression | 100,000 |
| Candidate cue pairs per reference / per query | 128 / 512 |
| Reference namespace / ID value | 64 / 256 bytes |
| Display name / provenance, each | 1,024 bytes |

Pair limits apply before weak-anchor filtering. Long, densely matching queries may
exhaust work limits; shorter excerpts reduce work without changing evidence rules.
The ordinary alignment predecessor bound is `512 * 128²` comparisons. The folder
uses finite `EPISODE_PAIR_LIMITS` of 1024/reference and 4096/query and additive season
index admission up to the 1000-reference metadata guard. Global suppression and
actual word/shingle/work bounds still apply; large catalogs can fail explicitly.

Container, OCR, decoded-text and cache budgets are separate. Caller-owned buffers,
allocator overhead, maps and model internals are not covered by a source byte cap.
The compact SRT line index can use 4 MiB of logical capacity at the 1 MiB input cap.
These are source-level bounds, not whole-process RSS or wall-clock guarantees.

## Language and recognition limits

No stemming, translation, transliteration, Unicode normalization or language-specific
tokenization. Combining marks split words; canonically equivalent spellings may not
match. Unspaced languages can produce too few tokens. Unicode lowercase is not full
caseless matching. Apostrophes/hyphens split words, and caption resegmentation can
destroy exact evidence.

No fuzzy edit-distance, general ASR robustness, drift correction, arbitrary edit
alignment, visual verification or dub recognition is supplied. Lexical-damage tests
only show that other exact shingles can survive a substituted word. Strict repetition
suppression intentionally loses recall, including near-identical references with too
little distinguishing content. See [regressions and validation limits](REGRESSIONS.md).
