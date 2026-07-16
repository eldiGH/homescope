# Scanner-to-gateway wire protocol — v0.4 (prototype)

> **Status: prototype, v0.4** (in flight 2026-07 — lands with the DeviceAddr
> refactor). This format will evolve as features are added (TLV measurement
> encoding, encryption). It's documented here so the gateway has a stable
> target, and so future versions can be diffed against it.
>
> **Changes since v0.3:** device identity moved out of the payload — the
> sensor no longer transmits its FICR `DEVICEID` in the air packet; identity
> is now the BLE **advertising address** (AdvA, derived from FICR
> `DEVICEADDR`), which the receiver observes on every report and forwards as
> `device_addr: DeviceAddr([u8; 6])` (LSB-first, matching BLE on-air order).
> `HardwareId(u64)` is gone. Frame shrank from 27 to **25 bytes**.
>
> **Changes since v0.2:** `DeviceId` renamed to `HardwareId`; `humidity: u8`
> replaced by `rh_cpercent: u16`; `pressure_pa` removed (pressure arrives
> later via the TLV encoding — see [Planned: v0.5](#planned-v05--tlv-payload-then-aead)).
> Frame shrank from 30 to 27 bytes.
>
> **Changes since v0:** payload type changed from `SensorPacket` to
> `SensorObservation` (adds receiver-observed metadata — RSSI and age).

The receiver firmware ([firmware/receiver](../firmware/receiver)) emits framed
packets over USB CDC. The gateway reads `/dev/ttyACM0` (or a udev symlink such
as `/dev/homescope-receiver` — the deployed setup, see
[deploy/udev](../deploy/udev/99-homescope-receiver.rules)) and parses frames.

## Frame layout (25 bytes)

```
+--------+--------+-----------------------------+----------+----------+
| MAGIC0 | MAGIC1 | payload (21 bytes)          |  CRC lo  |  CRC hi  |
| 0x48   | 0x53   | SensorObservation bytes     |       u16 LE        |
+--------+--------+-----------------------------+----------+----------+
   [0]      [1]    [2..23]                          [23]      [24]
```

- **Magic (bytes 0-1):** ASCII `HS` = `0x48 0x53`. Frame-boundary marker — lets the gateway resync after errors or after opening the stream mid-frame.
- **Payload (bytes 2-22):** raw bytes of `homescope_common::observation::SensorObservation`. Layout below.
- **CRC (bytes 23-24):** CRC-16/IBM-SDLC over the **payload only** (not over magic). Little-endian on the wire.

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
    device_addr: DeviceAddr,  // [0..6]   — [u8; 6], LSB-first (BLE on-air order)
    seq:         u32,         // [6..10]
    temp_cdegc:  i16,         // [10..12]
    rh_cpercent: u16,         // [12..14]
    battery_mv:  u16,         // [14..16]
    rssi:        i8,          // [16]
    age_ms:      u32,         // [17..21]
}
```

21 bytes total, no padding (`#[repr(C, packed)]`). Multi-byte fields are in target-native byte order. nRF52840 and typical gateway hosts are both little-endian, so this happens to be little-endian on the wire — but the gateway should not assume native endianness; use `SensorObservation::from_bytes` (which internally uses `bytemuck::pod_read_unaligned`) or `Frame::try_from_bytes` to do the decode.

`DeviceAddr` is a `#[repr(transparent)]` newtype around `[u8; 6]`, stored LSB-first exactly as the address appears on air. It renders as 12 uppercase hex chars, most-significant byte first (standard BLE address order, no colons). See [common/src/device_addr.rs](../common/src/device_addr.rs).

### Field semantics

| Field         | Type         | Source        | Meaning                                                                             |
| ------------- | ------------ | ------------- | ----------------------------------------------------------------------------------- |
| `device_addr` | `DeviceAddr` | **receiver** (from AdvA) | The sensor's BLE advertising address — a random static address derived from its FICR `DEVICEADDR`. Observed by the receiver on the advertising report; **not part of the air payload**. Rendered as 12 uppercase hex chars on MQTT/HTTP. |
| `seq`         | `u32`        | sensor        | Per-sensor monotonic counter. Currently resets on reboot; persistence is planned (see below). |
| `temp_cdegc`  | `i16`        | sensor        | Temperature in centi-degrees C (2143 = 21.43 °C).                                   |
| `rh_cpercent` | `u16`        | sensor        | Relative humidity in centi-percent (4521 = 45.21 %RH).                              |
| `battery_mv`  | `u16`        | sensor        | Battery voltage in millivolts.                                                      |
| `rssi`        | `i8`         | **receiver**  | Signal strength at the receiver, in dBm (typ. -30 to -110).                         |
| `age_ms`      | `u32`        | **receiver**  | Milliseconds between BLE capture and USB-CDC send. See "Age and timestamps."        |

Fields marked "receiver" are observed/computed by the receiver dongle and are not part of the over-the-air BLE payload. The sensor-side type (`SensorPacket`, defined in [common/src/packet.rs](../common/src/packet.rs)) is the strict subset `seq` through `battery_mv` — 10 bytes.

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

This is also why the gateway must stay the component that stamps
`received_at`: computed any later (e.g., at MQTT consumption), broker transit
and offline-queue delays would corrupt the timestamp.

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
- **InFrame:** read 23 bytes (payload + CRC) with a short timeout (~100 ms is plenty — actual transmission is sub-millisecond; the timeout only protects against partial-frame stalls). Verify CRC over the 21-byte payload against bytes [21..23] interpreted as little-endian u16. On success, emit the decoded observation. → `Hunting` either way.

Any I/O error, timeout, or CRC mismatch returns to `Hunting`. Don't try to "salvage" 25 bytes of a failed frame — at this packet rate, simply restarting the hunt costs at most one frame and avoids a fully byte-by-byte sliding-window matcher.

## Reference parser — Rust gateway implementation

[gateway/src/decoder.rs](../gateway/src/decoder.rs) implements this as a `tokio_util::codec::Decoder<Item = SensorObservation>` over `BytesMut`, which handles partial-frame buffering across `AsyncRead` boundaries for free:

1. `memchr` the first magic byte (`0x48`) in the buffer. If absent → `Ok(None)` (ask for more bytes).
2. `advance` past everything before the magic byte. If the buffer is now shorter than `FRAME_SIZE` (25) → `Ok(None)`.
3. Slice a `&[u8; 25]` from the front of the buffer, pass to `Frame::try_from_bytes` (which checks the second magic byte, runs the CRC, and returns `Result<Frame, FrameError>`).
4. On `Ok(frame)`: `advance(25)`, return `Ok(Some(frame.payload))`. On `Err`: `advance(1)` (skip past the false magic) and loop to search for the next candidate.

CRC mismatches and bad-magic false-syncs are absorbed silently by the loop — they're expected with magic-byte framing. Real I/O errors propagate as `Err` and the gateway exits (its supervisor restarts it).

## Planned: v0.5 — TLV payload, then AEAD

Settled 2026-07-16 (see `NOTES-packet-tlv-aead.md` at the repo root for the
worked-out plan). The fixed `SensorPacket` layout is replaced by a
**per-measurement TLV encoding**, which makes the frame **variable-length**:

- Air packet becomes `[seq: u32][id][data][id][data]…` — `seq` stays a fixed
  header (dedup / replay / future AEAD nonce); each measurement is a 1-byte
  **measurement ID** followed by its value. The ID comes from a registry in
  `common` that binds semantics + wire representation + scale + unit (e.g.
  *temperature = i16, ×0.01 °C*), so the ID **implies the length** — there is
  no per-field length byte. Unknown ID or truncated data ⇒ the whole packet
  is dropped with a warning.
- The frame gains a **payload-length byte** after the magic; the CRC stays
  CRC-16/IBM-SDLC over the (now variable) payload.
- The receiver and gateway treat the packet bytes as **opaque** — only sensor
  firmware encodes and only the API decodes. Adding a measurement type never
  touches the receiver, gateway, frame format, or MQTT topics.
- When AEAD lands (ChaCha20-Poly1305, per-device keys, decrypt in the API),
  the TLV section becomes the ciphertext and the cleartext header
  (`device_addr`, `seq`) is bound as associated data. The USB-CDC frame and
  MQTT envelope shapes don't change — the opaque blob just becomes
  ciphertext + 16-byte tag.
- Prerequisite: **`seq` persistence on the sensor** (RAM retention across
  System OFF + periodic flash checkpoint with jump-ahead on boot), so the
  counter never repeats across reboots — required for nonce uniqueness and
  for the API's monotonic replay/dedup check.

## Known limitations (will change in future versions)

- **Single, fixed payload type.** No version/type/length field after magic —
  superseded by the v0.5 plan above.
- **No encryption.** Payload is plaintext. Acceptable on USB CDC between two
  trusted devices; the BLE air link is the segment that needs AEAD (v0.5
  plan). Gateways stay keyless by design — decryption happens in the API.
- **Limited BLE-side metadata.** The advertising address is forwarded (that's
  `device_addr`), but PHY, channel index, and per-event details are not.
- **Sensor reboots reset `seq`.** Fixed by the planned seq persistence (v0.5
  prerequisite); until then the API's replay/dedup check must tolerate
  resets.
