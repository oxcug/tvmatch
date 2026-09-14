# media-isobmff

Native ISO Base Media File Format box layer, originating in media-core and included
in the tvmatch source workspace under its original package name. No package license
is declared; see [publication decisions](../../docs/PUBLICATION.md).

- `boxes`: native box encoders/decoders and codec sample-entry types.
- `parse`: video/audio track and sample-table parsing.
- `demux`: [bounded non-fragmented `Read + Seek` sample reads](DEMUX.md).
- `fmp4` (default feature): initialization segments and fragment writing.
- `heif` (default feature): HEIF metadata parsing and item access.

Container demux/mux is not codec decoding. The tvmatch adapter selects a narrower
[timed-text subset](../../docs/MP4.md); optional fragment/HEIF functionality remains
available to crate callers. `mp4-atom` is an exercised registry-only writer-test
oracle. Mozilla's `mp4parse` is a retained, currently unused dev dependency, not
evidence of cross-parser HEIF validation. Neither is a production parser. Run all
suites from the root with
`cargo test --offline --locked --workspace --all-features` after populating the
registry cache; synthetic tests do not establish universal conformance.

## Specifications and reference material

- ISO/IEC 14496-12: ISO Base Media File Format.
- ISO/IEC 23008-12: HEIF.
- [Nokia HEIF technical summary](https://nokiatech.github.io/heif/technical.html).
- [AOMedia AVIF specification and samples](https://github.com/AOMediaCodec/av1-avif).

Reference links are not bundled test corpora or permission to redistribute content.
