Automatically resume interrupted registry recovery and accept current manifests [BUG]

COMPLETED 2026-10-06: Resume repairs from the existing DB/WAL quarantine; share snapshot version validation, accept v3, preserve dirty evidence and process fences, and support the recovery alias. Include operation-scoped WAL checkpointing and release/install CLT 0.8.1. Recovered the live registry, retained all 23 projects, and reduced its active WAL from 128 MiB to 32 bytes. Background service left stopped.

Validation: formatting and strict Clippy passed; 47 engine tests, 706 application tests plus the large-WAL stress test passing in isolation after a full-suite timeout (one ignored test), 4 architecture tests, and 21 CLI tests passed. Verified automatic recovery against a private copy before applying it to the live registry; verified the installed version and stopped launchd service.

clt:stopped
