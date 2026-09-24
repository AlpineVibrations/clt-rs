# Repository guidance

## Documentation

- Keep [README.md](README.md) short: project overview, installation, quick start,
  and links to further documentation.
- Put new feature descriptions, detailed behavior, configuration, TUI controls,
  and troubleshooting in [features.md](features.md). Update its contents links
  when adding sections; do not append feature reports to README.
- Record user-facing changes under `Unreleased` in [CHANGELOG.md](CHANGELOG.md).
- Keep implementation boundaries and verification commands in
  [ARCHITECTURE.md](ARCHITECTURE.md), and publishing steps in
  [docs/RELEASING.md](docs/RELEASING.md).
- Keep linked user documentation in the Cargo package's include list.
