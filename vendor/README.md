# Bundled Turso engine

CLT publishes these three source directories inside its single `clt-rs` crate:

| Directory | Purpose |
| --- | --- |
| `turso_core` | Patched Turso 0.7.2 database engine; CLT's `clt_database` library target |
| `turso_sdk_kit` | SDK support compiled as a module of that library |
| `turso` | Rust database API compiled as a module of that library |

These are source directories, not nested Cargo packages. The root manifest lists
all required dependencies, and the root build script supplies the original
engine feature configuration and version metadata. There are no fork packages
or local Cargo patches to publish. Both locked and unlocked `cargo install
clt-rs` builds compile the bundled engine. Unmodified `turso_parser`, `turso_ext`,
`turso_macros`, and `turso_sdk_kit_macros` remain registry dependencies pinned to
`=0.7.2`.

The unused sync engine and sync SDK, Rust API sync module, standalone benches,
examples, integration test directories, bindgen helper, and separate package
build scripts have been removed. Platform-specific engine implementations remain
for Linux, macOS, and Windows. The SDK's C API and generated Rust bindings are
still needed by its Rust API.

## Source provenance and integration

The sources come from the published 0.7.2 crates. `UPSTREAM_Cargo.toml` and
`UPSTREAM_VCS_INFO.json` preserve each original manifest and upstream revision
`046e9cbf67d22491e8ecc941ec2891b02a9f3cad` in
[tursodatabase/turso](https://github.com/tursodatabase/turso/tree/046e9cbf67d22491e8ecc941ec2891b02a9f3cad).
Each directory retains an MIT `LICENSE`; the license text is retained from
[the upstream 0.7.0 source](https://github.com/tursodatabase/turso/blob/e7cb62a8bd2f3655a661a621ee389365c1a1e43e/LICENSE.md),
as the published archives omitted that file.

Original crates.io archive SHA-256 checksums:

| Package | SHA-256 |
| --- | --- |
| `turso_core` | `7a833cc3bf8d4e6c101c504fa470f8ab4270c2202ff2591b61b2e373b4f20d9b` |
| `turso_sdk_kit` | `18c1dc1c0304348c39b97bc6b27cdcb1d7292454ebd0de0f30b5ee3a4c61f9bb` |
| `turso` | `f9491d7a80312c5abe66a4409e4dce02065503a235453c94b9e4133877e39ffc` |

The engine keeps its upstream Rust 2021 edition and formatting; CLT's binary
uses Rust 2024. Cargo currently warns that per-target editions are deprecated.
The core remains at the library crate root so upstream macros and internal
imports keep working. SDK/API imports are adjusted to their module locations.
Engine `feature` checks use the private `clt_turso_feature` cfg, with the original
production feature set enabled by `build.rs`. This keeps CLT's `--all-features`
from enabling upstream experimental modes. Unrelated upstream test harnesses use
`clt_turso_tests` and are disabled; the mapped shared-WAL regression tests still
run normally alongside CLT's recovery tests. Those harness sources are retained
for comparison with upstream, without pulling their development dependencies.

CLT requires `CLT_WAL_PATCH_LEVEL` at compile time. The
[release check](../scripts/check_release.py) builds the extracted single archive,
rejects a dependency on a separate engine, and compares all packaged Rust source
with the checked-out source. See [the release procedure](../docs/RELEASING.md).
When updating Turso, reapply these module/configuration adaptations, review its
original dependency/features list, and run the WAL regressions and archive check.

## Local changes

The published 0.7.2 shared-WAL coordination source is unchanged from 0.7.0, so
CLT retains the same reader ownership fix and checkpoint pin. The versioned
shared-WAL header layout used by CLT's recovery workaround is also unchanged.

`storage/shared_wal_coordination.rs` fixes
`repair_transient_state_for_exclusive_open`: repair holds the local reader mutex
through probing and reclamation and skips every slot with a positive local reader
count. Linux OFD lock probes on the same open file description otherwise succeed
even when a sibling connection owns the reader, allowing repair to erase that
live owner's metadata. The later release then panics with
`reader slot released by non-owner`. WAL scans after an uncommitted or partial
trailing frame can trigger this repair while CLT's checkpoint reader remains live.

The existing non-Linux process ownership check and cross-process byte-lock probes
are retained. Repair still reclaims dead readers and preserves the durable frame
index. A focused core regression covers a reader on the same mapping, shared
snapshot references, an older pinned frame and normal reader release, using both
native and process-scoped mappings. CLT's integration regressions cover trailing
WAL data and overlapping registry users.

Reader release also keeps the OS byte lock until shared owner, frame and bitmap
cleanup is complete. The upstream release unlocked first; a peer could reclaim
the slot before the old owner cleared it, causing `shared owner slot released by
non-owner` on macOS or overwriting a successor reader's metadata on Linux. The
same-process ownership reservation remains held through the unlock. A deterministic
regression inserts a successor at the unlock boundary and verifies that the old
release cannot erase it, using native and process-scoped mappings.

Coordination reseeding now acquires both checkpoint and writer locks, reloads the
shared snapshot under those locks, and retains them until the index is published.
The previous idle-lock probe allowed a writer to start before repair trimmed the
frame index, causing `shared WAL frame index length changed while publishing an
entry`. Readers that cannot acquire the locks adopt the current shared snapshot
without changing its index. Transient reader repair preserves the guard's owner
metadata, and the guard releases both locks on return or unwind. Regressions cover
writer/checkpoint exclusion with native and process-scoped mappings, interrupted
repair, and concurrent connection opens during 2,000 registry writes.

Disk-scan reconciliation is serialized across local connection opens and consumes
`loaded_from_disk_scan` once. After adopting another process's commit metadata,
the local frame cache is no longer a complete disk scan. Reusing that flag on a
later connection could rebuild the shared index from old frames under a newer
header, resurrecting expired leases and allowing stale page writes to damage
tables and indexes. A CLT regression keeps an exclusive reopened store idle
after a partial checkpoint while a peer writes, then verifies repeated fresh
reads, lease replacement, project disable, reopen, and full integrity.

Return to an upstream engine dependency only after a released version contains
the equivalent ownership and reseeding fixes and passes these regressions. Keep
CLT's checkpoint pin and retained WAL data when making that transition.
