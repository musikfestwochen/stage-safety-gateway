# Stage Safety Gateway

[![Crates.io](https://img.shields.io/crates/v/stage-safety-gateway)](https://crates.io/crates/stage-safety-gateway)
[![docs.rs](https://img.shields.io/docsrs/stage-safety-gateway)](https://docs.rs/stage-safety-gateway)
[![License](https://img.shields.io/crates/l/stage-safety-gateway)](LICENSE)
[![Status](https://img.shields.io/badge/status-in%20development-orange)](#)

Gateway between stage-safety sensors (Broadweigh / Mantracourt) and the
[Musikfestapp](https://github.com/musikfestwochen/musikfestapp) Stage Safety API.
Currently supports the BW-WSS wind sensor via a T24 base station.

The repository contains the verified T24 value-frame decoder. The gateway is
under active development; not all components documented below are implemented
yet.

## Installation

```sh
cargo install stage-safety-gateway
```

## Usage

```sh
stage-safety-gateway config           # interactive setup wizard
stage-safety-gateway config validate  # check config, print redacted summary
stage-safety-gateway listen           # decode serial (or --stdin) and print readings
stage-safety-gateway run              # start the gateway (not implemented yet)
```

Config lives in the platform config dir (Linux: `~/.config/stage-safety-gateway/config.toml`);
override with `--config <path>`. One TOML file holds serial settings, the
aggregation policy, and any number of type-tagged `[[sensor]]` entries.

## Documentation

- [Protocol specification](docs/protocol.md)
- [Reverse-engineering evidence](docs/reverse-engineering.md)
- [Final capture report](docs/final-capture.md)
- [Final tested sensor configuration](docs/reference/BW-WSS-FD25D5-final.tcf)

## Development

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

The integration test in [`tests/`](tests) decodes
[`tests/fixtures/bw-wss-mps.hex`](tests/fixtures/bw-wss-mps.hex), a real
316-frame m/s recording stored as contiguous hexadecimal bytes, through the
public API.

The decoder accepts an arbitrary byte stream, resynchronizes after noise or
invalid frames, validates Modbus CRC-16, and returns only valid BW-WSS float
Data Provider frames.

This software is monitoring and advisory software, not fail-safe safety
control.
