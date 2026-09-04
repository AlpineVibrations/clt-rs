# Vendored Turso core

`turso_core/` contains the published `turso_core` **0.7.0** crate, copied from
Cargo's crates.io source cache. The package's `.cargo_vcs_info.json` identifies
upstream revision `e7cb62a8bd2f3655a661a621ee389365c1a1e43e`, directory `core`, in
[tursodatabase/turso](https://github.com/tursodatabase/turso/tree/e7cb62a8bd2f3655a661a621ee389365c1a1e43e/core).
The original crates.io archive checksum is
`a77f2106de5a3014261be18283999fde0d06c24ae5d4cb85a6eff1aaeaff453d`.

The published source, manifests, build script, benchmarks and tests are retained.
Only Cargo's local `.cargo-ok` extraction marker was omitted. `LICENSE` contains
the upstream [MIT license at that same revision](https://github.com/tursodatabase/turso/blob/e7cb62a8bd2f3655a661a621ee389365c1a1e43e/LICENSE.md),
which was not included in the published crate's extracted files.

CLT's root `[patch.crates-io]` selects this local package, including for
`cargo install --path . --locked`. All other dependency versions remain pinned
by the root `Cargo.lock`.

## Local change

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

Remove the patch only after a released upstream version contains the equivalent
reader ownership fix and passes these regressions. Do not replace it by removing
CLT's checkpoint pin or discarding the WAL.
