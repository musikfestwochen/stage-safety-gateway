# Stage Safety Gateway

Rust gateway between a Broadweigh BW-WSS/T24-BSi wind sensor and the
[Musikfestapp](https://github.com/musikfestwochen/musikfestapp) Stage Safety API.

The repository currently contains the verified, dependency-free T24 value-frame
decoder. Serial I/O and HTTP forwarding will follow the corrected ingestion
contract in [musikfestapp#195](https://github.com/musikfestwochen/musikfestapp/issues/195).

## Documentation

- [Protocol specification](docs/protocol.md)
- [Reverse-engineering evidence](docs/reverse-engineering.md)
- [Final capture report](docs/final-capture.md)
- [Final tested sensor configuration](docs/reference/BW-WSS-FD25D5-final.tcf)

## Development

```sh
cargo test
```

The decoder accepts an arbitrary byte stream, resynchronizes after noise or
invalid frames, validates Modbus CRC-16, and returns only valid BW-WSS float
Data Provider frames.

This software is monitoring and advisory software, not fail-safe safety
control.
