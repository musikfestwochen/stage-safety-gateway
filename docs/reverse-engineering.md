# Broadweigh / T24 serial protocol findings

> **Implementation reference:** [`protocol.md`](protocol.md) defines the final framing and decoding rules. This report preserves the evidence, configuration comparisons, and experiments that established them. Raw captures and generated CSV files were intentionally omitted from the gateway repository after the results were verified.
>
> **Final dataset:** [`final-capture.md`](final-capture.md) summarizes the final settings, decoded values, radio metadata, and delivery gaps.

## Result

The captured messages are standard Mantracourt T24 **Data Provider** transport packets. It is not correct to treat `0B 0B 01` as a fixed header: the first two bytes are duplicated packet lengths, followed by the base-station address. The second experiment resolves status bit 4; only status bits 2 and 3 remain device-specific/undocumented.

All 234 packets in `raw_serial_output.log` pass the documented Modbus CRC-16. All 150 rows in `broadweigh_log.csv` match the raw tag and float value. The utility clock is one hour ahead of hTerm; the residual timestamp difference is 0–18 ms.

## Captured 16-byte frame

Example:

```text
0B 0B 01 23 25 D5 10 04 3F EB 72 40 E4 E7 25 DB
│  │  │  │  └─tag─┘ │  │  └──float32──┘ │  │  └CRC┘
│  │  │  │          │  │                 │  └─ CV
│  │  │  │          │  │                 └──── RSSI
│  │  │  │          │  └─ data type
│  │  │  │          └──── status
│  │  │  └─────────────── packet type/flags
│  │  └────────────────── base-station address
└──┴───────────────────── duplicated length
```

| Offset | Captured bytes | Meaning | Interpretation |
|---:|---|---|---|
| 0 | `0B` | Length | 11 bytes from packet type (offset 3) through CV (offset 13). |
| 1 | `0B` | Duplicate length | Framing uses two identical length bytes, not a constant sync marker. |
| 2 | `01` | Base address | Base station address 1. |
| 3 | `23` | Packet type and flags | Low five bits `03` = Data Provider; bit 5 (`20`) = broadcast. Bit 6 (low battery) and bit 7 (error) are clear. |
| 4–5 | `25 D5` or `25 D6` | Data Tag, big-endian | `25D5` average wind; base tag + 1 = `25D6` gust. |
| 6 | `10` | Status | Bit 4 = power-up (power was applied rather than the device being radio-woken). The export independently reports `Status=16`. See the complete bit table below. |
| 7 | `04` | Data type | Type 4 = four-byte IEEE-754 float. No display/unit flags are set. |
| 8–11 | e.g. `3F EB 72 40` | Value, big-endian | `1.83942413`. The toolkit rounds this to `1.839424`. Values are already in the configured engineering unit (km/h). |
| 12 | e.g. `E4` | RSSI | Signed int8 minus 45: `E4` = -28 - 45 = **-73 dBm**. Capture range: -76 to -70 dBm. |
| 13 | e.g. `E7` | CV | `E7 & 7F` = **103**. Capture range: 94–107; about 55 is poor and 110 is excellent. |
| 14–15 | e.g. `25 DB` | CRC-16, little-endian | Modbus CRC-16 (`poly=A001`, init `FFFF`) over offsets 0–13. Stored value is `DB25`. |

Total frame size is `length + 5`: two length bytes, base address, the length-counted section, and two CRC bytes.

### Status byte

The T24-PA technical manual is applicable because the official WSS manual says the wind sensor is built around the T24-PA pulse acquisition module.

| Bit | Mask | Documented meaning |
|---:|---:|---|
| 0 | `01` | Shunt calibration active |
| 1 | `02` | Input integrity error |
| 2 | `04` | Reserved in T24-PA docs; asserted for the m/s preset and clear for every other offered unit. Always matched bit 3. |
| 3 | `08` | Reserved in T24-PA docs; asserted for the m/s preset and clear for every other offered unit. Always matched bit 2. |
| 4 | `10` | Power-up: power was interrupted/applied, not merely woken from sleep |
| 5 | `20` | Battery low |
| 6 | `40` | Digital input active |
| 7 | `80` | Digital output active |

## What the settings explain

`BW-WSS FD25D5.tcf` agrees with the wire data:

- `DataTag=25D5`; gust is specified by the manual as base tag + 1.
- `TXInterval=1000`; each tag is normally sent about once per second.
- `SampleTime=10000`; average wind uses a rolling 10-second window.
- Gust period is configured to 10 seconds (the export's opaque `Settings=1`).
- `Gain=4.02336`, `FactoryGain=1.002911`, and `PulsesPerRevolution=1`. Every distinct gust value is an integer multiple of `4.02336 / 1.002911 / 10 = 0.401168199 km/h`, exactly explaining its quantisation.
- `Status=16` agrees with wire status `0x10` (power-up).
- The engineering-unit label is not carried explicitly. The float is scaled by `Gain`. Captures 3–6 show status bits 2–3 are both asserted only for the m/s preset; mph, km/h, fps, and knots all use `00`.

The Broadweigh manual says the average is the moving average over the configured sample period. Gust is the maximum rolling-window average seen since the previous transmission. Both are transmitted at `TXInterval`; the gust message disappears when gust is disabled.

## Capture quality and losses

- Raw time span: 14:41:56.532–14:43:53.797.
- 234 valid frames: 116 × `25D5`, 118 × `25D6`.
- Zero discarded bytes, CRC failures, or incomplete trailing bytes, including hTerm lines that split or combine frames.
- Missing `25D5` transmissions at about 14:43:36.757 and 14:43:43.773. `25D6` arrives at both times. The toolkit CSV also omits those two average records.
- CSV span is shorter than the raw capture, hence 84 otherwise-valid raw packets have no CSV row.

## Second configuration and capture

The second export changed seven fields:

| Setting | Capture 1 | Capture 2 | Observed consequence |
|---|---:|---:|---|
| `Status` (information snapshot) | 16 (`10`) | 60 (`3C`) | Wire byte 6 matched in these captures, but capture 3 proves the export can retain a stale value. |
| `TXInterval` | 1000 ms | 10000 ms | Tag transmissions changed from ~1 s to ~10.03 s. |
| `SampleTime` | 10000 ms | 5000 ms | Configured average period became 5 s; framing unchanged. Because this is shorter than the 10 s transmit interval, the manual says the effective value is averaged since the preceding transmission (about 10 s). |
| `DataTag` | `25D5` | `25DF` | Average wire tag became `25DF`; gust correctly carried base + 1 = `25E0`, including the hexadecimal carry. |
| `StartupTime` | 10 s | 3 s | No distinguishable packet field; documented as not applicable to this module. |
| `BattLowLevel` | 2.2 V | 3.3 V | Low-battery flags appeared in both packet byte 3 and status byte 6. |
| `Gain` | 4.02336 | 1.1176 | Exact division by 3.6, proving a km/h → m/s unit change. Capture 4 later proves this also caused status bits 2–3 to become set. |

All 12 second-capture frames pass CRC and match all 12 toolkit CSV records. There are no discarded or corrupt serial bytes. Cadence reveals two radio-level omissions that are absent from both files: gust `25E0` near 15:41:13.94 and average `25DF` near 15:41:23.96. The same one-hour clock offset remains, with 1–19 ms residual delay.

The new constant packet bytes are:

```text
0B 0B 01 63 [25 DF|25 E0] 3C 04 [float] [RSSI] [CV] [CRC]
```

- Packet type `63` = Data Provider (`03`) + broadcast (`20`) + low battery (`40`). The error flag (`80`) remains clear.
- Status `3C` = reserved bit 2 (`04`) + reserved bit 3 (`08`) + power-up (`10`) + battery low (`20`). The `20` meaning is proven independently by both the raised 3.3 V threshold and packet-type low-battery flag. Bits 2–3 cannot be attributed because several settings changed together; the applicable T24-PA manual calls them reserved.
- The new gust values are integer multiples of `1.1176 / 1.002911 / 10 = 0.111435611 m/s`. This proves the gust period remains 10 s (`Settings=1`) while `SampleTime=5000` changed only the average window.
- RSSI is stronger in capture 2 (-62 to -53 dBm versus -76 to -70 dBm) and CV is 103–108. These are reception conditions, not configuration fields.

`broadweigh_decoded_2.csv` contains the complete second decode.

## Third configuration: isolated battery-threshold test

Version 3 changed exactly one configurable field from version 2:

```text
BattLowLevel: 3.3 V -> 2.21 V
```

All six frames pass CRC and match all six toolkit rows. The result isolates the battery flags:

```text
capture 2: 0B 0B 01 63 [tag] 3C 04 ...
capture 3: 0B 0B 01 23 [tag] 1C 04 ...
                     ^^       ^^
```

- Packet byte 3 cleared `0x40`, proving this is the packet-level low-battery flag.
- Status byte 6 cleared `0x20`, proving this is the status-level battery-low flag.
- Status bits 2 and 3 remained set (`0x0C`), proving they are **not** additional low-battery indicators.
- The configuration export still says `Status=60` (`0x3C`) while every wire packet says `0x1C`. Therefore `[Information] Status` is a snapshot that may be stale; use the transmitted byte as authoritative live state.
- The first packet pair contains true float zero (`00 00 00 00`), further confirming ordinary IEEE-754 handling.
- Packet timing remains ~10.02–10.03 s, with no inferred losses in this short capture.

`broadweigh_decoded_3.csv` contains the complete third decode.

## Fourth configuration: isolated unit test

Version 4 successfully changed only the unit from the second list entry, metres per second, to the fourth entry, feet per second. The configurable export difference is exactly the expected gain change:

```text
Gain: 1.1176 -> 3.6666
```

(`Status: 60 -> 16` is the information snapshot reacting to the change, not a configured field.) All six frames pass CRC and match all six toolkit rows. On the wire:

```text
m/s (capture 3): status 1C = bits 2+3 + power-up
fps (capture 4): status 10 = power-up only
```

This one-variable test proves status bits 2 and 3 are caused by unit/gain selection. They are not battery, timing, or measurement-activity flags: capture 3 had them set even at zero, while capture 4 clears them at nonzero readings.

They are not a straightforward two-bit unit index because the observed truth table is:

| Toolkit entry | Unit | Gain | Status bits 3:2 |
|---:|---|---:|---:|
| 1 | mph | 2.5 | `00` (capture 5) |
| 2 | m/s | 1.1176 | `11` |
| 3 | km/h | 4.02336 | `00` |
| 4 | fps | 3.6666 | `00` |

Capture 6 later completes entry 5. The precise observed meaning is “WSSx unit/gain-dependent flags; both asserted for m/s.” Because four units share `00`, a receiver cannot determine the engineering unit from these bits alone.

The fps gust `10.96787` is exactly 30 count steps: `3.6666 / 1.002911 / 10 × 30`, again confirming the gain, factory correction, and 10-second gust period.

`broadweigh_decoded_4.csv` contains the complete fourth decode.

## Fifth configuration: miles per hour

Version 5 changed only feet per second to miles per hour:

```text
Gain: 3.6666 -> 2.5
```

All four frames pass CRC and match all four toolkit rows. Wire status remains `0x10`, so mph produces status bits 3:2 = `00`. The completed cases are now:

| Toolkit entry | Unit | Gain | Status bits 3:2 |
|---:|---|---:|---:|
| 1 | mph | 2.5 | `00` |
| 2 | m/s | 1.1176 | `11` |
| 3 | km/h | 4.02336 | `00` |
| 4 | fps | 3.6666 | `00` |
| 5 | knots | ~2.17244 (inferred in capture 6) | `00` |

The mph gust quantisation again fits exactly: `2.5 / 1.002911 / 10 = 0.249274326 mph` per pulse count; observed gusts are 2 and 10 counts.

`broadweigh_decoded_5.csv` contains the complete fifth decode.

## Sixth configuration: knots

Version 6 completes the built-in unit list. Four raw frames pass CRC; the two toolkit rows match the later raw pair exactly, while the first pair predates toolkit logging. Every frame has status `0x10`, so knots produces bits 3:2 = `00`.

No version-6 `.tcf` was supplied, but the gust values identify the expected knots gain independently. `0.8664522` and the preceding `1.299678` are 4 and 6 pulse-count steps, implying:

```text
Gain = (0.8664522 / 4) * 1.002911 * 10 ≈ 2.17244 knots/Hz
```

The complete preset truth table is therefore:

| Toolkit entry | Unit | Gain | Status bits 3:2 |
|---:|---|---:|---:|
| 1 | mph | 2.5 | `00` |
| 2 | m/s | 1.1176 | `11` |
| 3 | km/h | 4.02336 | `00` |
| 4 | fps | 3.6666 | `00` |
| 5 | knots | ~2.17244 | `00` |

Bits 2 and 3 have never separated. Empirically, the pair is an **m/s-preset indicator**, not a general unit number. The T24-PA documentation still calls the individual bits reserved, so their separate internal names remain unavailable.

`broadweigh_decoded_6.csv` contains the complete sixth decode.

## Seventh configuration: m/s regression

Version 7 returns to metres per second and reproduces version 3. Comparing their exports shows every configurable field is identical. The only difference is the read-only information snapshot (`Status=60` in version 3 versus the now-current `Status=28` in version 7).

All 14 raw frames pass CRC with no discarded or trailing bytes:

- 7 average packets on `25DF` and 7 gust packets on `25E0`.
- Every status byte is `0x1C`: bits 2 and 3 asserted plus power-up.
- Packet type is `0x23`: broadcast Data Provider, with battery-low and error clear.
- Gust intervals are 10.015–10.033 seconds.
- Gust values represent exactly 8, 6, 4, 2, 4, 2, and 2 m/s pulse-count steps using `Gain=1.1176`, `FactoryGain=1.002911`, and a 10-second gust period.
- Length, base address, packet type, tags, data type, status, CRC scheme, and scaling behavior all regress to the earlier m/s result.

This closes the repeatability question: the `11` status pair is reproducibly tied to the m/s gain/preset, not a transient radio, battery, timing, or measurement condition.

The version-7 manufacturer CSV arrived under the name `broadweigh_decoded_7.csv`, which collided with the generated-output name and was overwritten during analysis. The decoded raw result is preserved as `broadweigh_protocol_7.csv`; reattach the original CSV if independent row-by-row timestamp correlation is required. Earlier displayed rows already agreed with the raw float values.

## Experiments to resolve and confirm the remaining points

Use a pulse generator or steady-speed fixture where possible and save the `.tcf`, raw serial log, and toolkit CSV together for each run.

1. **Status bits 2–3:** the built-in unit table and m/s regression are complete. To distinguish “m/s preset” from a numeric gain condition, change only gain just above/below `1.1176`, if the toolkit safely permits custom gain. Otherwise the pair's externally observable meaning is complete.
2. **Low battery threshold:** captures 2 and 3 prove packet byte 3 `23 ↔ 63` and status bit 5 `0 ↔ 1`. A controlled supply sweep is needed only if the exact hysteresis matters.
3. **Error flag:** create a documented, non-damaging sensor fault if the manufacturer identifies one. The prediction is byte 3 changes from `23` to `A3`.
4. **Units:** captures 1 and 3–6 cover every built-in unit and validate their gain/status behavior.
5. **Tags:** change the base tag to a conspicuous value such as `3A7F`. Expect offsets 4–5 to become `3A 7F`, and gust to use `3A80`.
6. **Average/gust periods:** record a step in pulse rate with average periods 1 and 10 seconds and gust disabled/1/3/5/10 seconds. This confirms time-domain semantics and should not change packet layout; disabling gust should remove the second tag.
7. **Radio metadata:** keep the input fixed while changing distance or attenuation. Only RSSI/CV, timing/loss, and CRC should vary. Changing base-station DIP address should change offset 2.

Channel, group key, transmit power, and transmit interval are not expected to be encoded as value-packet fields; they affect delivery, RSSI/CV, or timing.

## Parser

The retained Rust decoder in [`src/lib.rs`](../src/lib.rs) reconstructs packets
across arbitrary serial read boundaries, validates CRC, and decodes the value
and radio metadata. Its regression test contains a verified captured frame:

```sh
cargo test
```

## Protocol references

- Supplied `Broadweigh-User-Manual.md`, especially ID/Data Tags, BW-WSS data rate, gust behavior, units, and base-station serial limitations.
- Mantracourt T24 Technical Manual (packet structure, Data Provider layout, CRC, types, RSSI/CV): https://manualzilla.com/doc/5726126/t24-technical-manual
- Official T24 Telemetry User Manual download (module-specific status bits): https://us.mantracourt.com/wpfd_file/t24-telemetry-user-manual-8/
- Official T24-WSS manual: https://www.mantracourt.com/userfiles/documents/t24-wss_manual.pdf

Earlier fixed-header/framing notes are superseded by this report.
