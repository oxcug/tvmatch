# Folder identification and private reference cache

The [root CLI](../README.md#use) validates arguments before filesystem/cache/network
activity. Default features provide English folder identification with bundled OCR; there
are no public language/model/cache/sampling controls. The media folder and private
reference cache are separate: media content is never uploaded or rewritten.

An entirely empty, receipt-verified reference can offer a separately consented
replacement; see [fallback eligibility and recovery](FALLBACK.md). Ordinary missing
reference acquisition remains automatic, including during dry-run.

## Identity and coverage

Name resolution automatically accepts only one exact-name provider TV show in a complete
search. Otherwise the CLI offers at most five validated candidates, exact names first
then provider order, with title, year/IMDb when available and provider ID. It suggests
`--imdb ttID` instead or a numbered stdin option. Distinct IDs remain separate even when
titles match. A nonexact result requires explicit numbered consent; there is no
automatic fuzzy identity. Empty/EOF/invalid/oversized/read-error input cancels, as does
an out-of-range callback answer. Input needs a newline-terminated single displayed digit
(surrounding whitespace allowed), bounded to 64 bytes.

Search follows advertised pages with stable counts/progression, at most 10 page GET
calls and 1000 admitted rows (existing transport retry bounds still apply). The features
API's data-only, unpaginated envelope is also supported. Duplicate IDs, conflicting
metadata, invalid types/IDs/year, arrays over 1000 rows and malformed pagination fail
closed. More pages, accumulated results or display choices than these bounds permit
produce a labeled truncated list, never an automatic uniqueness conclusion. Missing
year/IMDb is labeled unavailable; rename-ready series metadata still requires the actual
show year before download POSTs. `--dry-run` offers unresolved choices and `--imdb`
guidance then stops with an error, without invoking the selection callback, reading
stdin, resolving episodes or downloading references for that unresolved show. A unique
exact match continues normally.

The `references_for_rename_with_interactions` API takes independent caller-owned
`ShowInteraction` and `FallbackInteraction` callbacks. Noninteractive APIs never prompt.
No shared cache transaction spans a show prompt. Explicit IMDb and frozen request scopes
bypass the picker. IMDb resolution uses `/features?imdb_id=NUMBER&type=tvshow`, verifies
the returned exact IMDb identity and TV-show type, then binds the stable provider show
ID/title. Strict public IMDb syntax is lowercase `tt` plus 1..10 decimal digits, numeric
value 1..2147483647. No media filename or OCR text supplies canonical identity.

Without an episode range, provider metadata supplies the actual episode list/count;
numbering is preserved, not inferred as 1..N. Optional single/range selection must exist
in that list. Season numbers 1..100 and episode numbers 1..1000 are finite metadata
guards, not subscription quotas. The season index accepts up to 1000 references
(including competing entries) while the library's ordinary `Index::build` still defaults
to 32. Global shared-shingle suppression and 100,000 distinct shingle/other actual work
limits remain unchanged; oversized work fails explicitly. Synthetic large-season
metadata/cache/index tests exercise this path.

Frozen scope manifests preserve IDs, exact titles and selected subtitle file IDs. An
explicit nonexact choice additionally stores optional `explicit-show-choice-v1` metadata
binding the exact query, selected provider ID and exact provider title to the immutable
manifest. Legacy manifests still require exact-name agreement; unknown or mismatched
choice metadata fails closed. Same-query range aliases retain this receipt; another
nonexact query needs fresh consent, never fuzzy alias inference. Exact-name/IMDb aliases
use their existing identity checks, not a transferred nonexact receipt. Existing
requests, original selections and raw references are never silently rewritten. Repeated
episode numbers may contain distinct provider episode IDs. Every ID with an eligible
English file (or an existing frozen choice) is retained independently, even when titles
repeat. Exact repeated ID/number/title rows are deduplicated before selection/downloads;
matching titles alone never merge identities. Each actual episode number must have at
least one reference. An ID without an eligible file need not block a covered number: the
frozen manifest records its ID, number, title and versioned
`no_eligible_english_reference_v1` reason, with an explicit warning on fresh/cached use.
That is a selection-time snapshot, not a claim that the IDs are interchangeable or that
unavailability is permanent. API errors/incomplete pagination are not absence; a whole
uncovered episode still fails. Existing frozen choices, including retained malformed
content, can never be discarded by this rule. The same ID assigned contradictory
numbers/titles still fails, as do conflicting file identities. One bounded subtitle
search per episode number supplies its candidates. Manifests are ordered by (episode
number, episode ID); existing caches remain valid. All catalog IDs (including
unavailable ones) count toward metadata limits; selected references count toward
quota/cache/index limits. Cached choices are reused by provider identity, not merely the
episode number. All selected candidates must have valid reference content before
matching. Their evidence is not pooled: a clear winner can identify, competing
qualifying results stay ambiguous, and indistinguishable or insufficient evidence can
remain unknown. Neither ambiguous nor unknown results rename.

A verified IMDb alias reuses an existing matching name catalog without downloading its
contents again. Ranges can reuse a cached superset. A range-only manifest is **not**
proof of complete-season membership: first whole-season use must obtain provider detail
metadata, but reuses existing selected files when identities agree. Whole-season
manifests then cache that coverage. Name/IMDb/season identities cannot cross-bind;
conflicting cached selections fail closed. Frozen catalogs are not destructively reset.

Missing references automatically trigger bounded acquisition. Complete cache hits return
before HTTP-client construction or credential reads: **HTTP=0**. Incomplete coverage is
never treated as a complete index for identification. Public metadata alone is not
dialogue evidence. Ranking stays fixed before OCR: English exact parent/episode
IDs/title, no AI/machine/foreign-only text, single-CD/single-file, conflicting
pack/range labels rejected; trusted, non-SDH, download count descending, numeric IDs
ascending. Full bounded pagination must agree, without score-driven alternate shopping.
Provider episode title comparisons decode one layer of common HTML character references
(`amp/lt/gt/quot/apos/nbsp` and valid numeric references); original catalog labels
remain frozen. There is no case folding, markup stripping or relaxed episode-ID check.
Genuine conflicts report season/episode and provider episode/subtitle IDs.

Empty timed provider records remain explicit `MissingText` outcomes in the record API;
the importer chooses to skip them with recomputed omission provenance. Invalid timing,
controls and other structural errors still fail, and the public SRT parser stays strict.
See [provider import](SRT_IMPORT.md). Retained bodies are recovered locally, not
redownloaded.

## Extraction

See [README](../README.md#supported-evidence) for eligible tracks and folder limits,
[OCR](OCR.md#cli-sampling-and-completion) for progressive sampling, and
[architecture](ARCHITECTURE.md#resource-limits) for matching budgets. Tracks are tried
independently; the first `Identified` result stops. If none identifies, an ambiguous
result takes precedence over unknown; if every track fails to scan, the file reports an
error. Source snapshots are rechecked between attempts and before accepting results.
Per-track errors do not block later eligible tracks.

## Preview and confirmed renames

One invocation scans once, builds the complete in-memory rename preview, then flushes
`Apply renames? (y/N): ` and reads stdin. Only a newline-terminated `y` or `yes`
(case-insensitive, surrounding whitespace allowed) applies it. Enter, `n`, EOF,
invalid/oversized input or a read error never approves changes. Input is bounded to 64
bytes. Piped confirmation is supported; there is no saved plan or second scan. No
eligible changes means no prompt. Preview-only automation can close stdin or supply `n`.

Only `Identified` files are eligible. Names are `Original Title (Year) - S01E08 -
Episode Title.mkv` using the provider episode label and preserving the source
extension's case. By default use provider `original_title` exactly, including
non-English spelling—not automatic title case or the lowercased `title`. `--show`
overrides the name only, even with IMDb; legacy name aliases do not override an IMDb run
implicitly.

The four-digit year is always the **show** metadata year, not the season year or a guess
from filenames. Missing/invalid year fails before download POSTs; a missing original
title needs an explicit `--show` override. Names allow 200 UTF-8 bytes, complete series
prefixes 207 bytes including ` (YYYY)`. Overrides are not persisted into lookup scopes,
provider metadata or frozen subtitle selections.

Original title/year is separately cached as bounded `series-ID.json` metadata under the
same coordinated cache. It is shared by seasons/name/range aliases of that show, counts
against the 250 MiB cap, and is not implicitly replaced. Older caches need one
identity-checked `/features` GET to enrich it; no subtitle POST/CDN fetch is used for
this. Reference-and-display-metadata hits remain offline and credential-free. This
metadata check precedes any required subtitle acquisition. Unsafe portable filename
characters are replaced with `_`, unsafe trailing dots/spaces are removed, and names are
bounded to 240 UTF-8 bytes without splitting a character. Sources retain native OS
paths, including non-Unicode names; display escapes controls unambiguously.
Duplicate/sanitized/case-colliding final targets are conflicts, not candidates for
overwriting, numbered suffixes or forced episode assignments. An occupied destination is
allowed when its current occupant is itself moving away in the same valid plan. Preview
resolves these dependencies; a stationary, unknown, ambiguous or conflicted occupant
blocks dependent renames too. Already-correct files are no-ops.

Apply orders rename chains from the free destination backward. Swaps, longer cycles and
case-only renames use an unused `.tvmatch-rename-...mkv` sibling temporarily to free one
slot, then finish the cycle. Every move, including staging, is atomic no-replace.
Temporary names are bounded and never overwrite another file. Source
identity/size/content mtime are checked again after staging; only the expected rename
ctime change is accepted. No temporary file is created when the preview is declined.

Before and after scanning, and again after confirmation, regular files are checked using
file identity, size and timestamps. Source/folder symlinks and Windows reparse points
are refused. Renames remain within the same directory. Atomic no-replace operations are
used on Windows (MoveFileExW without replacement), Linux (renameat2) and macOS
(renamex_np); unsupported OS/filesystems fail rather than falling back to an overwrite
or media copy. If a rename fails, earlier successful renames remain and the summary
reports the partial result; there is no automatic rollback. If a cycle fails after
staging, the summary names the temporary file left in the original folder. Interruption
can also leave temporary names; those regular media files can be inspected or rescanned
in a later invocation. The batch is not an all-or-nothing transaction. These checks are
not a sandbox against a hostile same-user process swapping paths.

Only per-file preview state lines have emoji: planned, already correct, conflicted,
unidentified, ambiguous or scan-failed. Other progress, prompts and summaries stay
plain. Conflicts name the occupying file and explain blocked dependencies to the root
cause.

The preview ends with episode coverage against the frozen provider season list
(including numbering gaps) or the explicit requested range. Same-number alternative
identities count as one episode. Only Identified reference IDs count, regardless of
rename conflicts/no-ops; filenames and ambiguous candidates never establish coverage.
Missing matches are listed by episode code, not asserted physically absent from
unrecognized or unsampled content.

Summary counts distinguish identification results from planned/applied/conflicted
renames. Exit 1 means any scan/input/conflict/apply error and takes precedence over 3
(ambiguous) or 2 (unknown); 0 means all identified without errors, including an
intentionally declined preview. `No changes needed` is printed only when every file was
identified and already has its desired name.

`--dry-run` shows the same complete preview and summary but never prompts, reads stdin,
or applies renames. Identification/conflict/error exit statuses are unchanged. It still
loads/acquires/recovers references normally: dry-run protects media names, not cache
state or download quota. A complete cached scope works without credentials.

## Cache cap and quota safety

Windows: `%LOCALAPPDATA%/tvmatch/references/opensubtitles-v1`. Unix:
`$XDG_CACHE_HOME/tvmatch/references/opensubtitles-v1`, otherwise
`$HOME/.cache/tvmatch/references/opensubtitles-v1`. Relative environment paths are
resolved against cwd; leading `~/` is expanded without shell evaluation. Native
paths/spaces/non-UTF8 bytes are preserved on Unix; only Windows converts MSYS drive
forms. Parent traversal, symlinks and Windows reparse points on owned paths are refused.

The cap is **262,144,000 logical bytes (250 MiB)** of owned reference content,
provenance, request manifests, staging files and charge-attempt markers. Models, media,
repo assets, lock/reservation housekeeping files and unrelated unowned paths are
excluded. Short shared OS-lock transactions inventory/trim oversized caches and reserve
room before materializing bytes. The bounded inventory visits at most 100,000 entries,
with fixed owned directory depth. Unknown paths are never traversed/deleted; unowned
children inside a content entry prevent eviction.

Cache ownership is keyed by **provider show ID + season**, not input spelling or
requested episode range. Name/IMDb aliases converge on the same lock. A competing run
for that season fails clearly before any download POST; different seasons/shows can
acquire concurrently. The shared housekeeping lock spans bounded filesystem operations
only, never HTTP, OCR or confirmation. It waits up to 30 s for another transaction;
season ownership is nonblocking and lasts until the cache is dropped (before OCR in the
CLI). OS locks release even on process termination; persistent `.coordination-v2.lock`
and `.locks-v2/*` files must not be deleted. Native OS file locking fails closed on
unsupported filesystems/platforms. Availability of that API does not establish a tested
MSRV; see [toolchain prerequisites](CONTRIBUTING.md#checkout-and-toolchain). The lock
inventory is bounded to 20,000 files; inactive reservation records are ignored.

A persistent `.lock` **protocol fence**, not a held root lock, prevents older binaries
from accessing the cache without the coordination rules. Migration refuses an existing
legacy lock: allow its owning older process to exit normally; never remove a live lock.
Use compatible cache clients for every invocation.

Complete entries are integrity/identity verified before eviction, then ordered by fetch
timestamp, with stable path ties: **FIFO, not read-LRU**. Reads never refresh that
order. Content and its successful `.attempt` marker are intentionally removed together
so later legitimate refetch is possible. Frozen manifests may reference an evicted file:
that is ordinary missing content, not corrupt identity, and the fixed file selection
remains reusable. Empty owned directories are pruned.

Unresolved staging is protected and recovered locally before HTTP. Bounded raw bytes are
saved before parsing; malformed bytes remain for review, not repeated downloads. A
synced full-response receipt (identity, byte count, SHA256) precedes the raw write.
Recovery requires matching receipt or existing verified provenance, plus an exact owned
sibling attempt marker if present. A parseable prefix is not completion evidence:
truncated/mismatched or receipt-less legacy staging stays protected without HTTP. An
exact owned attempt-only marker may be replaced under the cache lock on the next
deliberate invocation, with a warning that the prior POST may already have cost quota.
Unknown/malformed records are retained, not silently cleared. Manifests referencing
surviving content or protected attempts/staging are retained; only wholly
absent/unprotected manifests can be pruned for metadata space, ordered by insertion
mtime/path. Active scope/content is pinned during acquisition; other processes cannot
evict any content/manifests in an actively owned season. Before the first POST, the
entire missing working set reserves a conservative worst-case 1 MiB content plus 512 KiB
for receipt/provenance and the marker per entry. This may refuse a very large season
even if unknown eventual bodies might be smaller; it cannot thrash by
evicting/refetching its own needed set. Outstanding reservations are shared across
processes and consumed as bytes are written, so two seasons cannot both spend the same
cache capacity. A crashed owner's reservation ceases to count on OS unlock; its retained
attempts/staging still count and remain protected. Server quota and rate limits are
still shared and authoritative, not multiplied by concurrent runs. If protected state or
pinned work cannot fit, stop before charge. Successful future refetch after eviction can
cost quota.

`OPENSUBTITLES_API_TOKEN` is read internally as the app key; optional
`OPENSUBTITLES_BEARER_TOKEN` enables account quota checks. Never pass values in argv,
logs or cache. A key alone is not account authentication or a daily-quota promise. The
server's remaining/reset wins. Insufficient known quota means no download POST; unknown
initial quota allows one first requested file, then remaining coverage is checked before
further charges. Each missing selected file gets at most one POST per invocation; no
alternate-reference shopping. Episode-local content failures do not block other
unattempted files when quota permits. Incomplete coverage remains an error, never a
partial identification catalog.

Raw bodies remain authoritative. Provider-only repairs, omissions, ordering and
source-layout policies are separately versioned, recomputed on every cache load and
checked for exact retained caption/time semantics. Public SRT stays strict; see
[provider import](SRT_IMPORT.md) and [compatibility](SRT_COMPATIBILITY.md).

Recovered raw staging with a matching full-body receipt but without original provenance
records recovery time explicitly, not an invented original fetch time. Once synced
provenance carries the same validated body digest/size, the temporary receipt is
removed.

API TLS/credential host allowlists remain exact; credential-free CDN requests use a
separate allowlist. GET redirects max 3; GET 429 has at most one bounded 30 s numeric
Retry-After retry. Credential-free CDN GET 502/503/504 allows at most two short-backoff
retries of the same in-memory link, never another POST. Requests are paced 1.1 s with
512-request/30-minute admission bounds, 30 s/request, 16 KiB headers, 2 MiB JSON and 1
MiB SRT caps. Those finite request/body limits can independently stop unusually large or
heavily paginated seasons. Bodies, headers, signed links and secrets are not dumped.
Compression is disabled/rejected.

Retained limitations: admission is not a strict overall wall-clock deadline;
DNS-resolved private/loopback addresses are not separately filtered; files are synced
but parent directories are not separately synced for power loss; an interrupted write
can leave an attempt or incomplete data requiring review (OS locks auto-release).
Existing Unix directory ownership/ permissions are not enforced (created directories use
0700); provision a private cache. Windows relies on inherited user ACLs. Not a sandbox
against the same OS user. Public reference access is not redistribution permission;
captions remain private.
