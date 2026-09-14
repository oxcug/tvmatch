# Consented fallback for entirely empty references

When a receipt-verified downloaded SRT consists entirely of framed `MissingText`
records, the normal CLI can list up to three ranked alternative English uploads for
that same provider episode ID. It proposes the highest-ranked remaining version:

```text
Download fallback file ...? This may use download quota. (y/N):
```

Only a newline-terminated, trimmed, case-insensitive `y` or `yes` authorizes a
replacement download. Blank input, another answer, EOF, oversized input or a read
failure declines. This is **separate from the later rename confirmation**. Provider
metadata and ordinary missing references retain their normal automatic acquisition
behavior; fallback can cost an additional metadata search and one download charge.
Metadata does not establish that the proposed upload actually contains captions.

`--dry-run` lists alternatives but never calls the fallback confirmation handler or
downloads a replacement. Normal reference acquisition and local recovery still run;
dry-run is not generally offline or quota-free. Without complete references there
is no media scan/rename preview, and the command reports incomplete coverage.

## Eligibility and bounds

- Complete raw body and exact selection/size/SHA256 receipt must agree. At least one
  valid timed record must exist, and **every** record must report `MissingText`.
- Malformed syntax/Unicode/timing, missing receipts, nonempty zero-duration-only
  references and weak/ambiguous matching do **not** authorize fallback. Matching
  scores, media filenames and OCR never choose reference versions.
- Other references must already be valid or likewise proven empty; unrelated
  acquisition/corruption failures block this extra spending.
- Same language/show/season/episode/provider ID and original catalog title; existing
  pagination, release-conflict and eligibility checks apply. Ranking remains trusted,
  non-HI, download count, then IDs. Available duplicate episode IDs remain independent.
- At most **one fallback POST per invocation**, and **three approved alternative
  versions per original selection**. An empty or malformed replacement is retained;
  it never causes automatic cycling. A later invocation may offer another version
  only if the last received body was also entirely empty. No public tuning flags.

## Durable approvals and recovery

Original request manifests, empty bodies and download receipts are not rewritten.
Canonical episode-directory files `fallback-ORIGINAL_FILE_ID-STEP.json` record
`consented-empty-reference-fallback-v1`, the origin/from/candidate selections,
verified empty-body size/hash/record count and approval timestamp. These are immutable,
bounded, synced approval records, published before a replacement POST can be charged.
An interrupted `.partial` approval fails closed; it is not silently overwritten.

Name/IMDb/range aliases resolve the same approval chain, without rewriting their
frozen manifests or dropping an episode from the index. A replacement becomes usable
only after ordinary strict import/provenance validation. Completed retained responses
can recover locally without another prompt or POST. An approved attempt with no body
requires **fresh confirmation** to retry that same frozen candidate on a later run;
a prior approval is not unattended replay permission. Malformed retained bodies are
never redownloaded or silently bypassed.

Approval files and all approved versions (including successful replacements) are
protected from this version's cache eviction and count against the 250 MiB cap.
Capacity reservation, source revalidation after prompting, scoped OS locks and
no-replay markers still precede network spending. The fallback prompt holds canonical
show/season ownership but **not the shared filesystem transaction**; other seasons
can proceed. The cache is dropped before OCR and the rename prompt. Use only cache
clients that understand these approvals; incompatible versions cannot resolve them.

Library callers retain noninteractive `references` / `references_for_rename` behavior;
they can opt into `references_for_rename_with_fallback` with `FallbackInteraction`.
The library never reads stdin itself. `Cache::selected_references` continues to expose
original frozen coverage metadata; references load the approved effective file.

## Validation

Synthetic tests cover initial acquisition followed by fallback, no fallback for usable
content, decline/dry-run, explicit consent, aliases/offline reopening, fresh-consent
retry, interrupted complete-body recovery, three-version limits, malformed input,
zero quota, source changes during prompting, independent-season progress, approval
tampering, partial approval refusal and protected capacity/eviction.

See [the regression map](REGRESSIONS.md) for synthetic test sources.
