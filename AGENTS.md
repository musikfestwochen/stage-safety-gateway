# Project Guide

## Security audit skill

Use `.agents/skills/security-audit/SKILL.md` for security guidance and audits. Skills CLI owns this skill; do not edit its upstream files.

Before starting an audit, run:

```sh
npx --yes skills@1.7.6 check security-audit --project
```

Confirm output explicitly says the project skill is up to date. Missing skill/lock entries, network errors, or other check failures mean freshness is unknown; report the blocker and pause the audit. If an update exists, pause and refresh:

```sh
npx --yes skills@1.7.6 update security-audit --project --yes
node --test .agents/skills/security-audit/validate-findings.test.cjs .agents/skills/security-audit/validate-coverage-ledger.test.cjs
```

Review changed skill files and `skills-lock.json`, then reload the skill and its companion instructions before starting the audit. Keep audit reports outside this repository unless explicitly instructed otherwise. Follow the skill's sandbox requirements before executing target code.

The `Security audit skill` workflow checks updates weekly or on manual dispatch and opens a draft update PR. Upstream attribution is in `.github/security-audit-LICENSE`.

## Structure

- This is one Cargo package with both library and binary targets.
- `src/lib.rs` exposes T24 stream/frame decoding. `src/config.rs` owns TOML types, validation, persistence, and redacted display. `src/main.rs` owns CLI parsing and interactive terminal flows.
- `docs/protocol.md` is decoder specification; `docs/reverse-engineering.md` records supporting evidence. Update specification and decoder together when protocol understanding changes.

## Protocol Conventions

- Treat serial input as arbitrary byte stream, never capture lines or complete reads. Resynchronize one byte at a time after invalid frames and retain trailing `0B` for next read.
- Reject frames with invalid CRC, packet type, or data type before exposing values. Do not hardcode sensor tags.
- BW-WSS base tag carries average; optional gust uses `data_tag + 1`. Validate collisions across both slots, including `FFFF` overflow.
- Unit comes from config because packet does not identify all configured units. Track average and gust independently because either transmission may be lost.

## Configuration And Secrets

- Serde models reject unknown fields. Keep `Config::read` parse-only so wizard can repair invalid config; use `Config::load` for validated runtime config.
- Config contains API tokens. Never print tokens in output, fixtures, debug formatting, or errors; use existing redacted summaries.
- Preserve Unix `0600` creation and permission tightening in `Config::save`.

## Tests And Verification

- Keep module-level unit tests beside implementation under `#[cfg(test)]`; use `tests/` for integration tests through public API.
- `tests/decoder.rs` embeds `tests/fixtures/bw-wss-mps.hex`, real 316-frame capture. Keep fixture as contiguous hexadecimal text.
- Run one test with `cargo test <test_name>` and decoder integration test with `cargo test --test decoder`.
- Behavior changes require focused test additions or updates. Before PR, match CI order: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets`, `cargo publish --dry-run --allow-dirty`.
- CI uses stable Rust with rustfmt and clippy components.

## Git Workflow

- Use Conventional Commits, for example `feat: add serial reader` or `fix: retain partial frame`.
- Name branches `<type>/<short-slug>`, for example `feat/add-config-cli` or `fix/parser-resync`.
- Merge PRs with merge commits; do not squash or rebase-merge.
- release-plz handles versions, changelog entries, tags, and publishing from `main`; do not edit release metadata manually unless release task requires it.
- For PR creation or description work, follow `.ai/skills/pr-description/SKILL.md`; create draft PRs unless user asks otherwise.
