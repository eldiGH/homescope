# Scanner-to-gateway wire protocol — v0.5 (prototype)

> **Status: prototype, v0.5** (2026-07-25 — the TV-measurement-encoding
> release). Implemented in `common`, the sensor firmware, and the receiver
> firmware. ⚠️ **The gateway parser has not been migrated yet** — it still
> expects v0.4 fixed 25-byte frames; updating it (via a `frame::parse` in
> `common`) is the next task, together with the switch to the opaque-payload
> MQTT envelope. This format will keep evolving (AEAD next); it's documented
> here so both ends have a stable target and versions can be diffed.
>
> **Changes since v0.4:** the fixed `SensorPacket` struct is replaced by the
> **TV measurement encoding** (type–value; see below), which makes the air
> packet — and therefore the frame — **variable-length**. `SensorObservation`
> gained a 1-byte `packet_len` and now carries the air packet as an opaque
> byte string; the receiver and gateway no longer know what's inside it. Wire
> structs are no longer memory-mapped (`repr(C, packed)`/bytemuck); both ends
> use explicit little-endian encode/parse functions. Field order changed
> (`age_ms` now precedes `rssi`). Frame size: 16 + N bytes (N = air-packet
> length; 29 bytes with today's three measurements).
>
> **Changes since v0.3:** device identity moved out of the payload — identity
> is the BLE **advertising address** (AdvA, derived from FICR `DEVICEADDR`),
> forwarded as `device_addr: DeviceAddr([u8; 6])` (LSB-first, BLE on-air
> order). `HardwareId(u64)` is gone. Frame shrank from 27 to 25 bytes.
>
> **Changes since v0.2:** `DeviceId` renamed to `HardwareId`; `humidity: u8`
> replaced by `rh_cpercent: u16`; `pressure_pa` removed. Frame shrank from 30
> to 27 bytes.
>
> **Changes since v0:** payload type changed from `SensorPacket` to
> `SensorObservation` (adds receiver-observed metadata — RSSI and age).

The receiver firmware ([firmware/receiver](../firmware/receiver)) emits framed
packets over USB CDC. The gateway reads `/dev/ttyACM0` (or a udev symlink such
as `/dev/homescope-receiver` — the deployed setup, see
[deploy/udev](../deploy/udev/99-homescope-receiver.rules)) and parses frames.

## Frame layout (variable length: 16 + N bytes)

```
+--------+--------+------------------------------------+----------+----------+
| MAGIC0 | MAGIC1 | SensorObservation (12 + N bytes)   |  CRC lo  |  CRC hi  |
| 0x48   | 0x53   | header (12) + air packet (N)       |       u16 LE        |
+--------+--------+------------------------------------+----------+----------+
   [0]      [1]    [2 .. 14+N)                           [14+N]     [15+N]
```

- **Magic (bytes 0-1):** ASCII `HS` = `0x48 0x53`. Frame-boundary marker — lets the gateway resync after errors or after opening the stream mid-frame.
- **Payload (bytes 2..14+N):** `SensorObservation`, encoded field-by-field (layout below). Its `packet_len` byte is what delimits the frame.
- **CRC (last 2 bytes):** CRC-16/IBM-SDLC over the **observation bytes only** (magic excluded; the `packet_len` byte **is** covered). Little-endian on the wire.

N is bounded by `SensorPacket::MAX_WIRE_LEN` = **252** (the BLE AD-structure
budget: 255-byte AD length − 1 type byte − 2 company-ID bytes), so the largest
possible frame is 268 bytes. With the current three-measurement packet, N = 13
and the frame is 29 bytes.

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

Defined in [common/src/observation.rs](../common/src/observation.rs). No longer
a memory-mapped packed struct — `SensorObservation::encode` writes the fields
explicitly, little-endian, in this order (offsets relative to the start of the
observation, i.e. frame byte 2):

| Offset      | Field         | Type        | Source                   | Meaning |
| ----------- | ------------- | ----------- | ------------------------ | ------- |
| `[0..6]`    | `device_addr` | `[u8; 6]`   | **receiver** (from AdvA) | The sensor's BLE advertising address, LSB-first (BLE on-air order) — a random static address derived from its FICR `DEVICEADDR`. Not part of the air payload. Rendered as 12 uppercase hex chars on MQTT/HTTP. |
| `[6..10]`   | `age_ms`      | `u32` LE    | **receiver**             | Milliseconds between BLE capture and USB-CDC send. See "Age and timestamps." |
| `[10]`      | `rssi`        | `i8` (dBm)  | **receiver**             | Signal strength at the receiver (typ. −30 to −110). `127` is the HCI "not available" sentinel — pass through, treat as suspect. |
| `[11]`      | `packet_len`  | `u8`        | **receiver**             | N = length of the air packet that follows. Delimits the frame. |
| `[12..12+N]`| `packet`      | `[u8; N]`   | **sensor** (opaque)      | The air packet, byte-for-byte as received over BLE. The receiver and gateway never interpret it beyond `seq` (below). |

The fixed header is 12 bytes (`SensorObservation::HEADER_LEN`); the maximum
observation is 12 + 252 = 264 bytes (`SensorObservation::MAX_LEN`).

`DeviceAddr` is a newtype around `[u8; 6]`, stored LSB-first exactly as the
address appears on air. It renders as 12 uppercase hex chars,
most-significant byte first (standard BLE address order, no colons). See
[common/src/device_addr.rs](../common/src/device_addr.rs).

## Air packet (`SensorPacket`) — TV measurement encoding

Defined in [common/src/packet.rs](../common/src/packet.rs) and
[common/src/measurement.rs](../common/src/measurement.rs). This is the payload
of the BLE `ManufacturerSpecificData` AD structure (company ID `0xFFFF`
during development), forwarded opaquely inside the observation:

```
+-----------------+------+---------+------+---------+----
| seq (u32 LE)    | id   | value   | id   | value   | …
+-----------------+------+---------+------+---------+----
  [0..4]            1 B    ID-implied length each
```

- **`seq`** — per-sensor monotonic counter, **fixed cleartext header, always
  at offset 0**. It does protocol work (receiver-side burst dedup, API-side
  replay/dedup, future AEAD nonce) and is the only part of the packet the
  receiver reads. Persisted on the sensor across reboots (retained RAM +
  flash checkpoint with jump-ahead), so it never goes backwards.
- **TV section** — *type–value*: each measurement is a 1-byte **measurement
  ID** followed directly by its value. The ID comes from the registry in
  `common` and binds semantics + wire representation + scale + unit, so the
  ID **implies the length** — there is deliberately no per-field length byte.
  Values are little-endian scaled integers; never floats.

### Measurement ID registry

| ID     | Meaning     | Repr  | Scale | Unit | Example |
| ------ | ----------- | ----- | ----- | ---- | ------- |
| `0x01` | battery     | `u16` | ×1    | mV   | `2950` = 2.950 V |
| `0x02` | temperature | `i16` | ×0.01 | °C   | `2143` = 21.43 °C |
| `0x03` | humidity    | `u16` | ×0.01 | %RH  | `4521` = 45.21 %RH |

The registry is the single source of truth
(`homescope_common::measurement::Measurement`); only sensor firmware encodes
it and only the API decodes it. If a metric ever needs different
range/resolution, a **new ID** is minted — per-ID reversible, no version
bump.

### Decode posture (strict)

Unknown ID, truncated value, or duplicate ID in one packet ⇒ **drop the whole
packet with a warning** — don't salvage already-parsed fields. Both ends are
ours; a malformed packet is a bug or foreign traffic, not something to
tolerate. (Company ID `0xFFFF` is the Bluetooth SIG test ID and must be
treated as shared airspace: anything in it is untrusted input until AEAD
lands.)

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
arrives at the receiver and is stored alongside the packet bytes in the
backlog channel (`ScannedPacket`). When the observation is eventually written
to USB-CDC, the delta becomes its `age_ms`.

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

For implementers writing a parser in another language. Byte-at-a-time state
machine:

- **Hunting:** read 1 byte. If it equals `0x48` → `SawMagic0`. Else stay.
- **SawMagic0:** read 1 byte. If `0x53` → `InHeader`. If `0x48` → stay in
  `SawMagic0` (preserves candidate). Else → `Hunting`.
- **InHeader:** read the 12-byte observation header. `packet_len` is its last
  byte; if `packet_len > 252` → `Hunting` (false sync).
- **InFrame:** read `packet_len + 2` more bytes (air packet + CRC). Verify
  CRC over the `12 + packet_len` observation bytes against the trailing
  little-endian u16. On success, emit the decoded observation. → `Hunting`
  either way.

Use a short read timeout (~100 ms is plenty — actual transmission is
sub-millisecond; the timeout only protects against partial-frame stalls). Any
I/O error, timeout, or CRC mismatch returns to `Hunting`. Don't try to
"salvage" bytes of a failed frame — at this packet rate, restarting the hunt
costs at most one frame.

Note the frame is sent across multiple USB packets (64-byte full-speed bulk
limit); USB packet boundaries are invisible at the tty level and carry no
framing meaning — only the magic/length/CRC do.

## Rust gateway implementation — ⚠️ migration pending

[gateway/src/decoder.rs](../gateway/src/decoder.rs) still implements the
**v0.4** fixed-25-byte decoder (`memchr` for magic + `Frame::try_from_bytes`).
The v0.5 plan: a `frame::parse` in `common` with a three-outcome result —
*incomplete* (need more bytes), *ok* (observation + bytes consumed), or
*corrupt* (bad magic/CRC ⇒ skip one byte and resync) — so the
`tokio_util::codec::Decoder` reduces to a loop over it and every
byte-position fact stays in `common`. Until that lands, the gateway cannot
parse what the receiver now emits.

## Planned: AEAD (ChaCha20-Poly1305, decrypt in the API)

Settled 2026-07-16 (see `NOTES-packet-tv-aead.md`). The TV section becomes
the ciphertext; `seq` stays cleartext at offset 0:

- Air packet becomes `[seq: u32][ciphertext: N][tag: 16 B]`; the cleartext
  context (`device_addr` from AdvA + `seq`) is bound as **associated data**,
  so a valid ciphertext can't be grafted onto another device or seq.
- Nonce is derived deterministically from the persisted `seq` (no random
  component — safe because keys are per-device and seq never repeats).
- **The USB-CDC frame and MQTT envelope shapes don't change** — the opaque
  packet blob just becomes ciphertext + tag. Receiver and gateway stay
  keyless and unmodified; only firmware (encrypt) and API (decrypt) change.

## Known limitations (will change in future versions)

- **The gateway still speaks v0.4** — see above; until its decoder and MQTT
  envelope are migrated, the pipeline is receiver-ahead-of-gateway.
- **No encryption yet.** The air packet is plaintext TV data and anything in
  radio range can forge well-formed packets (company ID `0xFFFF` is shared).
  Acceptable during development; fixed by the AEAD step.
- **No frame-level version field.** Bumping the format means updating both
  ends together (they share `common`, so version skew is a deploy-ordering
  concern, not a code one).
- **Limited BLE-side metadata.** The advertising address is forwarded (that's
  `device_addr`), but PHY, channel index, and per-event details are not.
