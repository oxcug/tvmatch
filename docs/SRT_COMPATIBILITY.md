# SRT compatibility audit

Our reader is a **bounded SubRip subset**, not a universal player-compatibility or
formal-conformance implementation. The [Library of Congress format description](https://www.loc.gov/preservation/digital/formats/fdd/fdd000569.shtml)
characterizes conventional numbered, timestamped, blank-separated text cues; it is
not a universal conformance test suite. Sequential numbering from 1, chronological
starts, positive durations and our resource caps are explicit application policies,
not proof that every rejected file is invalid SRT. No external SRT parser dependency is used.
Raw downloaded bytes—not parsed/serialized output—are the lossless artifact.

## Implemented compatibility and explicit limits

`Transcript::parse(&str)` retains strict evidence validation, not a single spelling
of otherwise unambiguous syntax. `parse_bytes(&[u8])` adds bounded decoding. Provider
recovery remains explicit and provenance-checked; see [SRT_IMPORT.md](SRT_IMPORT.md).

| Feature | Public reader | Provider importer | Executable coverage |
|---|---|---|---|
| LF, CRLF, CR and mixed endings; BOM; EOF without newline | Supported; physical error line numbers retained | Same; bare CR recorded in source-layout policy | `srt::tests::compact_line_index_matches_endings_and_bounds_newline_heavy_inputs`; both layout corpora |
| Spaces/tabs/no spaces around literal `-->`; comma or dot three-digit milliseconds | Supported; exact two-digit clock fields and ranges retained | Supported; existing bounded padded-field policy unchanged | `srt::tests::compatible_layout_corpus_preserves_every_text_and_time_field` (48 combinations); provider counterpart (36 combinations) |
| Optional `X1:n X2:n Y1:n Y2:n` | All four ordered unsigned u32 values required, non-reversed rectangle; validated, not projected into cues | Same; discarded rendering placement recorded in source-layout provenance | `placement_and_clock_extensions_fail_closed_on_partial_or_malformed_fields`; provider composition/reopen tests |
| UTF-8; explicitly BOM-marked UTF-16LE/BE | `parse_bytes`; `parse` still takes UTF-8 Rust text | Automatic only with a matching BOM; encoding recorded; original binary body retained | `byte_api_decodes_only_explicit_boms_without_replacement_or_size_bypass`; provider UTF-16 receipt/reopen/empty-proof test |
| Unicode, multiline text, leading/trailing caption spaces, tabs and literal markup | Preserved; no line trimming or markup interpretation | Preserved except separately authorized C1/paragraph policies | Layout corpora assert exact text/start/end fields, not document equality |
| Gapped, fractional, missing indices; missing separators; padded clocks | Sequential labels from 1 and conventional framing still required | Existing bounded, versioned framing only; no invented cues/precision | `src/opensubtitles/tests/{framing,records,boundaries}.rs` |
| Out-of-order starts | Rejected; overlaps/equal starts allowed | Existing stable-order provenance, exact times preserved | `src/opensubtitles/tests/ordering.rs` |
| Empty or zero-duration records | Typed errors, never accepted evidence | Explicit MissingText/zero-duration omission; all-empty is not a usable reference | `src/opensubtitles/tests/{community,records,fallback}.rs` |
| Interior blank caption paragraphs | Rejected as ambiguous conventional framing | Consecutive anchored numbered boundaries only, exact nonblank text/times | `src/opensubtitles/tests/paragraphs.rs` |
| Other settings, malformed/partial placement, guessed decimal precision, invalid surrogates, unmarked legacy encodings, forbidden controls | Rejected | Rejected; not a reason to try other parsers/downloads | Syntax/byte negative tests, existing framing/control tests |

The optional `provider-srt-source-layout-v1` records encoding, original cue count,
bare-CR count, positioned cue count and a domain-separated layout digest. Cache
loads recompute every field. Existing UTF-8 LF/CRLF bodies without positioning do
not acquire this policy. Synthetic reopen tests require prior policies to remain
unchanged and source-layout metadata to recompute exactly.

The line index stores one u32 endpoint per physical line: at most 4 MiB of
logical index capacity for a 1 MiB newline-only source, using compact offsets.
Synthetic maximum-size LF/CR/CRLF tests check that capacity and fail-closed results;
caption sizes are checked before joins/blank-position collection. Input and decoded
UTF-8 each have the 1 MiB cap; invalid surrogates are never replaced. These are bounded
allocation tests and source-level bounds, **not peak RSS or universal 1 MiB-memory claims**;
raw/decoded text, records, maps and derivative strings coexist.

## Rust alternatives inspected

These findings are from source, documentation and test inspection, not execution
of the alternative libraries or a comparative benchmark.
Main/master source links below are mutable; pin commits before reproducible comparison.

| Candidate | Useful compatibility | Reasons not to replace our evidence boundary directly |
|---|---|---|
| [`subtp`0.2.0](https://github.com/mochi-neko/subtp) | PEG grammar, CR/LF/CRLF, arrow whitespace, position settings; MIT/Apache-2.0, peg/thiserror | Trims caption lines; no interior-blank recovery; grammar/test requires final caption newline; sequence-only subtitle equality is not a text/time oracle; no application budgets or chronology/range checks. |
| [`srtparse`0.3.0](https://github.com/rossnomann/srtparse) | BOM, multiline, gapped labels, EOF text; MIT and no dependencies | Trims lines; no paragraph recovery; missing separators may absorb headers into text; no application limits/range checks; unchecked time conversion and non-three-digit millisecond serialization need scrutiny. |

Primary inspected sources: subtp [grammar](https://raw.githubusercontent.com/mochi-neko/subtp/main/src/str_parser.rs),
[model/equality](https://raw.githubusercontent.com/mochi-neko/subtp/main/src/srt.rs),
[manifest](https://raw.githubusercontent.com/mochi-neko/subtp/main/Cargo.toml);
srtparse [parser](https://raw.githubusercontent.com/rossnomann/srtparse/master/src/parser.rs),
[reader](https://raw.githubusercontent.com/rossnomann/srtparse/master/src/reader.rs),
[time](https://raw.githubusercontent.com/rossnomann/srtparse/master/src/time.rs),
[manifest](https://raw.githubusercontent.com/rossnomann/srtparse/master/Cargo.toml).
These are source-inspection findings, not executed exploit/benchmark claims or a
maintenance guarantee. `subtile`'s inspected SRT surface was writing, not reading;
`subtitles` was a generator rather than the needed parser.

## Comparison limitations

Neither inspected candidate is a safe drop-in for the strict evidence boundary or
directly supports anchored interior-blank recovery. Keep provider transformations
explicit; trying multiple parsers until one accepts can silently lose evidence.
A bounded synthetic differential comparison should use explicit caption/time fields,
not document equality. More permissive acceptance is not automatically correctness.

The native compatibility corpora and allocation-bound tests run normally.
Cross-library differential execution and whole-process peak-memory profiling are
not established; alternative parser acceptance is not an oracle for author intent.
