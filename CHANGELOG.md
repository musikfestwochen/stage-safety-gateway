# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.0](https://github.com/musikfestwochen/stage-safety-gateway/compare/v0.1.3...v1.0.0) - 2026-07-25

### Added

- add gateway run daemon
- add aggregated send policy
- add stage safety HTTP client
- edit sensor wizard, rssi_dbm unit, low_bat label

### Fixed

- redact URLs from HTTP errors
- harden gateway runtime safeguards
- edit-sensor token default + test comment typo

### Other

- add release readiness guides
- verify high-frequency aggregation

## [0.1.3](https://github.com/musikfestwochen/stage-safety-gateway/compare/v0.1.2...v0.1.3) - 2026-07-24

### Fixed

- listen serial timeout, Ctrl+C exit, and silent idle

## [0.1.2](https://github.com/musikfestwochen/stage-safety-gateway/compare/v0.1.1...v0.1.2) - 2026-07-24

### Added

- serial ingestion + listen subcommand

### Fixed

- listen diagnostics to stderr + binary name in help, README in-development badge

### Other

- add pull request description skill
- add project agent guide

## [0.1.1](https://github.com/musikfestwochen/stage-safety-gateway/compare/v0.1.0...v0.1.1) - 2026-07-24

### Other

- Reject inknown fields
- Reject non-finite change_percent; use display_name in details
- Use sensor display name in messages + share tag-occupied check with wizard
- Address review: gust-tag validation, 0600 config creation, wizard repair flow
- Add config model and interactive config CLI
- release v0.1.0

## [0.1.0](https://github.com/musikfestwochen/stage-safety-gateway/releases/tag/v0.1.0) - 2026-07-23

### Other

- Add crates.io metadata, CI, release-plz, and binary stub
- Add GPLv3 license and serial fixture
- Initialize Stage Safety gateway
