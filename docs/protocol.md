# Scanner-to-gateway wire protocol — v0.3 (prototype)

> **Status: prototype, v0.3.** This format will evolve as features are added
> (encryption, multi-payload types, BLE-side metadata). It's documented here
> so the gateway has a stable target, and so future versions can be diffed
> against it.
>
> **Changes since v0.2:** `DeviceId` renamed to `HardwareId` (still a
> FICR-sourced `u64` newtype); `humidity: u8` (whole %) replaced by
> `rh_cpercent: u16` (centi-percent, matching the SHT45's resolution);
> `pressure_pa` removed — barometer-less nodes no longer pad a dead field,
> and the future BMP581 node will reintroduce pressure via a typed payload
> (see [Known limitations](#known-limitations-will-change-in-future-versions)).
> Frame shrank from 30 to **27 bytes**.
>
> **Changes since v0:** payload type changed from `SensorPacket` to
> `SensorObservation` (adds receiver-observed metadata — RSSI and age);
> device id widened from `u8` to a `u64` newtype sourced from the sensor's
> FICR.

The receiver firmware ([firmware/receiver](../firmware/receiver)) emits framed
packets over USB CDC. The gateway reads `/dev/ttyACM0` (or a udev symlink such
as `/dev/homescope-receiver` — the deployed setup, see
[deploy/udev](../deploy/udev/99-homescope-receiver.rules)) and parses frames.

## Frame layout (27 bytes)

```
+--------+--------+-----------------------------+----------+----------+
| MAGIC0 | MAGIC1 | payload (23 bytes)          |  CRC lo  |  CRC hi  |
| 0x48   | 0x53   | SensorObservation bytes     |       u16 LE        |
+--------+--------+-----------------------------+----------+----------+
   [0]      [1]    [2..25]                          [25]      [26]
```

- **Magic (bytes 0-1):** ASCII `HS` = `0x48 0x53`. Frame-boundary marker — lets the gateway resync after errors or after opening the stream mid-frame.
- **Payload (bytes 2-24):** raw bytes of `homescope_common::observation::SensorObservation`. Layout below.
- **CRC (bytes 25-26):** CRC-16/IBM-SDLC over the **payload only** (not over magic). Little-endian on the wire.

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

## SensorObservation layout

Defined in [common/src/observation.rs](../common/src/observation.rs) as `homescope_common::observation::SensorObservation`:

```rust
#[repr(C, packed)]
struct SensorObservation {
    hardware_id: HardwareId,  // [0..8]   — u64 newtype, repr(transparent)
    seq:         u32,         // [8..12]
    temp_cdegc:  i16,         // [12..14]
    rh_cpercent: u16,         // [14..16]
    battery_mv:  u16,         // [16..18]
    rssi:        i8,          // [18]
    age_ms:      u32,         // [19..23]
}
```

23 bytes total, no padding (`#[repr(C, packed)]`). Multi-byte fields are in target-native byte order. nRF52840 and typical gateway hosts are both little-endian, so this happens to be little-endian on the wire — but the gateway should not assume native endianness; use `SensorObservation::from_bytes` (which internally uses `bytemuck::pod_read_unaligned`) or `Frame::try_from_bytes` to do the decode.

`HardwareId` is a `#[repr(transparent)]` newtype around `u64`, so byte-for-byte identical to a `u64` on the wire. See [common/src/hardware_id.rs](../common/src/hardware_id.rs).

### Field semantics

| Field         | Type         | Source        | Meaning                                                                             |
| ------------- | ------------ | ------------- | ----------------------------------------------------------------------------------- |
| `hardware_id` | `HardwareId` | sensor's FICR | 64-bit factory-set chip identifier. Rendered as 16 uppercase hex chars (`XXXXXXXXXXXXXXXX`) on MQTT/HTTP. |
| `seq`         | `u32`        | sensor        | Per-sensor monotonic counter. Resets on reboot.                                     |
| `temp_cdegc`  | `i16`        | sensor        | Temperature in centi-degrees C (2143 = 21.43 °C).                                   |
| `rh_cpercent` | `u16`        | sensor        | Relative humidity in centi-percent (4521 = 45.21 %RH).                              |
| `battery_mv`  | `u16`        | sensor        | Battery voltage in millivolts.                                                      |
| `rssi`        | `i8`         | **receiver**  | Signal strength at the receiver, in dBm (typ. -30 to -110).                         |
| `age_ms`      | `u32`        | **receiver**  | Milliseconds between BLE capture and USB-CDC send. See "Age and timestamps."        |

Fields marked "receiver" are observed/computed by the receiver dongle and are not part of the over-the-air BLE payload — they live only in the gateway-bound `SensorObservation`. The sensor-side type (`SensorPacket`, defined in [common/src/packet.rs](../common/src/packet.rs)) is a strict subset (`hardware_id` through `battery_mv`, 18 bytes).

> **Note — pressure is not in this payload.** Exactly one designated **indoor**
> node will carry a BMP581 (pressure is house-wide; see
> [architecture.md](architecture.md#sensors)). Rather than pad every frame with
> a dead field, pressure will arrive with the planned type discriminator (see
> [Known limitations](#known-limitations-will-change-in-future-versions)) as a
> distinct payload variant.

## Age and timestamps

Sensors are battery-powered deep-sleep nodes with no wall clock. The receiver
also has no wall clock — only a monotonic uptime counter (`embassy_time::Instant`).
The gateway has the wall clock.

Rather than try to synchronize clocks across these three actors, the receiver
stamps each observation with `age_ms` at **send time**:

```text
age_ms = Instant::now() - Instant::at_capture
```

The capture timestamp is recorded the moment a matching BLE advertisement
arrives at the receiver and is stored alongside the observation in the
backlog channel. When the observation is eventually written to USB-CDC, the
delta becomes its `age_ms`.

The gateway then computes the wall-clock arrival time as:

```text
received_at = Utc::now() - age_ms
```

This works correctly for both live packets (`age_ms ≈ 5-20`) and packets
drained from the receiver's backlog after a gateway restart (`age_ms` can be
minutes or hours). No clock-sync handshake is required — each observation is
self-describing.

`age_ms` is a `u32`, so the maximum representable age is ~49.7 days. If the
delta ever exceeds that (it shouldn't — the receiver is USB-powered and any
unplug resets its uptime), the receiver saturates to `u32::MAX` rather than
wrapping. Gateway code should treat unusually large ages as suspect but not
incorrect.

## Reference parser algorithm

For implementers writing a parser in another language. The Rust gateway in this repo uses a buffered approach (described below); the byte-at-a-time state machine is here as the canonical spec.

Byte-at-a-time state machine:

- **Hunting:** read 1 byte. If it equals `0x48` → `SawMagic0`. Else stay.
- **SawMagic0:** read 1 byte. If `0x53` → `InFrame`. If `0x48` → stay in `SawMagic0` (preserves candidate). Else → `Hunting`.
- **InFrame:** read 25 bytes (payload + CRC) with a short timeout (~100 ms is plenty — actual transmission is sub-millisecond; the timeout only protects against partial-frame stalls). Verify CRC over the 23-byte payload against bytes [23..25] interpreted as little-endian u16. On success, emit the decoded observation. → `Hunting` either way.

Any I/O error, timeout, or CRC mismatch returns to `Hunting`. Don't try to "salvage" 27 bytes of a failed frame — at this packet rate, simply restarting the hunt costs at most one frame and avoids a fully byte-by-byte sliding-window matcher.

## Reference parser — Rust gateway implementation

[gateway/src/decoder.rs](../gateway/src/decoder.rs) implements this as a `tokio_util::codec::Decoder<Item = SensorObservation>` over `BytesMut`, which handles partial-frame buffering across `AsyncRead` boundaries for free:

1. `memchr` the first magic byte (`0x48`) in the buffer. If absent → `Ok(None)` (ask for more bytes).
2. `advance` past everything before the magic byte. If the buffer is now shorter than `FRAME_SIZE` (27) → `Ok(None)`.
3. Slice a `&[u8; 27]` from the front of the buffer, pass to `Frame::try_from_bytes` (which checks the second magic byte, runs the CRC, and returns `Result<Frame, FrameError>`).
4. On `Ok(frame)`: `advance(27)`, return `Ok(Some(frame.payload))`. On `Err`: `advance(1)` (skip past the false magic) and loop to search for the next candidate.

CRC mismatches and bad-magic false-syncs are absorbed silently by the loop — they're expected with magic-byte framing. Real I/O errors propagate as `Err` and the gateway exits (its supervisor restarts it).

## Known limitations (will change in future versions)

- **Single payload type.** No version or type field after magic. A future revision will likely insert a 1-byte type discriminator at byte 2, shifting the payload and CRC. The magic will stay the same. This is also how pressure (BMP581 node) and other sensor variants will be carried.
- **No encryption.** Payload is plaintext. Acceptable on USB CDC between two trusted devices. When BLE-side AEAD lands, the receiver and gateway will forward the ciphertext *opaquely* — decryption happens in the API, so the USB-CDC frame (and the MQTT envelope) will carry plaintext routing fields (`hardware_id`, `seq`, `rssi`, `age_ms`) plus an encrypted sensor-fields blob. Gateways stay keyless by design.
- **No BLE-side metadata.** PHY, channel index, and the per-advertisement BLE MAC address are not forwarded. If you ever want to bind `hardware_id` ↔ MAC for verification, those fields would need to be added to `SensorObservation`.
- **Sensor reboots reset `seq`.** Replay protection (when encryption is added) must account for this; one option is a per-boot random session prefix in the future nonce.
