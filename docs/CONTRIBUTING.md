# Contributing

## Checkout and toolchain

This self-contained source workspace uses local path dependencies with default
features disabled in [Cargo.toml](../Cargo.toml):

```text
tvmatch/
  crates/
    media-mkv-webm/
    media-isobmff/
```

Both crates originate in media-core and retain their package names. Their sources,
optional features and test oracles are included here. No sibling source or external
include path is needed. The root is the default workspace member, so ordinary build
and install commands retain the full CLI defaults. Use `--workspace` to check all
three packages. Code and copied-crate licensing remain owner decisions; see
[publication](PUBLICATION.md).

Use a Rust toolchain supporting edition 2024 and the dependency APIs, including
native OS file locks. No `rust-version` is declared and no minimum supported Rust
version has been established by a toolchain matrix. [Cargo.lock](../Cargo.lock)
pins registry packages; local crate sources are part of this checkout.

`--offline` requires an already populated Cargo cache. A missing cached package is
a prerequisite failure, not a reason for a validation command to silently fetch
it or change lockfiles. Dependency acquisition is a separate, deliberate setup
operation. The root lockfile covers the workspace, including optional audio and
registry cross-parser dev dependencies.

## Checks

Run from the repository root after prerequisites are available:

```sh
cargo fmt --all --check
cargo test --offline --locked --workspace --all-features
cargo test --offline --locked --no-default-features
cargo test --offline --locked --no-default-features --features media
cargo clippy --offline --locked --workspace --all-features --all-targets -- -D warnings
cargo clippy --offline --locked --no-default-features --all-targets -- -D warnings
cargo clippy --offline --locked --no-default-features --features media --all-targets -- -D warnings
cargo test --offline --locked --all-features --test cli
cargo test --offline --locked --doc --all-features
cargo doc --offline --locked --workspace --all-features --no-deps
```

The normal suites use synthetic inputs, not private media, credentials or live
reference downloads. Keep opt-in private-media/model/live-network diagnostics
ignored; do not use an unrestricted ignored-test run as the normal check command.
CLI tests invoke the local executable, not external media tools.

Cargo does not run dependency unit tests transitively, but the workspace command
above runs each member's suites, including registry cross-parser oracles.
[tests/mp4_demux.rs](../tests/mp4_demux.rs) additionally bridges the local demux
unit module; [tests/pgs_bounded.rs](../tests/pgs_bounded.rs) bridges four local PGS
acquisition oracles. These keep the targeted contracts in root-only tests too.
Adapter tests separately exercise the compiled local dependencies.

See the [regression map](REGRESSIONS.md) for source-named tests and limitations.
Report command, toolchain, target, feature set, ignored tests and failures rather
than treating a test-count total as acceptance proof.

## Changes and bug reports

Keep container parsing in the local media-core-derived crates and application
evidence policy in tvmatch. Preserve
fail-closed bounds, raw evidence/provenance, independent identities and separate
show/fallback/rename consent. Prefer small synthetic regressions that reproduce the
failure through the actual public/dependency path; do not weaken matching thresholds
to make a reference win.

Do not attach private subtitle bodies, media, credentials, signed URLs, personal
paths or cache dumps to public issues. Provide a minimal original/rights-cleared
fixture and sanitized error, expected behavior and reproduction command. Media
metadata is unverified; a filename is not an episode oracle.

A proposed dependency or runtime exception needs a demonstrated capability gap,
alternatives, target/build/runtime requirements, opt-out behavior, resource evidence
and license/redistribution review. Pure Rust does not itself prove security,
compatibility, static-runtime deployment or content rights. See
[publication decisions](PUBLICATION.md) before distributing code or binaries.
