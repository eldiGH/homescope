# Scanner-to-gateway wire protocol — v0.6 (prototype)

> **Status: prototype, v0.6 — complete end to end** (2026-07-29). `common`,
> the sensor firmware, the receiver firmware, the gateway (decoder loop over
> `frame::parse` plus the opaque-payload MQTT envelope) and the API (envelope
> subscription → TV decode → TimescaleDB) all speak v0.6. **AEAD is next.**
>
> **Changes since v0.5:** the air packet gained a two-byte **magic** (`b"HP"`,
> air-side only — the receiver checks and strips it) and a one-byte **version**
> ahead of `seq`. `SensorPacket` downstream is therefore `[ver][seq][body]`.
> `SensorPacket::parse` handles only that prefix and stays version-blind;
> `SensorPacket::decode` dispatches on `ver` and yields the canonical
> `Metrics`. Air packet grows 13 → 16 bytes with today's three measurements;
> frames 30 → 33. See [the version charter](#the-version-charter).
>
> **Changes since v0.4:** the fixed `SensorPacket` struct is replaced by the
> **TV measurement encoding** (type–value; see below), which makes the air
> packet — and therefore the frame — **variable-length**. The frame is now
> **content-agnostic**: it gained a u16 payload-length field after the magic
> and carries the payload as an opaque byte string (framing knows nothing
> about observations). `SensorObservation` carries the air packet opaquely;
> the receiver and gateway no longer know what's inside it. Wire structs are
> no longer memory-mapped (`repr(C, packed)`/bytemuck); both ends use
> explicit little-endian encode/parse functions. Field order changed
> (`age_ms` now precedes `rssi`). Frame size: 6 bytes overhead + payload
> (30 bytes total with today's three measurements).
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

## Frame layout (variable length: 6 + N bytes)

The frame is **content-agnostic**: it transports an opaque payload byte
string and knows nothing about what's inside (`common/src/frame.rs`; payload
types plug in via the `Encode` trait).

```
+--------+--------+--------+--------+----------------------+----------+----------+
| MAGIC0 | MAGIC1 | LEN lo | LEN hi | payload (N bytes)    |  CRC lo  |  CRC hi  |
| 0x48   | 0x53   |     u16 LE      | opaque               |       u16 LE        |
+--------+--------+--------+--------+----------------------+----------+----------+
   [0]      [1]      [2]      [3]     [4 .. 4+N)              [4+N]     [5+N]
```

- **Magic (bytes 0-1):** ASCII `HS` = `0x48 0x53`. Frame-boundary marker — lets the gateway resync after errors or after opening the stream mid-frame.
- **Length (bytes 2-3):** N, the payload length, u16 little-endian. Capped at `frame::MAX_PAYLOAD_LEN` = **1024** — a transport-level anti-stall bound (a parser must reject a bigger claim *before* waiting for the bytes), deliberately not derived from any payload type.
- **Payload (bytes 4..4+N):** opaque. In this protocol it's a `SensorObservation` (layout below), but the framing layer neither knows nor checks that.
- **CRC (last 2 bytes):** CRC-16/IBM-SDLC over **length + payload** (bytes 2..4+N; magic excluded, length field covered — a corrupted length is detectable). Little-endian on the wire.

Frame overhead is 6 bytes (`frame::OVERHEAD`). With today's observation
(11-byte header + 16-byte air packet = 27-byte payload) a frame is 33 bytes;
the observation's maximum payload is 263 bytes (frame max: 269).

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
a memory-mapped packed struct — `SensorObservation` implements the frame's
`Encode` trait and writes its fields explicitly, little-endian, in this order
(offsets relative to the start of the frame payload, i.e. frame byte 4):

| Offset      | Field         | Type        | Source                   | Meaning |
| ----------- | ------------- | ----------- | ------------------------ | ------- |
| `[0..6]`    | `device_addr` | `[u8; 6]`   | **receiver** (from AdvA) | The sensor's BLE advertising address, LSB-first (BLE on-air order) — a random static address derived from its FICR `DEVICEADDR`. Not part of the air payload. Rendered as 12 uppercase hex chars on MQTT/HTTP. |
| `[6..10]`   | `age_ms`      | `u32` LE    | **receiver**             | Milliseconds between BLE capture and USB-CDC send. See "Age and timestamps." |
| `[10]`      | `rssi`        | `i8` (dBm)  | **receiver**             | Signal strength at the receiver (typ. −30 to −110). `127` is the HCI "not available" sentinel — pass through, treat as suspect. |
| `[11..]`    | `packet`      | `[u8; …]`   | **sensor** (opaque)      | The air packet, byte-for-byte as received over BLE — **all remaining payload bytes** (the frame's length field delimits it; the observation has no length field of its own). The receiver and gateway never interpret it beyond `seq` (below). |

The fixed header is 11 bytes (`SensorObservation::HEADER_LEN`); the maximum
observation is 11 + 252 = 263 bytes (`SensorObservation::MAX_ENCODED_LEN`).

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
+--------+-----------------+------+---------+------+---------+----
| ver    | seq (u32 LE)    | id   | value   | id   | value   | …
+--------+-----------------+------+---------+------+---------+----
  [0]      [1..5]            1 B    ID-implied length each
```

(On air this is preceded by the two-byte magic, which the receiver strips —
see [Air-packet magic + version header](#air-packet-magic--version-header).)

- **`ver`** — protocol version of everything after `seq`. Read by the API,
  never by the receiver. See [the version charter](#the-version-charter).
- **`seq`** — per-sensor monotonic counter, **fixed cleartext header** (offset
  1 downstream, offset 3 on air). It does protocol work (receiver-side burst
  dedup, API-side replay/dedup, future AEAD nonce) and is the only *structured*
  field the receiver reads. Persisted on the sensor across reboots (retained
  RAM + flash checkpoint with jump-ahead), so it never goes backwards. Its
  position and width are pinned by the receiver's dedup contract — see
  [the magic + version header](#air-packet-magic--version-header).
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
lands. v0.6's magic filters the accidental share of that; only the AEAD tag
filters the deliberate share.)

An unknown ID is unrecoverable rather than merely unknown: with no per-field
length byte, the parser cannot skip a value whose width it doesn't know, so
everything after it is unparseable too. `Measurements` therefore fuses after
the first error rather than trying to resynchronise.

**A missing measurement is not an error.** Partial packets are the point of
the encoding — a node with no humidity sensor, or one whose SHT45 read failed,
still reports what it has, and the API stores the absent metric as a NULL
column. The rule is: *reject when the packet is unusable or untrustworthy,
otherwise store what arrived.* A packet with **zero** measurements is rejected
(`DecodeError::Empty`) — well formed on the wire, useless as a row.

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
- **InHeader:** read the 2-byte length field (u16 LE). If `len > 1024`
  (`MAX_PAYLOAD_LEN`) → `Hunting` (false sync — reject **before** waiting
  for the claimed bytes, or a corrupted length stalls the parser).
- **InFrame:** read `len + 2` more bytes (payload + CRC). Verify CRC over
  the length field + payload (`2 + len` bytes) against the trailing
  little-endian u16. On success, emit the payload (then parse it as a
  `SensorObservation`). → `Hunting` either way.

The Rust implementation (`frame::parse` in `common`) collapses this into a
single function over a growing buffer with three outcomes: *incomplete*
(need more bytes — not an error), *ok* (payload + total bytes consumed), or
*corrupt* (bad magic / oversized length / bad CRC), which carries a
**`discard` hint**: the number of bytes to drop before retrying, computed
inside the parser as the distance to the next magic-candidate byte (or the
whole buffer if there is none). It is always ≥ 1, so the stream is
guaranteed to make progress, and it never skips an unexamined magic
candidate — a false magic match may hide a real frame's start one byte
later, and the hint lands exactly on it.

Use a short read timeout (~100 ms is plenty — actual transmission is
sub-millisecond; the timeout only protects against partial-frame stalls). Any
I/O error, timeout, or CRC mismatch returns to `Hunting`. Don't try to
"salvage" bytes of a failed frame — at this packet rate, restarting the hunt
costs at most one frame.

Note the frame is sent across multiple USB packets (64-byte full-speed bulk
limit); USB packet boundaries are invisible at the tty level and carry no
framing meaning — only the magic/length/CRC do.

## Rust gateway implementation and the MQTT envelope

[gateway/src/decoder.rs](../gateway/src/decoder.rs) implements a
`tokio_util::codec::Decoder` as a loop over `frame::parse`: Incomplete →
`Ok(None)` (wait for more serial bytes); Ok → advance `consumed` and parse
the payload as a `SensorObservation`; Corrupt → advance by the returned
`discard` and rescan. A CRC-valid frame whose observation fails to parse is
dropped and the loop continues with the next frame (this symptom means the
receiver and gateway disagree about the protocol version).

Each observation is republished as an **opaque JSON envelope** on
`homescope/sensors/<device-addr>/envelope` (QoS 1): the receiver-observed
metadata in cleartext, the air packet forwarded byte-for-byte as base64
(`ObservationEnvelope` in `common/src/observation_envelope.rs`, camelCase):

```json
{
  "deviceAddr": "F1E2D3C4B5A6",
  "rssi": -63,
  "receivedAt": "2026-07-26T12:34:56.789Z",
  "packet": "BQAAAAECPAg..."
}
```

`receivedAt` is stamped by the gateway as `now − age_ms` (see "Age and
timestamps"); `packet` is the `[seq][TV…]` air packet, untouched. The
gateway never looks inside it — decode (and later AEAD verify/decrypt)
happens in the API.

### API side (implemented 2026-07-28)

`homescope-api` subscribes to `homescope/sensors/+/envelope` and turns each
envelope into a row:

1. Resolve `deviceAddr` against the `DeviceRegistry`. Unknown ⇒ warn-once and
   drop (no auto-registration — the `devices` table becomes the AEAD key
   registry, and a row must exist before readings are accepted).
2. `SensorReading::try_from(&envelope)` — `SensorPacket::parse` over the
   base64 blob, then walk `Measurements` into one `Option` per metric.
   `SensorReading` lives in `common/src/reading.rs`; it is the *decoded*
   shape, not a wire type, and carries no version field.
3. Insert with `ON CONFLICT DO NOTHING` against a
   `UNIQUE (device_id, seq, time)` constraint. The `time` column is in the
   key because TimescaleDB rejects unique indexes on a hypertable that omit
   the partitioning column. Because `received_at` is stamped by the gateway
   and carried in the envelope unchanged, an MQTT **redelivery** collides and
   is silently ignored; **multi-receiver** dedup still needs the planned
   per-device seq monotonicity check, since two receivers stamp different
   `received_at` values for the same `seq`.

Metric columns are nullable (`temp_degc`, `rh_percent`, `battery_mv`);
`time`, `device_id`, `seq` and `rssi` are NOT NULL.

## Air-packet magic + version header

Landed 2026-07-29 (v0.6). Two fields sit in front of `seq`:

```
on air (inside ManufacturerSpecificData, company ID 0xFFFF):
+--------+--------+--------+-----------------+------+---------+----
| 'H'    | 'P'    | ver    | seq (u32 LE)    | id   | value   | …
| 0x48   | 0x50   | u8     |                 |      |         |
+--------+--------+--------+-----------------+------+---------+----
  [0]      [1]      [2]      [3..7]            [7..]

after the receiver strips the magic — this is `SensorPacket`, the blob that
travels in the observation and the MQTT envelope:
+--------+-----------------+------+---------+----
| ver    | seq (u32 LE)    | id   | value   | …
+--------+-----------------+------+---------+----
  [0]      [1..5]            [5..]
```

**Magic — `b"HP"`, two bytes, air-side only.** Company ID `0xFFFF` is the
Bluetooth SIG *test* identifier, so the airspace is shared with every
unbranded dev board in range. The magic is what lets the receiver reject
foreign traffic before it costs anything. Declared as a `[u8; 2]`, never a
`u16`, so there is no endianness question and the check is a slice compare.
Deliberately different from the frame's `"HS"`, though **not** for any
correctness reason: because the receiver strips it, the air magic never enters
the USB stream, so the two constants never coexist in one byte sequence and
identical values would work. They are kept distinct because they answer
different questions — the frame magic only has to be *findable* after stream
corruption (its value is arbitrary), while the air magic has to be *unlikely*
to collide with a foreign `0xFFFF` advertisement (its value is a selectivity
choice that might grow to three bytes if the airspace gets noisy). Sharing one
constant would couple two decisions that have no reason to move together, and
would quietly become unsafe if anyone ever added a raw-air-bytes passthrough.

Since it is a convention rather than an invariant, it is **not** enforced with
a `const` assert — `packet.rs` and `frame.rs` deliberately don't depend on each
other, and an assert whose violation breaks nothing trains readers to skim past
the asserts that do matter. Cross-referencing doc comments on the two constants
carry it instead.

Unrelated but worth not confusing with the above: arbitrary payload bytes
*can* contain `0x48 0x53` by chance (a `device_addr` pair, a `seq`, a
measurement value). That is inherent to magic-delimited framing and is what the
length field and CRC are for.

The receiver **strips it**: past the dongle you are on a point-to-point USB
link that `frame.rs` already delimits, and then on MQTT where the topic
identifies the sender. Forwarding a constant that no downstream layer can
learn anything from would only invite re-validation at a layer where passing
tells you nothing. Each layer strips its own header.

**Why the magic matters more than it looks.** The receiver's dedup cache is
`LruCache<DeviceAddr, u32, 32>` and the only gate in front of it is "company
ID `0xFFFF` and at least four bytes". Foreign advertisers therefore claim LRU
slots keyed by their own `AdvA`, and roughly 32 of them **evict real sensors
from the dedup cache**. An evicted sensor's next burst no longer dedups, so
all ~20 advertising events forward as distinct packets — 20× amplification
into MQTT and the API, precisely when the airspace is busiest. The magic is
the cheapest possible fix and it is not primarily about bandwidth.

**Version — `u8`, ahead of `seq`, travels downstream.** A single monotonic
counter, not major/minor: every change to a packet this small is breaking,
and a minor-version escape hatch is exactly the reasoning that produces
silent misdecodes. 256 values is a century at one or two revisions a year.

It goes **before** `seq` because a version field must be readable before you
parse anything else, *including the fields ahead of it*. Behind `seq` you get
a circular dependency — locating the version requires already knowing `seq`'s
width, which is what the version was supposed to tell you. The practical
consequence is diagnostic: with `ver` at a fixed offset an unrecognised
packet still reports `unknown version 7 from AA:BB:CC`; behind `seq` the same
packet is undiagnosable and indistinguishable from noise or corruption.

**The receiver does not read `ver`.** It checks the magic, reads `seq` at its
fixed offset for dedup, and forwards everything from byte 2 on. This is
deliberate:

- The magic is *constant*, so filtering on it never forces a dongle reflash.
  Filtering on `ver` would make every protocol bump a reflash — and one dongle
  is destined for the far end of a VPN.
- A node still running old firmware must stay **visible**. If the dongle
  dropped unknown versions, a sensor you forgot to reflash would vanish with
  no signal, indistinguishable from a dead battery or an RF problem. Forwarded,
  the API reports `unsupported version 1 from AA:BB:CC` and you know which node
  to go find.

### The version charter

> **`ver` names the format of everything after `seq`. Nothing else.**

That scope is forced by decisions made elsewhere, and stating it prevents the
field from becoming a catch-all:

- The **prefix** (`magic`, `ver`, `seq`) is dongle-visible and therefore frozen.
  Changing it is a **new magic**, not a version bump — a bump could not help,
  because the receiver never reads `ver`.
- **Measurement semantics** belong to the ID registry. A new metric, a new
  resolution, a new unit, a status flag: all new IDs, no bump.
- What remains is the **body layout**, and `ver` is its name.

#### When to bump

| Change | Action |
| --- | --- |
| New metric, resolution, unit, status flag | **New measurement ID.** No bump. |
| New node variant / different populated sensors | **Nothing.** TV already expresses it. |
| Body layout changes (batching, encryption, TLV, crypto parameters, a packet-type byte) | **Bump `ver`.** |
| `seq` moves or resizes; anything in the prefix changes | **New magic.** |

Never reuse a number. Never bump without a body change that an older parser
would misread — a bump that changes nothing teaches you to ignore the field.

#### What it is actually for

The realistic cases, in the order we think they'll arrive:

1. **Batching several readings per advertisement.** Today one packet is one
   reading, so sampling resolution and radio cadence are the same number. To
   sample every 15 s but transmit every 5 min, the body becomes
   `[count][Δt][TV][Δt][TV]…` — roughly 60 bytes inside a 254-byte budget, for
   20× the time resolution at the same radio duty cycle. The TV registry cannot
   express this; it is a body-layout change.
2. **Crypto parameter changes.** ChaCha20-Poly1305 → XChaCha20-Poly1305 if
   seq-derived 96-bit nonces prove awkward, a different nonce derivation, a
   different tag length. Crypto choices get revisited on a multi-year horizon
   more often than packet layouts do, and none of them is a new ID.
3. **A packet-type byte**, if transmissions ever diversify (boot announcement,
   diagnostic dump). Introducing one is itself a body change, so it arrives via
   a bump — but note `ver` must **not** be overloaded to mean type ("v3 = the
   status format" is a field meaning two things and validating neither).
4. **TV → TLV.** Lower probability, since deploying the API before flashing
   firmware keeps unknown IDs from occurring. It is the classic reason protocols
   grow length prefixes, and it is a body change if it ever happens.

#### Why it earns its byte even at one version

Two things, neither of which is authenticity — AEAD covers that, and a version
byte adds nothing there:

- It converts *silent misparse* into *named rejection*. This is not
  hypothetical: a node still on pre-v0.5 firmware emits `[seq][temp][rh][batt]`,
  and the only reason it isn't writing garbage rows today is that indoor
  humidity keeps the stale bytes outside the valid-ID range (see
  [Known limitations](#known-limitations-will-change-in-future-versions)).
- After AEAD it separates **stale firmware** from **bad key**. Without it both
  surface identically as a failed tag check, and those two have completely
  different fixes — reflash versus re-provision. With it, the API can report
  `device X is on v1, expected v2` before it ever touches the key.

The deciding argument is asymmetry rather than any single use case: keeping the
byte and never bumping costs one byte, one field and one match arm, while
adding it later costs a fleet-wide flag day *and* leaves you diagnosing a mixed
fleet with the diagnostic missing.

Note what is **not** an argument for it. AEAD arrives as a hard cutover — the
API stops accepting cleartext outright — so there is no dual-accept window and
therefore no downgrade path for a version byte to close.

#### Handling versions in code

The pattern is **one canonical model, one adapter per version** — an
anti-corruption layer. `ver` never enters the canonical type; it is a wire
concern that dies at the parse boundary, so every version's parser yields the
same `SensorReading`. Layout in `common`:

- `SensorPacket::parse` handles the **version-invariant prefix only** and must
  **not** validate `ver` — the receiver calls it purely to read `seq`, and a
  version check there would resurrect exactly the stale-node blindness the
  design avoids.
- Body access performs the dispatch and returns `UnsupportedVersion(u8)`.
- `body` stays **private**. That privacy, not the `match`, is what actually
  enforces the version check: a public body lets a caller read the section
  without dispatching, and the first time that happens `ver` becomes
  decoration.
- Each version's body **decoder** lives in its own module (`packet/v1.rs`) with
  its own `MAX_BODY_LEN`.

**Encoding is single-version.** A sensor only ever emits the current format, so
there is exactly one encoder and it lives with the prefix, writing a named
`CURRENT_VERSION` rather than a literal. Old encoders are *not* kept: an old
encoder tested against its own parser verifies round-tripping, not wire
compatibility — the pair can drift together and still pass. Regression tests
for retired versions use **golden byte arrays**, ideally captured from real
firmware, which cannot drift.

**Growing the canonical model.** When a new version adds a field, construct
`SensorReading` with every field spelled out and never `..Default::default()`.
An added field then fails to compile in every older version's parser until you
decide what it means there; with a default, an old parser silently starts
reporting a value it never received.

**What `Option` does not cover: cardinality.** `Option<T>` says "this version
didn't carry that field". Batching says "this version carries *N* readings",
which is a different shape — one v2 packet becomes several rows. So keep all
`SensorReading` construction inside the single `TryFrom<&ObservationEnvelope>`
so the eventual 1→N change touches one function and the insert loop rather than
scattered call sites. The schema already tolerates it: `UNIQUE (device_id, seq,
time)` includes `time`, so several readings sharing one `seq` don't collide.

**`seq` stays the dedup key.** Its position and width are pinned by the
receiver's contract, which is a real constraint but an inert one: `seq` cannot
be removed (it is the AEAD nonce source *and* the API's replay check) and a
`u32` at one packet per minute wraps in ~8000 years. A CRC32-over-payload dedup
key was considered — it would make the dongle fully blind to structure — but it
does not unpin `seq` either: without a per-transmission-varying field, two
identical consecutive readings become byte-identical and dedup would swallow
the second, losing the liveness signal. (Use CRC32, not CRC16, if this is ever
revisited: at 1 packet/min CRC16 collides about once per device per 45 days,
silently dropping a real reading.) Post-AEAD the Poly1305 tag is already a
per-packet unique value and is the natural key if the change is ever made.

## Planned: AEAD (ChaCha20-Poly1305, decrypt in the API)

Settled 2026-07-16 (see `NOTES-packet-tv-aead.md`), layered on top of v0.6.
The TV section becomes the ciphertext; the `ver` + `seq` header stays
cleartext:

- Air packet becomes `[magic][ver][seq: u32][ciphertext: N][tag: 16 B]`; the
  cleartext context (`device_addr` from AdvA + `ver` + `seq`) is bound as
  **associated data**, so a valid ciphertext can't be grafted onto another
  device, version or seq. `ver` *must* be in the AAD or it is flippable.
  The magic is **not** in the AAD — it is a constant both sides already know,
  so it contributes nothing, and it isn't transmitted past the receiver.
- Nonce is derived deterministically from the persisted `seq` (no random
  component — safe because keys are per-device and seq never repeats).
- **The USB-CDC frame and MQTT envelope shapes don't change** — the opaque
  packet blob just becomes ciphertext + tag. Receiver and gateway stay
  keyless and unmodified; only firmware (encrypt) and API (decrypt) change.

## Known limitations (will change in future versions)

- **No encryption yet.** The air packet is plaintext TV data and anything in
  radio range can forge well-formed packets (company ID `0xFFFF` is shared).
  Acceptable during development; fixed by the AEAD step. Note the v0.6 magic
  filters *accidental* collisions only — about 1 in 65536 of random foreign
  traffic still passes, and anyone can transmit the constant deliberately.
  It is a noise filter, never an authenticity check.
- **No version field yet** (arriving in v0.6). Today, bumping the format means
  updating both ends together — they share `common`, so version skew is a
  deploy-ordering concern rather than a code one. The gap is visible right
  now: a node still running pre-v0.5 firmware emits `[seq][temp][rh][batt]`,
  whose `seq` happens to land at the same offset, so the receiver forwards it
  and the API parses byte 4 (the low byte of `temp_cdegc`) as a measurement
  ID. That is almost always an unknown ID and the packet is rejected — but
  only *almost*: it decodes cleanly into plausible stored values when the
  temperature's low byte and the humidity's high byte both happen to fall in
  `1..=3`, i.e. below roughly 10 %RH. Indoors that never fires. That is luck,
  not design, and it is what the version byte exists to end.
- **No frame-level (USB-CDC) version field.** The frame layer is versioned by
  this document only; v0.6's version byte lives in the air packet, which is
  the layer whose producers are flashed firmware in the field.
- **Limited BLE-side metadata.** The advertising address is forwarded (that's
  `device_addr`), but PHY, channel index, and per-event details are not.
