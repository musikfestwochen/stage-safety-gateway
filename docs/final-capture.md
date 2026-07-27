# Final BW-WSS capture

Inputs:

- [`reference/BW-WSS-FD25D5-final.tcf`](reference/BW-WSS-FD25D5-final.tcf)
- Backed-up raw serial capture and decoded CSV (not published in this repository)

The normative parsing rules are in [`protocol.md`](protocol.md).

## Final configuration

| Setting | Value |
|---|---:|
| Base Data Tag | `25DF` |
| Average tag | `25DF` |
| Gust tag | `25E0` |
| Engineering unit | m/s (`Gain=1.1176`) |
| Transmit interval | 5000 ms |
| Average sample period | 10000 ms |
| Gust period | 10 s (`Settings=1`) |
| Factory gain | 1.002911 |
| Battery-low threshold | 2.2 V |
| Status snapshot | 28 (`0x1C`) |

## Parse result

| Check | Result |
|---|---:|
| Capture span | 07:41:25.724–08:09:40.789 |
| Valid frames | 663 |
| CRC failures/discarded bytes | 0 |
| Incomplete trailing bytes | 0 |
| Frame lengths | 16 bytes only |
| Packet types | `0x23` only: broadcast Data Provider |
| Status values | `0x1C` only: power-up + undocumented bits 2–3 (empirically correlated with the m/s preset) |
| Data types | `0x04` only: big-endian float32 |
| Average packets (`25DF`) | 329 |
| Gust packets (`25E0`) | 334 |

No packet reported low battery, input integrity error, general error, shunt calibration, or digital I/O activity.

## Values and radio metadata

| Field | Minimum | Maximum |
|---|---:|---:|
| Average wind (`25DF`) | 0.3207388 m/s | 2.4767122 m/s |
| Gust (`25E0`) | 0.3343068 m/s | 2.6744547 m/s |
| RSSI | -77 dBm | -64 dBm |
| CV | 79 | 108 |

Every gust value is an integer multiple of:

```text
Gain / FactoryGain / gust period
= 1.1176 / 1.002911 / 10
= 0.1114356109 m/s
```

Observed gust counts ranged from 3 to 24 steps; maximum floating-point residual from an integer count was below `0.000002` count.

## Delivery gaps

The median interval for both tags was 5.009 seconds. Across the common capture span, the cadence implies 339 scheduled messages per tag:

| Tag | Received | Inferred absent | Delivery |
|---|---:|---:|---:|
| Average `25DF` | 329 | 10 | 97.05% |
| Gust `25E0` | 334 | 5 | 98.53% |
| Combined | 663 | 15 | 97.79% |

Each inferred absence appears as one approximately 10-second gap rather than the normal approximately 5-second interval. There are no malformed or CRC-invalid bytes at those positions, so these are missing whole transmissions/deliveries, not partially captured frames. Consumers must track the two tags independently and tolerate packet loss.

## Reproduce

The raw capture is intentionally not published. Run the retained captured-frame
regression check with `cargo test`.
