# Releasing CLT

CLT 0.6.15 bundles its patched Turso 0.7.2 engine, SDK kit, and Rust API in
**one `clt-rs` package**. There are no separate fork crates to publish. The
library target compiles the source under `vendor/` directly; normal crates.io
dependencies supply the unmodified parser, extension, and proc macros.
See [the vendor notes](../vendor/README.md) for patch and update details.

## Verify before publication

Use a recent stable Rust toolchain and Python 3.11 or newer. From the repository
root, run:

```sh
rustfmt --edition 2024 --check build.rs src/main.rs src/lib.rs tests/architecture.rs tests/cli.rs
cargo clippy --package clt-rs --no-deps --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
python3 scripts/check_release.py
```

Formatting and strict lints apply to CLT; the bundled engine retains upstream
formatting and lint policy. Its library keeps Rust 2021 while CLT uses Rust 2024.
Cargo currently accepts this target edition override with a deprecation warning.

During preparation, use `python3 scripts/check_release.py --allow-dirty`.
Before publishing, commit the reviewed changes and repeat the check from that
clean checkout. Keep the release version and dated changelog section in sync.

The release check runs `cargo package --locked` with verification enabled. Cargo
builds the extracted archive using its normalized published manifest. The script
then verifies that the archive contains the tested application and engine source,
the upstream licenses and provenance, and the compile-time WAL patch marker. It
rejects separate Turso core/SDK/fork dependencies, nested Cargo packages, and
unexpected vendor directories. The four upstream Turso leaf dependencies remain
pinned to 0.7.2.

Finally, the check runs the archive-built native binary against temporary task
and registry directories. It checks version output and registry persistence
across separate registration, status, and pause commands. No services are started
or stopped. CI runs these checks on Linux and macOS.

## Publish

After verification, publish the single package:

```sh
cargo publish --locked
```

Future engine patch changes ship with the next CLT version in the same way.
Do not restore a local `[patch.crates-io]` override: Cargo does not preserve that
override for users installing the published package.

## Check the public installation

After publication, test both installation modes outside this repository, using
separate temporary Cargo/install directories:

```sh
CARGO_HOME="$(mktemp -d)" cargo install clt-rs --version 0.6.15 --locked --root /tmp/clt-release-locked
CARGO_HOME="$(mktemp -d)" cargo install clt-rs --version 0.6.15 --root /tmp/clt-release-unlocked
/tmp/clt-release-locked/bin/clt --version
/tmp/clt-release-unlocked/bin/clt --version
```

Run registry smoke checks against a temporary `CLT_AGENT_STATE_DIR`. Test explicit
recovery in an isolated OS user/service environment: a separate state directory
alone does not isolate the per-user scheduler service that recovery stops.

Users upgrade with `cargo install clt-rs --locked --force`. Users already in
recovery-required state should close CLT TUIs, install the fixed release, run
`clt agent recover`, review the result, and use `clt agent start` when ready.
Keep the quarantined database bundles.
