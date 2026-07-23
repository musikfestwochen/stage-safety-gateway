# Stage Safety Gateway

Rust gateway between a Broadweigh BW-WSS/T24-BSi wind sensor and the
[Musikfestapp](https://github.com/musikfestwochen/musikfestapp) Stage Safety API.

The repository contains the verified, dependency-free T24 value-frame decoder.
Serial I/O, aggregation, and HTTP forwarding follow the ingestion contract in
[musikfestapp#195](https://github.com/musikfestwochen/musikfestapp/issues/195)
and are tracked in this repository's epic issue.

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

The regression fixture in [`tests/fixtures/bw-wss-mps.hex`](tests/fixtures/bw-wss-mps.hex)
is a real 316-frame m/s recording stored as contiguous hexadecimal bytes.

## Releasing

Releases are automated with [release-plz](.github/workflows/release-plz.yml):
merging to `main` keeps a release PR up to date; merging that PR publishes to
crates.io. Requires a `CARGO_REGISTRY_TOKEN` repository secret (crates.io API
token with `publish-update` scope for this crate).

The decoder accepts an arbitrary byte stream, resynchronizes after noise or
invalid frames, validates Modbus CRC-16, and returns only valid BW-WSS float
Data Provider frames.

This software is monitoring and advisory software, not fail-safe safety
control.
