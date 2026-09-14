# Reference coverage and implementation choices

## Reference rights and coverage

No authoritative, openly licensed, episode-labeled audiovisual fingerprint or
timestamped-dialogue corpus suitable for a redistributable general-TV identification
product was established within the researched sources. This bounded negative finding
is not proof of nonexistence. An open matching algorithm, metadata API or downloadable
dataset does not itself supply reference rights, canonical episode/edition labels or
sufficient coverage.

The CLI uses independent provider-labeled English subtitles in a private cache;
library callers can supply labeled references. Keep code, models and reference
content legally distinct. Possessing media does not establish redistribution rights
to it or a derived pack. A distributable pack needs source authority, episode/edition/
language, timing basis, authorization, transformation version and digest.

| Source | Finding and constraint |
|---|---|
| [TVR](https://github.com/jayleicn/TVRetrieval) | Research retrieval data from six shows; [data license](https://raw.githubusercontent.com/jayleicn/TVRetrieval/master/data/LICENSE) is CC BY-NC-SA 4.0. Annotation rights do not establish commercial redistribution rights to dialogue/footage. |
| [TVSM](https://zenodo.org/records/7025971) | Audio features and speech/music labels, not an established canonical episode fingerprint registry. Data rights and episode-ID mapping remain unverified. |
| [BBCAVS:10k](https://raw.githubusercontent.com/bbc/dsrp_bbcavs10k_distribution/master/README.md) | Broadcaster provenance, but access requires partner-university affiliation and a signed agreement. |
| [OPUS OpenSubtitles](https://opus.nlpl.eu/legacy/OpenSubtitles-v2018.php) | Inspected material did not establish a blanket open-content license or reliable edition/timing mapping. No bundling assumption. |
| OpenSubtitles [API overview](https://opensubtitles.tawk.help/article/about-the-api) and [terms](https://opensubtitles.tawk.help/article/terms-of-service) | Inspected API material advertises commercial packages while general terms prohibit commercial use. Applicability/indexing/redistribution clarification is required; the conflict is not resolved here. |
| [ACRCloud custom recognition](https://docs.acrcloud.com/get-started/tutorials/recognize-custom-content) | Bring-your-own uploaded catalog and proprietary service, not a free TV corpus. |
| [Audible Magic](https://www.audiblemagic.com/technology/) | Commercial rights-holder recognition; exact episode/edition coverage, IDs, terms and local deployment remain unverified. |
| [TVmaze](https://www.tvmaze.com/api), [TMDB](https://developer.themoviedb.org/docs/faq), [TVDB](https://github.com/thetvdb/v4-api) | Metadata/ordering, not dialogue or soundtrack reference lookup. |
| Original or authorized supplied references | Coverage equals the labeled material supplied; synthetic fixtures demonstrate mechanics, not real-TV accuracy. |

## Native media boundary

The implementation uses the local media-core-derived MKV and ISO BMFF crates for native
container parsing. Demuxing is not decoding audio/video. PGS reconstructs subtitle
images; [OCR](OCR.md) converts them to fallible text. No FFmpeg, audio fingerprinting
or ASR path is selected. Current support is defined by [MKV](MKV.md) and [MP4](MP4.md),
not by the capabilities of researched alternatives.

Useful primary sources for evaluating alternatives, **not tested tvmatch support**:

- [Symphonia 0.6.1 API](https://docs.rs/symphonia/0.6.1/symphonia/),
  [manifest](https://raw.githubusercontent.com/pdeljanov/Symphonia/v0.6.1/symphonia/Cargo.toml),
  [MKV mappings](https://raw.githubusercontent.com/pdeljanov/Symphonia/v0.6.1/symphonia-format-mkv/src/codecs.rs),
  [MP4 entries](https://raw.githubusercontent.com/pdeljanov/Symphonia/v0.6.1/symphonia-format-isomp4/src/atoms/stsd.rs):
  AAC-LC/PCM/FLAC decoding options do not supply AC-3/E-AC-3/DTS decoders merely
  because demuxers recognize those identifiers. MPL-2.0 obligations need review.
- [matroska-demuxer](https://github.com/hasenbanck/matroska-demuxer) and
  [mp4-rust](https://github.com/alfg/mp4-rust): container/sample readers, not audio/video
  decoders or a reason to duplicate the selected parser.
- [oxideav-ac3](https://github.com/OxideAV/oxideav-ac3) and
  [oxideav-dts](https://github.com/OxideAV/oxideav-dts): experimental decoder candidates;
  author conformance claims were not independently reproduced. DTS Core support
  does not establish DTS-HD extension support.
- [Candle](https://github.com/huggingface/candle) and
  [whisper-rs](https://codeberg.org/tazz4843/whisper-rs): ASR candidates, not selected
  dependencies. The latter uses a C/C++ backend. Both need trusted model artifacts,
  audio preparation, timing and target-specific build/runtime evaluation; neither
  supplies episode identity.
- [audfprint](https://github.com/dpwe/audfprint) (MIT),
  [Panako](https://github.com/JorenSix/Panako) (AGPL), and
  [vPDQ](https://github.com/facebook/ThreatExchange/tree/main/vpdq) (BSD repository):
  algorithm references, not episode catalogs or unchanged Rust-only runtimes.
  vPDQ's documented comparison ignores frame order, requiring temporal verification
  for ordered evidence. Code licenses are not content licenses or patent clearance.

For SRT parser alternatives and source-level tradeoffs, see
[SRT compatibility](SRT_COMPATIBILITY.md#rust-alternatives-inspected).

## Research limits

These are bounded source/document/license-inspection findings, not independently
executed codec conformance, security or maintenance audits. Some source retrievals
were incomplete, particularly TVSM rights/mapping. No vendor agreement or general
legal determination is supplied. Mutable source links need exact revisions before
reproducible comparison; external sources have not been re-fetched for this document.

Meaningful recognition evaluation needs legally usable independent reference/query
pairs, out-of-catalog material, same-series neighbors, recaps, misleading filenames,
dubs, edits and multi-episode content. Separate tuning/calibration/testing by episode
and include held-out series. Measure accepted precision versus coverage, unknown
false accepts, candidate recall, latency and peak memory. Neither synthetic success
nor a small selected sample establishes those properties.
