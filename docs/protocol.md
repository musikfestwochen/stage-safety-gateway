# Broadweigh BW-WSS serial value protocol

This is the implementation specification for decoding Broadweigh BW-WSS value messages received through a T24 base station. [`reverse-engineering.md`](reverse-engineering.md) contains the capture evidence and experiments behind it.

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
7       1     Data type = 0x04
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
status        power-up + WSSx m/s flags
data type     float32
value         0.5597934 m/s
RSSI          -50 dB
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

| Bit | Mask | Meaning |
|---:|---:|---|
| 0 | `01` | Shunt calibration active |
| 1 | `02` | Input integrity error |
| 2 | `04` | T24-PA documentation: reserved. BW-WSSx: asserted for the m/s preset; always matched bit 3. |
| 3 | `08` | T24-PA documentation: reserved. BW-WSSx: asserted for the m/s preset; always matched bit 2. |
| 4 | `10` | Power-up: power was applied/interrupted rather than the device merely being radio-woken |
| 5 | `20` | Battery low |
| 6 | `40` | Digital input active |
| 7 | `80` | Digital output active |

Observed values:

| Status | Meaning |
|---:|---|
| `10` | Power-up; used by mph, km/h, fps, and knots captures |
| `1C` | Power-up + both BW-WSSx m/s flags |
| `3C` | Same as `1C` + battery low |

Bits 2 and 3 have no published individual names and never separated in testing. Across all five built-in units and a repeated m/s regression, their complete externally observable behavior is: both are set for m/s and clear for every other unit. They do not uniquely encode the unit.

The `.tcf` `[Information] Status` value may be stale. Use the status byte in each received packet as the live state.

## Data type and value: offsets 7–11

The captured data-type byte is `0x04`: T24 type 4, IEEE-754 binary32. Decode offsets 8–11 in big-endian byte order:

```python
value = struct.unpack(">f", frame[8:12])[0]
```

The value is already scaled into the sensor's configured engineering unit. The packet does not uniquely identify that unit: status distinguishes m/s from “not m/s,” but cannot distinguish mph, km/h, fps, and knots. Supply unit metadata from configuration.

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

RSSI uses signed two's-complement with an offset of 45:

```python
rssi_db = int.from_bytes(frame[12:13], signed=True) - 45
```

CV uses only the lower seven bits:

```python
cv = frame[13] & 0x7F
```

About 55 is poor and 110 is excellent. The documented operational LQI is:

```python
lqi = ((94 + rssi_db + cv - 55) / 2) * 3.9
```

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

- Engineering unit. The status byte only identifies m/s versus the four other built-in choices.
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
        "rssi_db": int.from_bytes(frame[12:13], signed=True) - 45,
        "cv": frame[13] & 0x7F,
    }
```

The working stream implementation is [`src/lib.rs`](../src/lib.rs). Run its
captured-frame regression check with `cargo test`.

## Confidence and remaining limitation

Every structural field, endianness rule, flag needed by a consumer, value conversion, radio field, and checksum rule was validated across the supplied captures. There are no unknown bits blocking an implementation.

The only remaining vendor-internal detail is the separate name or purpose of status bits 2 and 3. Their external behavior is fully characterized for all built-in units: they always move together and are asserted only for m/s.
