# Vendored Turso core

`turso_core/` contains the published `turso_core` **0.7.2** crate, copied from
Cargo's crates.io source cache. The package's `.cargo_vcs_info.json` identifies
upstream revision `046e9cbf67d22491e8ecc941ec2891b02a9f3cad`, directory `core`, in
[tursodatabase/turso](https://github.com/tursodatabase/turso/tree/046e9cbf67d22491e8ecc941ec2891b02a9f3cad/core).
The original crates.io archive checksum is
`7a833cc3bf8d4e6c101c504fa470f8ab4270c2202ff2591b61b2e373b4f20d9b`.

The published source, manifests, build script, benchmarks and tests are retained.
Only Cargo's local `.cargo-ok` extraction marker was omitted. `LICENSE` contains
the upstream [MIT license retained from 0.7.0](https://github.com/tursodatabase/turso/blob/e7cb62a8bd2f3655a661a621ee389365c1a1e43e/LICENSE.md),
which was not included in the published crate's extracted files.

CLT's root `[patch.crates-io]` selects this local package, including for
`cargo install --path . --locked`. All other dependency versions remain pinned
by the root `Cargo.lock`.

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

Remove the patch only after a released upstream version contains the equivalent
reader ownership fixes and passes these regressions. Do not replace it by removing
CLT's checkpoint pin or discarding the WAL.
