# Regression coverage and validation

Normal checks use original synthetic fixtures, not private media, credentials or
live downloads. Opt-in diagnostics remain ignored. No matching threshold is relaxed
and no forced assignment is used to make a fixture pass. Reproduce checks with the
[contributing commands](CONTRIBUTING.md#checks), recording toolchain/target/features
and failures rather than relying on a test-count snapshot.

## Source-named coverage

Test names below are search anchors within their linked source files; provider
modules live under `src/opensubtitles/tests/`. Names are retained as implemented,
not descriptions of private collection outcomes.

| Contract | Synthetic regression anchors |
|---|---|
| Public SRT syntax, encoding and bounds | [src/srt.rs](../src/srt.rs): `compatible_layout_corpus_preserves_every_text_and_time_field` (48 combinations), `compact_line_index_matches_endings_and_bounds_newline_heavy_inputs`, `placement_and_clock_extensions_fail_closed_on_partial_or_malformed_fields`, `byte_api_decodes_only_explicit_boms_without_replacement_or_size_bypass` |
| Provider layout/provenance | [compatibility.rs](../src/opensubtitles/tests/compatibility.rs): `provider_layout_corpus_has_exact_fields_and_versioned_new_syntax` (36 combinations), `utf16_raw_body_receipt_encoding_and_empty_outcome_are_preserved`, `new_layout_policy_is_recomputed_on_reopen_and_all_fields_are_bound`, newline/decoded/caption caps and composed policies |
| Anchored interior blank paragraphs | [paragraphs.rs](../src/opensubtitles/tests/paragraphs.rs): `framer_retains_paragraphs_provider_normalizes_and_public_srt_stays_strict`, `ambiguous_paragraphs_or_bad_headers_are_not_assigned_invented_timestamps`, exact nonblank text/times, composition and offline tamper/reopen |
| Unified framing with combined defects | [framing.rs](../src/opensubtitles/tests/framing.rs): 64 layout combinations, missing labels/separators, fractional labels, padded clocks, zero duration, numeric ambiguity, precision and recomputed framing metadata |
| Strict public `MissingText` vs provider omission | [community.rs](../src/opensubtitles/tests/community.rs): typed per-record errors, caller-only omission, exact retained duplicates/text/times, composition, fatal errors and offline tampering |
| Numbering and zero-duration policies | [records.rs](../src/opensubtitles/tests/records.rs): `gapped_indices_keep_every_existing_timed_caption_and_do_not_create_missing_cues`, `zero_duration_omission_preserves_raw_and_retained_semantics_not_public_parser_leniency`, no hidden invalid input, policy composition and no-redownload reopening |
| Unindexed cues, explicit C1 placeholders and stable ordering | [boundaries.rs](../src/opensubtitles/tests/boundaries.rs), [ordering.rs](../src/opensubtitles/tests/ordering.rs): exact retained semantics, ambiguity/control/timing rejection and independent policy tampering |
| PGS acquisition `0x40` refresh and lifetime safety | [tests/pgs_bounded.rs](../tests/pgs_bounded.rs): `acquisition_regressions` bridges four local crate oracles covering fresh resources/color, stale-resource refusal, canvas/incomplete/reserved state and cumulative budgets; `pgs_acquisition_mkv_stream_preserves_images_and_interval_metadata` exercises actual Matroska integration |
| True stopping-packet tail validation | [tests/pgs_bounded.rs](../tests/pgs_bounded.rs): `pgs_acquisition_stop_request_still_rejects_malformed_same_packet_tail` makes an actual `ControlFlow::Break` request and requires the malformed same-packet tail to fail |
| Video-only BlockAdditionMapping families | [tests/streaming_bounded.rs](../tests/streaming_bounded.rs): `video_mapping_family_preserves_selected_packet_and_track_type_order`, `video_mapping_does_not_allow_other_track_transforms_or_escape_caps`; repeated dvcC/hvcE maps, both type orders, exact packet/time, payload skipping, invalid widths/types/budgets |
| MKV metadata, raw timing, language and bounded I/O | [tests/streaming_bounded.rs](../tests/streaming_bounded.rs): MinCache unsigned-width/duplicate checks, metadata discovery, Cues/seek semantics, `filtered_walk_skips_interleaved_payloads_without_readahead_amplification`, cumulative I/O/error bounds; [tests/media.rs](../tests/media.rs): raw-nanosecond ordering, LanguageIETF precedence, selectors and text limits |
| PGS composition and allocation | [tests/pgs_bounded.rs](../tests/pgs_bounded.rs): RLE, fragmentation, palette transparency/deltas, crop/window/alpha, display intervals, complete/stopped results, old+new peaks, cumulative budgets and malformed prefixes |
| MP4 native demux oracles | [tests/mp4_demux.rs](../tests/mp4_demux.rs): eight tests from the local crate's normal demux module; tables/ranges, edits, opaque tracks, guarded payload skipping and `selected_sample_read_eof_and_seek_failures_are_terminal_not_successful_prefixes` (persistent failure without further I/O) |
| MP4 application adapter | [src/media/mp4/tests.rs](../src/media/mp4/tests.rs): actual path dependency, front/back `moov`, fixed/variable sizes, 32/64-bit offsets, timing edits, UTF-8/UTF-16, forced/external/unsupported tracks, late invalid samples and unchanged matching |
| Sparse tall OCR crops without glyph loss | [src/media/ocr.rs](../src/media/ocr.rs): `tall_sparse_crops_preserve_every_composited_pixel_and_band_order`, `tall_compaction_preserves_detached_glyph_gaps_and_separate_lines`, `ordinary_crop_geometry_and_pixels_remain_byte_identical`, dense/source/line limits and full-source charging before inference |
| Progressive OCR without score pooling | [src/media/ocr.rs](../src/media/ocr.rs) `stream_tests`: one collector/no prefix replay, blank attempts, EOF, packet/later failure and repetition removing support; [src/folder/progressive.rs](../src/folder/progressive.rs): `widening_announces_each_transition_and_has_a_hard_stop`, `identified_stops_without_widening_or_output` |
| Indistinguishable reference content remains Unknown | [tests/engine.rs](../tests/engine.rs): `identical_labeled_references_have_no_distinguishing_evidence`, `near_identical_references_with_only_one_unique_cue_remain_unidentifiable`, shared intros, repeated phrases, reversed dialogue, offsets, jitter and ambiguous competitors |
| Bounded show picker and immutable explicit choice | [show_selection.rs](../src/opensubtitles/tests/show_selection.rs), [show_picker.rs](../src/folder/show_picker.rs): max five choices, pagination/truncation without false uniqueness, explicit ID/frozen bypass, dry-run/EOF/read-error cancellation, consent receipt/tamper/aliases and no shared transaction during prompt |
| Empty-reference fallback and recovery | [fallback.rs](../src/opensubtitles/tests/fallback.rs): acquisition then separate consent, dry-run/decline, same-ID aliases, fresh-consent retries, retained-body recovery, three-version cap, no fallback for malformed/usable content, quota/tampering/source-change refusal and protected capacity |
| Catalog identities and availability | [availability.rs](../src/opensubtitles/tests/availability.rs), [alternatives.rs](../src/opensubtitles/tests/alternatives.rs): distinct IDs with repeated titles, exact row deduplication, covered duplicate-number alternatives, unavailable warnings/aliases, explicit ranges, incomplete pagination/uncovered episodes and frozen malformed references failing closed |
| Opaque release tags and escaped metadata titles | [catalog.rs](../src/opensubtitles/catalog.rs): `opaque_tag_prefixes_are_not_episode_labels_but_complete_claims_still_are`; [community.rs](../src/opensubtitles/tests/community.rs): single-layer character references without changed IDs/frozen labels; conflicting complete/ranged episode claims remain rejected |
| Original title/year and display override | [series.rs](../src/opensubtitles/tests/series.rs), [src/folder.rs](../src/folder.rs), [tests/cli.rs](../tests/cli.rs): identity/year/Unicode validation, override scope separation, legacy metadata/aliases, offline hits, capacity/immutability and both help forms |
| Cache no-replay and concurrent progress | [src/opensubtitles/cache.rs](../src/opensubtitles/cache.rs), [locking.rs](../src/opensubtitles/tests/locking.rs), [cache/concurrency.rs](../src/opensubtitles/cache/concurrency.rs): full-body receipts vs prefixes, protected malformed state, canonical alias ownership, paused downloads, shared reservations, active eviction protection, legacy fencing and abrupt process-exit unlock |
| Consent-safe rename plans | [src/rename/tests.rs](../src/rename/tests.rs), [dependencies.rs](../src/rename/tests/dependencies.rs): declined/EOF/error/dry-run consent, source revalidation, no-overwrite races, stationary/duplicate/sanitized/case conflicts, chains/swaps/cycles, partial failures/no rollback and reported temporary names |
| Mixed extensions and truthful coverage | [dependencies.rs](../src/rename/tests/dependencies.rs): `mp4_and_m4v_swaps_preserve_extension_case_bytes_and_consent`; [src/rename/tests.rs](../src/rename/tests.rs): coverage uses expected IDs, counts confident conflicts once, excludes ambiguous candidates and never infers from filenames |

## Validation limits

- Dependency tests do not run transitively. The eight MP4 demux and four PGS
  acquisition source bridges are part of normal app checks; they do not substitute
  for complete crate suites. Use the workspace test command in
  [contributing](CONTRIBUTING.md#checks) to run those suites too.
- Native MKV checks and focused native Linux rename/OS-lock checks provide narrower
  platform evidence than the complete CLI matrix. Full offline Linux Cargo testing
  has a missing `ocrs` registry-cache prerequisite; a complete Unix CLI run and the
  macOS rename path are not established. No clean-machine/static-runtime promise.
- MP4 tests are synthetic; real MP4 interoperability is not established. Live API
  show-picker interoperability is not established by mock pagination/consent tests.
- SRT corpora test explicit text/time fields, provenance and bounded allocations,
  not universal player conformance. Cross-library differential execution and
  whole-process peak-memory profiling are not established.
- PGS samples do not validate unread suffixes. Probes do not validate media payloads.
  Bounded deterministic mutation tests are not a fuzzing campaign or security proof.
- No universal recognition accuracy, calibrated confidence, held-out precision/recall
  or global reference uniqueness is claimed. Unknown/ambiguous results remain valid
  outcomes, not permission for alternate-reference shopping or forced naming.

Do not unignore private/live diagnostics to reproduce the normal synthetic suite.
No private collection, body digest or installed-executable snapshot is needed for
these regression contracts.
