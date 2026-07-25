<p align="center">
  <img src="https://raw.githubusercontent.com/musikfestwochen/stage-safety-gateway/main/art/logo.png" alt="Stage Safety Gateway" width="860">
</p>

<p align="center">
  <strong>Reliable serial-to-HTTP bridge for stage-safety sensors.</strong><br>
  Broadweigh / Mantracourt T24 frames in, canonical Musikfestapp readings out.
</p>

<p align="center">
  <a href="https://crates.io/crates/stage-safety-gateway"><img src="https://img.shields.io/crates/v/stage-safety-gateway?style=flat-square" alt="Crates.io"></a>
  <a href="https://docs.rs/stage-safety-gateway"><img src="https://img.shields.io/docsrs/stage-safety-gateway?style=flat-square" alt="docs.rs"></a>
  <a href="https://github.com/musikfestwochen/stage-safety-gateway/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/musikfestwochen/stage-safety-gateway/ci.yml?branch=main&style=flat-square&label=CI" alt="CI"></a>
  <a href="https://github.com/musikfestwochen/stage-safety-gateway/blob/main/LICENSE"><img src="https://img.shields.io/crates/l/stage-safety-gateway?style=flat-square" alt="License"></a>
</p>

Stage Safety Gateway decodes verified T24 serial frames, filters configured
sensors, applies an independent average/gust send policy, and forwards readings
to the [Musikfestapp](https://github.com/musikfestwochen/musikfestapp) Stage
Safety API. Currently supports the BW-WSS wind sensor through a T24 base station.

## Quick Start

Install a current stable [Rust toolchain](https://rustup.rs/), then:

```sh
cargo install stage-safety-gateway
stage-safety-gateway config
stage-safety-gateway config validate
stage-safety-gateway run
```

On Linux, service user needs access to serial device, commonly through
`dialout` group. Use global `--config <path>` option to select non-default
config file.

## Commands

| Command | Purpose |
| --- | --- |
| `stage-safety-gateway config` | Create or edit config interactively. |
| `stage-safety-gateway config validate` | Validate config and print redacted summary. |
| `stage-safety-gateway listen` | Decode configured serial readings without HTTP. |
| `stage-safety-gateway listen --stdin` | Decode raw binary input from stdin. |
| `stage-safety-gateway run` | Run foreground serial-to-HTTP gateway. |
| `stage-safety-gateway run --verbose` | Also log readings, policy decisions, and HTTP attempts. |

## Configuration

Wizard is recommended. Minimal equivalent TOML:

<details>
<summary>Show example</summary>

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

</details>

## Documentation

| Guide | Contents |
| --- | --- |
| [Configuration reference](https://github.com/musikfestwochen/stage-safety-gateway/blob/main/docs/configuration.md) | Every TOML key, default, and constraint. |
| [Operations guide](https://github.com/musikfestwochen/stage-safety-gateway/blob/main/docs/operations.md) | Reliability, logging, systemd, security, and fixture replay. |
| [Protocol specification](https://github.com/musikfestwochen/stage-safety-gateway/blob/main/docs/protocol.md) | Verified T24 frame format and decoder rules. |
| [Reverse-engineering evidence](https://github.com/musikfestwochen/stage-safety-gateway/blob/main/docs/reverse-engineering.md) | Captures and evidence behind protocol. |
| [Final capture report](https://github.com/musikfestwochen/stage-safety-gateway/blob/main/docs/final-capture.md) | Validated BW-WSS recording. |
| [Sensor reference config](https://github.com/musikfestwochen/stage-safety-gateway/blob/main/docs/reference/BW-WSS-FD25D5-final.tcf) | Final tested toolkit configuration. |

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo publish --dry-run --allow-dirty
```

Captured-frame integration tests replay a real 316-frame recording. Decoder
accepts arbitrary byte streams, resynchronizes after noise, validates Modbus
CRC-16, and exposes only valid BW-WSS float Data Provider frames.

> **Safety:** Monitoring and advisory software only. Not fail-safe safety
> control.
