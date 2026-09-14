# media-mkv-webm

Matroska/WebM container crate, originating in media-core and included in the tvmatch
source workspace under its original package name. No package license is declared;
see [publication decisions](../../docs/PUBLICATION.md).

- `ebml`: typed element readers/writers and Matroska schema identifiers.
- `demux` / `cluster`: slice-based metadata and borrowed frame access.
- `streaming`: [bounded `Read + Seek` extraction](src/streaming/README.md).
- `mux`: encoded-packet muxing, SeekHead/Cues and opt-in live streaming output.
- `pgs`: [bounded headerless PGS image decoding](src/pgs/README.md).
- `audio`: backend-independent audio decoder trait; default `audio-symphonia`
  enables AAC/Vorbis/FLAC/PCM decoding. Opus decoding is unsupported.

The muxer enforces the WebM codec whitelist using DocType. Readers report DocType
but do not enforce that whitelist; callers can use the `profile` policy helper.
Video frames and unknown Matroska codec payloads remain opaque; callers own video
decoding. Selected lacing
is unsupported. Attachments/chapters and richer metadata are not a high-level API;
streaming and slice readers have distinct documented validation subsets. Muxing
packages already encoded packets, not audio/video encoding.

Run all suites from the root with
`cargo test --offline --locked --workspace --all-features` after populating the
registry cache. Synthetic cross-parser tests use `matroska-demuxer` as a dev-only
oracle for track/frame parity, mux output, Cues seeking and streaming shape.
These and the bounded tvmatch adapter tests are not universal format conformance
or end-to-end audio decoder validation. The app disables crate default features
and supports only its documented [subtitle subset](../../docs/MKV.md).

## Specifications

- [Matroska specification](https://www.matroska.org/technical/specs/index.html).
- [Matroska codec mappings](https://www.matroska.org/technical/codec_specs.html).
- [WebM container guidelines](https://www.webmproject.org/docs/container/).
- [EBML RFC 8794](https://www.rfc-editor.org/rfc/rfc8794).
