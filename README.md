# Stage Safety Gateway

[![Crates.io](https://img.shields.io/crates/v/stage-safety-gateway)](https://crates.io/crates/stage-safety-gateway)
[![docs.rs](https://img.shields.io/docsrs/stage-safety-gateway)](https://docs.rs/stage-safety-gateway)
[![License](https://img.shields.io/crates/l/stage-safety-gateway)](LICENSE)
[![Status](https://img.shields.io/badge/status-in%20development-orange)](#)

Gateway between stage-safety sensors (Broadweigh / Mantracourt) and the
[Musikfestapp](https://github.com/musikfestwochen/musikfestapp) Stage Safety API.
Currently supports the BW-WSS wind sensor via a T24 base station.

The gateway decodes T24 serial frames, applies its configured send policy, and
forwards normalized readings to the Musikfestapp Stage Safety API.

## Installation

Install a current stable [Rust toolchain](https://rustup.rs/), then:

```sh
cargo install stage-safety-gateway
stage-safety-gateway --version
```

On Linux, the service user must be allowed to open the serial device. This
commonly means membership in the `dialout` group; exact group depends on the
distribution and device permissions.

## Usage

```sh
stage-safety-gateway config           # interactive setup wizard
stage-safety-gateway config validate  # check config, print redacted summary
stage-safety-gateway listen           # decode serial (or --stdin) and print readings
stage-safety-gateway run              # run the serial-to-HTTP gateway
stage-safety-gateway run --verbose    # also log readings and HTTP attempts
```

`--config <path>` selects a non-default config file and works with every
command. `listen` is diagnostic only and makes no HTTP requests. `run` stays in
the foreground for service-manager use.

## Quick Start

Create config with the wizard, verify it, then start gateway:

```sh
stage-safety-gateway config
stage-safety-gateway config validate
stage-safety-gateway run
```

Equivalent minimal config:

```toml
[serial]
port = "/dev/ttyUSB0"
baud_rate = 115200

[aggregation]
change_percent = 20.0
min_interval_secs = 30
max_interval_secs = 300

[[sensor]]
type = "bw-wss"
name = "WINDMESSER01"
id = "FD25D5"
unit = "m/s"
data_tag = "25DF"
gust = true
average_window_secs = 10
gust_window_secs = 10
url = "https://musikfestapp.ch/stage-safety/readings"
token = "replace-with-api-token"
```

Full config constraints and operational behavior live in documentation below.

## Documentation

- [Configuration reference](https://github.com/musikfestwochen/stage-safety-gateway/blob/main/docs/configuration.md)
- [Operations guide](https://github.com/musikfestwochen/stage-safety-gateway/blob/main/docs/operations.md)
- [Protocol specification](https://github.com/musikfestwochen/stage-safety-gateway/blob/main/docs/protocol.md)
- [Reverse-engineering evidence](https://github.com/musikfestwochen/stage-safety-gateway/blob/main/docs/reverse-engineering.md)
- [Final capture report](https://github.com/musikfestwochen/stage-safety-gateway/blob/main/docs/final-capture.md)
- [Final tested sensor configuration](https://github.com/musikfestwochen/stage-safety-gateway/blob/main/docs/reference/BW-WSS-FD25D5-final.tcf)

## Development

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo publish --dry-run --allow-dirty
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
