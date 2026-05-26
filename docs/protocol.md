# Scanner-to-gateway wire protocol — v0 (prototype)

> **Status: prototype, v0.** This format will evolve as features are added
> (reception metadata, multi-payload types, encryption). It's documented here
> so the gateway has a stable target, and so future versions can be diffed
> against it.

The receiver firmware ([firmware/receiver](../firmware/receiver))
emits framed packets over USB CDC. The gateway reads `/dev/ttyACM0` (or a
udev symlink such as `/dev/homescope-receiver`) and parses frames.

## Frame layout (18 bytes)

```
+--------+--------+--------------------+----------+----------+
| MAGIC0 | MAGIC1 | payload (14 bytes) |  CRC lo  |  CRC hi  |
| 0x48   | 0x53   | SensorPacket bytes |       u16 LE        |
+--------+--------+--------------------+----------+----------+
   [0]      [1]    [2..16]                [16]      [17]
```

- **Magic (bytes 0-1):** ASCII `HS` = `0x48 0x53`. Frame-boundary marker — lets the gateway resync after errors or after opening the stream mid-frame.
- **Payload (bytes 2-15):** raw bytes of `homescope_common::SensorPacket`. Layout below.
- **CRC (bytes 16-17):** CRC-16/IBM-SDLC over the **payload only** (not over magic). Little-endian on the wire.

## CRC algorithm

CRC-16/IBM-SDLC (a.k.a. CRC-16/X-25, CRC-16/HDLC):

```
Polynomial: 0x1021
Init:       0xFFFF
RefIn:      true
RefOut:     true
XorOut:     0xFFFF
```

Both firmware and gateway use the `crc` crate with `crc::CRC_16_IBM_SDLC`. Identical bit-for-bit on both sides.

## SensorPacket layout

Defined in [common/src/packet.rs](../common/src/packet.rs) as `homescope_common::packet::SensorPacket`:

```rust
#[repr(C, packed)]
struct SensorPacket {
    device_id:   u8,    // [0]
    seq:         u32,   // [1..5]
    temp_cdegc:  i16,   // [5..7]
    humidity:    u8,    // [7]
    pressure_pa: u32,   // [8..12]
    battery_mv:  u16,   // [12..14]
}
```

14 bytes total, no padding (`#[repr(C, packed)]`). Multi-byte fields are in target-native byte order. nRF52840 and typical gateway hosts are both little-endian, so this happens to be little-endian on the wire — but the gateway should not assume native endianness; use `SensorPacket::from_bytes` (which internally uses `bytemuck::pod_read_unaligned`) or `SensorPacket::parse_frame` to do the decode.

### Field semantics

| Field         | Type   | Meaning                                              |
| ------------- | ------ | ---------------------------------------------------- |
| `device_id`   | `u8`   | Per-sensor identifier. Maps to friendly name in DB.  |
| `seq`         | `u32`  | Per-sensor monotonic counter. Resets on reboot.      |
| `temp_cdegc`  | `i16`  | Temperature in centi-degrees C (2143 = 21.43 °C).    |
| `humidity`    | `u8`   | Relative humidity %, 0–100.                          |
| `pressure_pa` | `u32`  | Barometric pressure in Pascals.                      |
| `battery_mv`  | `u16`  | Battery voltage in millivolts.                       |

## Reference parser algorithm

For implementers writing a parser in another language. The Rust gateway in this repo uses a buffered approach (described below); the byte-at-a-time state machine is here as the canonical spec.

Byte-at-a-time state machine:

- **Hunting:** read 1 byte. If it equals `0x48` → `SawMagic0`. Else stay.
- **SawMagic0:** read 1 byte. If `0x53` → `InFrame`. If `0x48` → stay in `SawMagic0` (preserves candidate). Else → `Hunting`.
- **InFrame:** read 16 bytes (payload + CRC) with a short timeout (~100 ms is plenty — actual transmission is sub-millisecond; the timeout only protects against partial-frame stalls). Verify CRC over the 14-byte payload against bytes [14..16] interpreted as little-endian u16. On success, emit the decoded packet. → `Hunting` either way.

Any I/O error, timeout, or CRC mismatch returns to `Hunting`. Don't try to "salvage" 18 bytes of a failed frame — at this packet rate, simply restarting the hunt costs at most one frame and avoids a fully byte-by-byte sliding-window matcher.

## Reference parser — Rust gateway implementation

[gateway/src/main.rs](../gateway/src/main.rs) implements this as a `tokio_util::codec::Decoder<Item = SensorPacket>` over `BytesMut`, which handles partial-frame buffering across `AsyncRead` boundaries for free:

1. `memchr` the first magic byte (`0x48`) in the buffer. If absent → `Ok(None)` (ask for more bytes).
2. `advance` past everything before the magic byte. If the buffer is now shorter than `SENSOR_PACKET_FRAME_LEN` (18) → `Ok(None)`.
3. Slice a `&[u8; 18]` from the front of the buffer, pass to `SensorPacket::parse_frame` (which checks the second magic byte, runs the CRC, and returns `Result<SensorPacket, FrameError>`).
4. On `Ok(packet)`: `advance(18)`, return `Ok(Some(packet))`. On `Err`: `advance(1)` (skip past the false magic) and loop to search for the next candidate.

CRC mismatches and bad-magic false-syncs are absorbed silently by the loop — they're expected with magic-byte framing. Real I/O errors propagate as `Err` and the gateway reopens the port.

## Known limitations (will change in future versions)

- **Single payload type.** No version or type field after magic. A future revision will likely insert a 1-byte type discriminator at byte 2, shifting the payload and CRC. The magic will stay the same.
- **No encryption.** Payload is plaintext. Acceptable on USB CDC between two trusted devices; will matter once BLE-side encryption is added (the receiver will then decrypt before forwarding, and the gateway side stays plaintext over CDC).
- **No reception metadata.** The receiver does not forward RSSI, receiver-side PHY, or a timestamp. The gateway can stamp its own arrival time, but RSSI for range diagnostics is currently lost.
- **Sensor identity is `device_id` only.** The per-advertisement BLE MAC address is not exposed to the gateway. If you ever want to bind device_id ↔ MAC for verification, that field will need to be added.
