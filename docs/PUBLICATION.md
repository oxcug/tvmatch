# Publication and licensing

Documentation describes the current implementation, not a promise that the checkout
is ready for unrestricted reuse or reproducible public builds.

## Owner decisions required

- **Code license:** no root code license file or Cargo package license is declared.
  The owner must choose and supply the applicable code license and copyright/notice
  information. Neither the model license nor fixture permission licenses the code.
- **Copied crate rights:** `crates/media-mkv-webm` and `crates/media-isobmff`
  originate in media-core and are included in this source workspace. Neither
  declares a package license. Confirm redistribution permission and supply applicable
  licenses/notices for both; no public dependency URL is needed to build this checkout.
- **Distribution terms:** review the locked dependency closure for the intended
  target/features and carry its applicable licenses, copyrights and NOTICE files.
  The OCR code inspection identified MIT OR Apache-2.0 packages, FlatBuffers under
  Apache-2.0, and additional Unicode-3.0 notices for unicode-ident; preserve applicable
  obligations rather than treating this as a complete distribution audit. Code
  and model weights have separate licensing requirements. Do not infer reference
  redistribution rights from either one.
- **Provider use:** resolve applicable OpenSubtitles API/commercial/indexing terms
  for the intended use. Publicly accessible subtitles are not an openly licensed
  redistributable reference pack; keep acquired captions private unless separately
  authorized. See [reference research](RESEARCH.md).
- **Supported builds:** establish a tested toolchain/target matrix and runtime
  requirements before claiming an MSRV, static runtime or clean-machine support.
  No published binary assets, CI badges or release guarantees are asserted here.

`publish = false` remains intentional. Publishing a GitHub repository is distinct
from Cargo registry publication, licensing permission and binary redistribution.
No license choice or public origin URL is invented by this documentation.

## Bundled OCR model

The unmodified recognizer is published by Robert Knight / ocrs and licensed
**CC-BY-SA-4.0**, independently of the OCR engine's code licensing. Its immutable
source, revision, original filename, byte count and SHA256 are in
[assets/ocr/NOTICE.md](../assets/ocr/NOTICE.md). The
[full legal text](../assets/ocr/LICENSE-CC-BY-SA-4.0.txt) accompanies the weights.

The executable embeds the same bytes whenever `ocr` is enabled. Distribute the
notice and full model license alongside executable distributions, preserve
attribution/license notices and comply with ShareAlike requirements for adaptations.
No publisher endorsement is implied. No detector or television references are bundled.
The notice's source URL is attribution, not a runtime download instruction.

The [synthetic fixture permission](../fixtures/README.md) applies only to that
invented dialogue. It supplies no television coverage or rights to other content.

## Publication review

Check that public files contain only intended source, synthetic fixtures and licensed
assets, with no private cache/media, credentials, personal paths, local validation
logs or executable snapshots. Review opt-in diagnostic source for dataset-specific
selectors and retained-content summaries before public release; ignored tests are
still public source when committed. Synthetic tests using public show metadata are
distinct from private collection diagnostics. Keep legal attribution and model
provenance intact. Check local Markdown links and generated API docs; do not publish
stale commands.

Run the [reproducible checks](CONTRIBUTING.md#checks) and review
[validation limits](REGRESSIONS.md#validation-limits). Test success is not universal
container conformance, a security audit, legal advice or an accuracy guarantee.

## Credentials and audit limits

For uncached references, configure `OPENSUBTITLES_API_TOKEN` as your OpenSubtitles
application API key in the process environment; internet access and quota are
required. `OPENSUBTITLES_BEARER_TOKEN` is optional for authenticated account behavior.
Do not include real values in source, shell arguments, logs, issues or cache exports.
The CLI does not load `.env` files. Verified reference and display-metadata cache
hits need neither credentials nor HTTP.

Publication checks should inventory both the working tree and staged blobs: ignoring
local secret files does not remove already staged content. Review and restage the
intended sanitized files before publishing. Pattern scans can miss credentials or
private data and are not exhaustive proof of absence. This checkout currently has
no committed history to audit; future publication must review any history separately.
If a plausible real credential is found, stop publication and have its owner rotate
it without testing or disclosing the value.
