# Broadweigh BW-WSS serial value protocol

This is the implementation specification for decoding Broadweigh BW-WSS value messages received through a T24 base station. The supplied Mantracourt [T24 Technical Manual](reference/t24-technical-manual.pdf) is the authoritative source for T24 transport framing, packet types, Data Provider layout, data types, radio metadata, and CRC. [`reverse-engineering.md`](reverse-engineering.md) preserves BW-WSS-specific capture evidence and experiments.

## Required parser behavior

Treat the serial input as an arbitrary byte stream. A read may contain part of a packet, one packet, or several packets. hTerm line boundaries and timestamps are capture metadata, not protocol bytes.

Do **not** search for a fixed `0B 0B 01` header. The two `0B` bytes in these captures are duplicated packet lengths; another packet type or data size may use another value.

For the captured BW-WSS float messages:

1. Buffer incoming bytes.
2. Find two consecutive `0B` bytes.
3. Wait until 16 bytes are available from that position.
4. Verify the Modbus CRC-16 over the first 14 bytes.
5. Require `(packet_type & 0x1F) == 0x03` and `(data_type & 0x07) == 0x04`.
6. If validation succeeds, decode the frame and remove all 16 bytes.
7. If validation fails, discard one byte and search again.

For a general T24 parser, let the first byte be `N`, require the second byte to equal `N`, and read `N + 5` total bytes. The CRC is always the final two bytes and covers everything before them. The BW-WSS value packet has `N = 11`, hence 16 bytes total.

## 16-byte BW-WSS value frame

```text
Offset  Size  Field
0       1     Length N = 0x0B
1       1     Duplicate length = 0x0B
2       1     Base-station address
3       1     Packet type and flags
4       2     Data Tag, big-endian
6       1     Status bits
7       1     Data type, lower three bits = 0x04
8       4     Value, IEEE-754 binary32, big-endian
12      1     RSSI byte
13      1     CV byte
14      2     CRC-16/Modbus, little-endian
```

The length counts the 11 bytes from offset 3 through offset 13. Total size is therefore `N + 5`.

Example m/s average packet:

```text
0B 0B 01 23 25 DF 1C 04 3F 0F 4E 9F FB E8 5E A6
```

This decodes as:

```text
length        11
base address  1
packet type   Data Provider, broadcast
tag           25DF
status        power-up + undocumented bits 2 and 3
data type     float32
value         0.5597934 m/s
RSSI          -50 dBm
CV            104
CRC           0xA65E, valid
```

## Packet type and flags: offset 3

```text
kind       = byte[3] & 0x1F
broadcast  = bool(byte[3] & 0x20)
low_battery= bool(byte[3] & 0x40)
error      = bool(byte[3] & 0x80)
```

`kind == 0x03` means Data Provider. Normal BW-WSS value packets observed here use:

| Byte | Meaning |
|---:|---|
| `23` | Data Provider + broadcast |
| `63` | Data Provider + broadcast + low battery |
| `A3` | Predicted Data Provider + broadcast + error; not induced during testing |

## Data Tag: offsets 4–5

Decode as an unsigned 16-bit big-endian number.

- The configured base tag identifies average wind speed.
- Base tag + 1 identifies optional gust wind speed.
- Example: base `25DF`, gust `25E0`. Addition is hexadecimal and may carry between digits.

Do not hardcode a tag. It is user-configurable and is the only reliable way to associate a packet with its configured measurement.

## Status: offset 6

The T24 Technical Manual assigns global meanings only to bits 0 and 1. All
other bits are device-specific. The applicable T24-PA documentation defines
bits 4–7 as below but marks bits 2 and 3 reserved.

| Bit | Mask | Meaning |
|---:|---:|---|
| 0 | `01` | Shunt calibration active |
| 1 | `02` | Input integrity error |
| 2 | `04` | Reserved (T24-PA documentation) |
| 3 | `08` | Reserved (T24-PA documentation) |
| 4 | `10` | Power-up: power was applied/interrupted rather than the device merely being radio-woken |
| 5 | `20` | Battery low |
| 6 | `40` | Digital input active |
| 7 | `80` | Digital output active |

Observed values:

| Status | Meaning |
|---:|---|
| `10` | Power-up; used by mph, km/h, fps, and knots captures |
| `1C` | Power-up + undocumented bits 2 and 3 |
| `3C` | Same as `1C` + battery low |

Bits 2 and 3 have no published individual names and never separated in testing. Empirically, across all five built-in units and a repeated m/s regression, both were set for the m/s preset and clear for every other unit. This is capture evidence, not an official protocol definition. Treat both bits as reserved and do not use them to determine the engineering unit.

The `.tcf` `[Information] Status` value may be stale. Use the status byte in each received packet as the live state.

## Data type and value: offsets 7–11

The lower three bits select the data type; upper five bits carry function and
display metadata.
The captured byte is `0x04`: T24 type 4, IEEE-754 binary32, with no display
metadata set. Decode offsets 8–11 in big-endian byte order:

```python
value = struct.unpack(">f", frame[8:12])[0]
```

The value is already scaled into the sensor's configured engineering unit. The packet does not officially identify that unit. Supply unit metadata from configuration.

Observed built-in gains:

| Unit | Gain |
|---|---:|
| mph | 2.5 |
| m/s | 1.1176 |
| km/h | 4.02336 |
| fps | 3.6666 |
| knots | approximately 2.172436 |

For this device, the gust quantisation step is:

```text
step = Gain / FactoryGain / gust_period_seconds
```

With `FactoryGain=1.002911` and a 10-second gust period, every recorded gust value was an integer multiple of this step.

## RSSI, CV, and LQI: offsets 12–13

Packets received from remote devices end with RSSI (Received Signal Strength
Indication) and CV (Correlation Value) bytes before the CRC. RSSI approximates
received signal strength in dB and uses signed two's-complement with an offset
of 45:

```python
rssi_dbm = int.from_bytes(frame[12:13], signed=True) - 45
```

CV uses only the lower seven bits:

```python
cv = frame[13] & 0x7F
```

About 55 is poor and 110 is good. The documented raw LQI (Link Quality
Indication) is:

```python
lqi_raw = ((94 + rssi_dbm + cv - 55) / 2) * 3.9
```

This averages the RSSI margin above `-94 dBm` and the CV margin above 55, then
scales the result to the manual's approximate operational range of 0–255. The
manual defines raw values 50–128 as the usable range corresponding to 0–100%.
This specification uses linear interpolation and clamps values outside that
range:

```python
lqi_percent = min(100, max(0, (lqi_raw - 50) * 100 / (128 - 50)))
```

| Raw LQI | Display value |
|---:|---:|
| `<= 50` | `0%` |
| `89` | `50%` |
| `>= 128` | `100%` |

Mantracourt does not define names for portions of the percentage scale. A UI
that needs short labels can use this application convention:

| Percentage | Label |
|---:|---|
| `100%` | Perfect |
| `75%` to `< 100%` | Good |
| `50%` to `< 75%` | OK |
| `25%` to `< 50%` | Weak |
| `< 25%` | Poor |

“Perfect” means the top of the normalized scale, not guaranteed packet
delivery. Values above raw 128 all display as 100%, so retain raw LQI when
differences between strong links matter. Communication may remain possible
throughout the 0–100% range but can become intermittent near zero.

These calculations follow the supplied Mantracourt [T24 Technical Manual](reference/t24-technical-manual.pdf); current display guidance comes from the [T24 Telemetry User Manual](https://www.mantracourt.com/wp-content/uploads/T24-Telemetry-User-Manual.pdf).

RSSI and CV describe the radio reception at the base station. They are not sensor values and legitimately change between otherwise identical packets.

## CRC: offsets 14–15

Use CRC-16/Modbus with initial value `0xFFFF` and reflected polynomial `0xA001`. Calculate over offsets 0–13 and compare against offsets 14–15 interpreted little-endian.

```python
def crc16_modbus(data: bytes) -> int:
    crc = 0xFFFF
    for byte in data:
        crc ^= byte
        for _ in range(8):
            crc = (crc >> 1) ^ (0xA001 if crc & 1 else 0)
    return crc

valid = crc16_modbus(frame[:-2]) == int.from_bytes(frame[-2:], "little")
```

Never accept or use a value from a frame with an invalid CRC.

## Configuration that is not carried in the packet

The consumer must know or configure these externally:

- Engineering unit. No officially documented packet field identifies it.
- Which configured Data Tag belongs to which sensor.
- Transmit interval.
- Average sample period.
- Gust sample period and whether gust transmission is enabled.
- Calibration gain/offset if raw engineering interpretation beyond the already-scaled float is required.

For BW-WSS:

- The base tag carries the moving average.
- The base tag + 1 carries the optional gust.
- Average is calculated over the configured moving window. If its configured period is shorter than the transmit interval, the manual states that it is effectively averaged since the preceding transmission.
- Gust is the maximum rolling-window average observed since the preceding transmission.

Packet loss is possible. Do not assume strict alternation or invent a value when one tag is absent; track each tag and timestamp independently.

## Minimal decoder

```python
import struct

def decode_bw_wss(frame: bytes, unit: str) -> dict:
    if len(frame) != 16 or frame[0] != 11 or frame[1] != 11:
        raise ValueError("not a 16-byte BW-WSS value frame")
    if crc16_modbus(frame[:-2]) != int.from_bytes(frame[-2:], "little"):
        raise ValueError("bad CRC")
    if frame[3] & 0x1F != 3 or frame[7] & 0x07 != 4:
        raise ValueError("not a float Data Provider frame")

    return {
        "base_address": frame[2],
        "broadcast": bool(frame[3] & 0x20),
        "low_battery": bool(frame[3] & 0x40),
        "error": bool(frame[3] & 0x80),
        "tag": int.from_bytes(frame[4:6], "big"),
        "status": frame[6],
        "value": struct.unpack(">f", frame[8:12])[0],
        "unit": unit,
        "rssi_dbm": int.from_bytes(frame[12:13], signed=True) - 45,
        "cv": frame[13] & 0x7F,
    }
```

The working stream implementation is [`src/lib.rs`](../src/lib.rs). Run its
captured-frame regression check with `cargo test`.

## Confidence and remaining limitation

Every structural field, endianness rule, flag needed by a consumer, value conversion, radio field, and checksum rule was validated across the supplied captures. There are no unknown bits blocking an implementation.

The only remaining vendor-internal detail is the separate name or purpose of status bits 2 and 3. Capture evidence fully characterizes their behavior for all built-in units: they always move together and are asserted only for m/s. Official documentation still marks them reserved, so consumers must not assign them protocol meaning.
