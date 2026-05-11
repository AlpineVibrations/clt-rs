# Changelog

All notable changes to this project are documented in this file.

This project follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) style sections and uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html) for releases.

## [Unreleased]

Use this section while developing the next release.

### Added

### Changed

### Fixed

## [0.1.10] - 2026-05-11

### Added

- Added scroll handling in TUI task columns so keyboard navigation keeps the selected task visible.
- Added TUI task editing, deletion, help popover, console feedback, cursor-aware text input, and wrapping for selected long tasks.
- Added CLI deletion support and single-status task listing.
- Added support for unquoted multi-word task descriptions in `clt add`.
- Added `clt-skill.md` guidance for agent task workflows.

### Changed

- Made the TUI Kanban board the default when running `clt` with no subcommand.
- Made completed tasks appear at the top of the Done column.
- Updated README usage examples to match the current CLI behavior.

### Fixed

- Fixed stale or empty TUI selections causing add, edit, navigation, and move panics.
- Fixed terminal cleanup so raw mode and alternate screen are restored on TUI error paths.
- Fixed task moves so destination write failures do not remove the source task.
- Fixed TUI navigation on empty boards.

[Unreleased]: https://github.com/AlpineVibrations/clt-rs/compare/v0.1.10...HEAD
[0.1.10]: https://github.com/AlpineVibrations/clt-rs/releases/tag/v0.1.10
