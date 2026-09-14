# tvmatch

Identify television files from independently episode-labeled subtitle evidence.
`tvmatch` is a Rust library and CLI with native MKV/MP4 text extraction and CPU PGS
OCR. It previews filename changes and requires explicit confirmation to apply them.

**No FFmpeg, external OCR, model downloads, media uploads, audio inference or forced
episode assignment.** Subtitle matches are not audio/video verification; results
can be `Identified`, `Ambiguous` or `Unknown`.

## Build prerequisites

This is a **self-contained source workspace**: the media-core-derived
`media-mkv-webm` and `media-isobmff` crates live under `crates/`, with their
original package names. No sibling checkout is required. Registry dependencies
still require acquisition or an already populated Cargo cache. See
[contributing](docs/CONTRIBUTING.md) for toolchain requirements and workspace checks.

With a suitable Rust toolchain and registry dependencies available:

```sh
cargo build --locked
cargo install --path . --locked
```

Default features include `ocr` and `opensubtitles`, providing the full CLI. The
recognizer is embedded; no consumer model setup is needed. The std-only library is
available with `--no-default-features`; media-only library builds use
`--no-default-features --features media`. These reduced builds do not provide folder
matching. No minimum Rust version or clean-machine runtime guarantee is established.

## Use

```sh
tvmatch --folder './media/Example Show/Season 1' --show 'Example Show' --season 1 --dry-run
# In a media directory, select identity by IMDb; an optional name changes display only:
tvmatch --imdb tt1234567 --show 'Example Show' --season 1
```

Names and IDs above are illustrative; supply the actual show identity.

```text
tvmatch [--folder PATH] (--show NAME | --imdb ttID [--show NAME]) --season N [--episodes N|A-B] [--dry-run]
tvmatch --help | -h
```

Folder defaults to the current directory. Quote paths and names with spaces.
Enumeration is nonrecursive: at most 32 `.mkv`/`.mp4`/`.m4v` files, case-insensitive,
and 256 directory entries. Season numbers are 1–100, episode numbers 1–1000.
Without an episode range, provider metadata supplies the actual season list, not
file count or inferred consecutive numbering.

With IMDb, `--show` overrides only the display name. Name-only lookup automatically
accepts a unique exact match only from a complete search. Otherwise it lists up to
five validated TV shows for explicit numbered selection, or suggests using IMDb.
Search is bounded to 10 pages / 1000 results; truncation never proves uniqueness.
An unresolved picker in dry-run lists choices and stops without reading stdin.

## Supported evidence

| Container | Embedded subtitles | Processing |
|---|---|---|
| MKV | `S_TEXT/UTF8` | Direct text |
| MKV | `S_HDMV/PGS` | Native bitmap decoding and bundled CPU OCR |
| Non-fragmented, unencrypted MP4 / compatible M4V | `tx3g` | UTF-8 or BOM-marked UTF-16 text |

Enabled, non-forced **declared-English** tracks are tried sequentially in track-number
order. The first confident match wins; evidence is never pooled across tracks.
Text tracks are read fully within fixed budgets and never initialize OCR. PGS starts
at 64 visible frames and widens to 128, then 192 only if unresolved. Each transition
is announced; the unread suffix remains unvalidated.

No sidecars, ASS/SSA, WebVTT/TTML, burned-in text, encrypted/fragmented MP4 or non-English
reference acquisition. Unicode decoding is not general multilingual recognition.
See [MKV](docs/MKV.md), [MP4](docs/MP4.md) and [OCR](docs/OCR.md) for exact subsets.

## References, costs and consent

Missing references are acquired automatically through OpenSubtitles HTTPS. The CLI
requires `OPENSUBTITLES_API_TOKEN`, your OpenSubtitles application API key, for
uncached references. Internet access and provider quota are also required.
`OPENSUBTITLES_BEARER_TOKEN` is optional for authenticated account behavior.
Configure these environment variables using your OS/session secret facility before
starting the CLI; do not commit values to this repository. The CLI reads the process
environment, not `.env` files.
Never put secret values in arguments or logs. Verified reference **and** series-title/year
cache hits need no HTTP or credentials. Older caches may need one metadata-only GET.
Server quota applies; incomplete reference coverage stops identification.

The private reference cache has a 250 MiB logical cap and FIFO eviction of eligible
complete entries. Protected recovery data and fallback approvals count against the
cap. A later refetch of evicted content can cost quota. Public access to subtitles
is not redistribution permission.

Three decisions are separate: selecting an unresolved show, approving an empty-reference
fallback download, and applying renames. Entirely caption-empty, receipt-verified
references can offer a separately confirmed replacement; malformed or weak-matching
references cannot. At most one fallback download per invocation and three approved
alternatives per original selection. See [fallback](docs/FALLBACK.md).

**Dry-run prevents media renames, not ordinary downloads or cache recovery.** It is
not generally offline or quota-free. Unresolved show selection and fallback dry-run
are list-only: neither reads confirmation nor downloads a fallback replacement.

## Rename preview

```text
Rename preview:
  📝 input.mkv
    → Example Show (2020) - S01E01 - Episode Title.mkv
Apply renames? (y/N):
```

Only newline-terminated `y` or `yes` applies the same-run preview. Enter, `n`, EOF,
invalid input or a read error leaves names unchanged. Dry-run never prompts. Names
use provider `original_title (show year)`, or the display override with the same
metadata year, plus the identified episode label; source extension/case is preserved.

Unknown and ambiguous files remain untouched. Existing files are never overwritten.
Valid chains and swaps can move into destinations vacated by the same rename plan;
stationary occupants and duplicate final names conflict. Sources are snapshotted and
revalidated. Partial failures retain earlier successful moves and may leave reported
temporary sibling names; there is **no rollback or all-or-nothing guarantee**.
See [folder and rename safety](docs/FOLDER.md#preview-and-confirmed-renames).

Exit codes: **1** input/scan/conflict/apply error (takes precedence), **3** ambiguous,
**2** unknown, **0** all identified without errors, including a declined preview.
Missing episode matches do not prove physical absence from unsampled/unrecognized files.

## Library and documentation

- [Architecture, library example and evidence rules](docs/ARCHITECTURE.md)
- [Folder identity, cache, transport and rename safety](docs/FOLDER.md)
- [SRT compatibility](docs/SRT_COMPATIBILITY.md) and [provider import](docs/SRT_IMPORT.md)
- [Regression map and validation limitations](docs/REGRESSIONS.md)
- [Research and reference rights](docs/RESEARCH.md)
- [Contributing and reproducible checks](docs/CONTRIBUTING.md)
- [Publication and licensing decisions](docs/PUBLICATION.md)

Synthetic tests demonstrate bounded mechanics, not universal format conformance,
calibrated confidence or real-TV precision/recall. Real MP4 and live show-picker
interoperability are not established.

No root code license is declared; the owner must choose one before reuse terms can
be stated. The bundled model has its own [attribution and CC-BY-SA-4.0
license](assets/ocr/NOTICE.md), independent of code and reference-content rights.
