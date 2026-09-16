Expose the CLT version and document installation with required registry fixes (BUG, CLI)

Completion note:
COMPLETED 2026-09-16: Added package-derived `clt --version` and `clt -V` output and corrected the CLI display name to `clt`. Updated installation guidance to use repository builds containing the required Turso patches and documented that Cargo packaging removes those patches. Verified both flags with an unusable registry path, help output, 14 CLI integration tests, 16 CLI unit tests, the existing partial/uncommitted WAL-tail regression, formatting, and diff checks. The reported Linux recovery completed, but its panic path identifies the unpatched crates.io Turso source; the affected computer still needs a repository-built binary. Changes are not installed, committed, or pushed.
