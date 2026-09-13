# Packet TV redesign + seq persistence + AEAD

> **Status: ✅ implemented** — as protocol v0.5 (TV encoding, 2026-07-25 → 28),
> v0.6 (air magic + version header, 2026-07-29) and v0.7 (ChaCha20-Poly1305,
> live end to end 2026-07-31), with the flash-persisted seq counter in between.
> **This is the design record; `docs/protocol.md` is the current wire format**
> and wins wherever the two differ — see § As built below. Still open from this
> plan: the API's per-device seq check ([ingest-db-error-handling.md](ingest-db-error-handling.md)) and
> the integrations republish.

Context: decisions settled 2026-07-16
(architecture review). Supersedes the earlier bitmask idea and the old
fixed-`SensorPacket`-plus-random-nonce AEAD sketch in architecture.md's
history. Guiding principle for the whole block: **extensible, not generic** —
adding sensor type N+1 must be a small local change at each layer; handling
*unimagined* sensor types with zero code changes is a non-goal (that's a
platform, and platforms already exist).

**Terminology**: **TV = type–value** — each field is a measurement ID (type)
followed directly by its value; the ID implies the length. This is the
length-implied member of the TLV family (3GPP TS 24.007 calls this format
"TV"; there is deliberately **no length byte** — see below). BTHome works the
same way.

## Plan order (owner's sequencing)

1. ✅ **Finish the DeviceAddr refactor** — identity = AdvA from FICR
   `DEVICEADDR`; protocol v0.4, 25-byte frames.
2. ✅ **TV packet** — protocol v0.5, variable-length frames.
3. ✅ **Seq persistence on the sensor** — prerequisite for AEAD nonces; also
   fixes the reboot-resets-seq caveat in the API's replay/dedup check.
4. ✅ **AEAD** — ChaCha20-Poly1305, per-device keys, decrypt in the API.
   (v0.6's magic and version header landed between 3 and 4.)

## As built — where the implementation departed from this plan

- **A header the plan did not have.** v0.6 put `[magic b"HP"][ver: u8]` in front
  of `seq`. The receiver checks and strips the magic; `ver` precedes `seq`
  because a version field must be readable before the fields it describes. The
  rules for bumping it are the version charter in `docs/protocol.md`.
- **Measurement IDs as assigned:** `0x01` battery mV `u16`, `0x02` temperature
  centi-°C `i16`, `0x03` humidity centi-%RH `u16`. The table in §2 was
  illustrative.
- **AAD is `device_addr ‖ ver ‖ seq`** — `ver` joined once it existed; the magic
  is excluded, being constant and stripped before the API sees it. The nonce is
  `seq` in the low 4 bytes, with no random component.
- **The USB-CDC frame** is `[magic "HS"][len: u16 LE][payload][crc: u16]`, not a
  single payload-length byte.
- **The MQTT envelope** is `{deviceAddr, rssi, receivedAt, packet}` on
  `homescope/sensors/<device-addr>/envelope`. `seq` travels inside the packet's
  cleartext header rather than as a field of its own.
- **Seq persistence:** the flash checkpoint is built — a two-page circular log,
  reservation block 1024, jump-ahead on boot. The retained-RAM fast path for
  System OFF waits on sleep optimisation.
- **Decryption sits in `packet::decode`, above the `ver` dispatch,** so version
  modules stay keyless and a failed tag means *re-provision* while
  `UnsupportedVersion` means *reflash*.
- **`ver` stayed 1 through the AEAD cutover** — a deliberate one-off, possible
  only because nothing was deployed yet.
- **Keys are per-device, in UICR**, written by `homescope-provision` (see
  [provisioning.md](provisioning.md)); the shared bring-up key is retired from the firmware.
- **The API replay check did not land before AEAD**, as §4 hoped. It is still
  open.

## 2. TV packet (protocol v0.5)

- Air packet: `[seq: u32][id: u8][data][id][data]…`. `seq` stays a **fixed
  header outside the TV section** — it does protocol work (dedup, replay,
  AEAD nonce) and must be findable at a fixed offset, cleartext, forever.
- **Measurement ID registry lives in `common`** — one enum, each ID binding
  semantics + wire repr + scale + unit, with a `wire_len()`:

  | id  | meaning     | repr | scale | unit |
  |-----|-------------|------|-------|------|
  | 0x01| temperature | i16  | ×0.01 | °C   |
  | 0x02| humidity    | u16  | ×0.01 | %RH  |
  | 0x03| battery     | u16  | ×1    | mV   |
  | 0x04| pressure    | u32  | ×0.01 | Pa   |

  (Illustrative — assign for real when implementing. BTHome's object-ID table
  is worth stealing from / aligning with: <https://bthome.io/format/>.)
- **ID implies length — no per-field len byte.** A len byte would only buy
  unknown-ID skipping (old parser, newer firmware); we control both ends and
  the posture is strict anyway: **unknown ID or truncated data ⇒ warn + drop
  the whole packet** (don't salvage already-parsed fields). Duplicate ID in
  one packet ⇒ reject (firmware bug).
- **No self-describing repr tags, no f32 on the wire.** Repr/scale are fixed
  per ID; if a metric ever needs more range/resolution, **mint a new ID**
  (e.g. temperature-hires i32 ×0.001) — reversible per-ID, no version bump.
- **Receiver + gateway are semantics-blind.** Packet bytes are opaque from
  the receiver onward; only firmware encodes, only the API decodes. Frame
  gains a payload-length byte (frame-level, for USB-CDC framing — distinct
  from the absent per-field length); CRC unchanged. `SensorPacket` stops
  being a `Pod` struct — encode by appending, decode with a cursor/iterator
  over the byte slice (works in no_std; the API can consume the iterator
  directly, no intermediate Vec needed).
- **MQTT envelope (new shape lands here, not at AEAD):** the gateway can no
  longer emit `tempDegc` etc. once the payload is opaque. Envelope = JSON
  with cleartext metadata + base64 blob:
  `{deviceAddr, seq, rssi, receivedAt, payload: base64(TV bytes)}`.
  JSON+base64 chosen over binary deliberately: the gateway must inject
  `received_at` anyway (timestamping near reception — see protocol.md), so an
  envelope schema exists either way; JSON keeps `mosquitto_sub` useful and
  the size overhead is nothing at our volume. When AEAD lands the blob just
  becomes ciphertext+tag — **zero envelope/gateway change at step 4**.
- **DB stays wide** — one nullable column per *physical metric* in canonical
  units (f64 °C etc.); `time/device_id/seq/rssi` stay NOT NULL. Multiple wire
  encodings of one metric converge into one column (the API converts before
  insert). EAV/narrow storage rejected at this scale. Adding a metric
  end-to-end = firmware emits new ID + API match arm + one `ALTER TABLE ADD
  COLUMN` migration. Existing metric columns become nullable in the same
  migration that adds the first optional metric.

## 3. Seq persistence (sensor)

Why: (a) ChaCha20-Poly1305 **nonce reuse under the same key is catastrophic**
— a rebooting sensor restarting at seq=0 would reuse nonces; (b) the API's
replay/dedup check assumes per-device monotonic seq, and reboots (battery
swap, watchdog, panic) currently reset it.

Sketch (work out details when implementing):

- **System OFF wake cycles**: keep the counter in a **retained RAM section**
  (nRF52840 RAM retention in System OFF) — no flash wear on the every-minute
  path.
- **Real reboots / battery swaps**: checkpoint to a dedicated internal flash
  page every N counts (e.g. N=1024) and on boot resume at
  `last_checkpoint + N` (jump-ahead — never risk going backwards). Wear math:
  at 60 s cadence, one write per ~17 h; nRF52 page endurance 10k cycles ⇒
  decades. Consider two alternating slots for power-loss-during-erase safety.
- Persisted monotonic seq means the AEAD nonce needs **no random component**
  (supersedes the old 4-byte-random-nonce-lower sketch).

## 4. AEAD (ChaCha20-Poly1305, per-device keys, decrypt in the API)

- On-air payload: `[seq: 4 B cleartext][ciphertext(TV section): N][tag: 16 B]`.
- **AAD** = the cleartext context: `device_addr` (from AdvA) + `seq` — so a
  valid ciphertext can't be grafted onto another device or sequence number.
- **Nonce** (96-bit): deterministic from the persisted `seq` (e.g. seq in the
  low 32 bits, remainder fixed) — safe because keys are per-device and seq
  never repeats per device. Decide exact construction at implementation.
- Keys: 32 B per device, `devices` table = key registry (row must exist
  before ingest accepts — the existing no-auto-registration rule), stored
  **envelope-encrypted under a KEK** from the API's secrets. Firmware side:
  key in **UICR**, written once by the `homescope-provision` CLI — see
  **[provisioning.md](provisioning.md)** (2026-07-20), which supersedes the build-time
  `DEVICE_KEY` env → link_section sketch this note originally referenced.
- API replay check: reject `seq ≤ last_seen` per device — same mechanism as
  multi-receiver dedup and MQTT-redelivery idempotency (see
  [ingest-db-error-handling.md](ingest-db-error-handling.md); ideally the seq check lands with that
  work, before AEAD).

## Keyless gateways — re-examined 2026-07-16, reaffirmed

Gateway-decrypt was re-litigated against the "useful to others" goal and
rejected again: keyless is simultaneously cheaper (no key distribution or
rotation machinery) and safer (symmetric AEAD ⇒ verify = can forge; the
remote-house Pi is the most exposed box), and gateway-decrypt would re-couple
the gateway to the measurement registry (version-skew data loss). Decision is
reversible in one direction only: an **opt-in** gateway decrypt mode can be
added later without protocol changes (envelope already carries everything).

**Flip condition**: someone besides the owner actually wants to run the
dongle+gateway standalone — then add opt-in decrypt (or an unencrypted /
BTHome-compatible firmware build, which is the more realistic third-party
path anyway).

**Integrations get plaintext from the API, not the gateway**: after
decrypt + verify + seq-dedup, the API republishes decoded readings JSON
(e.g. `homescope/<site>/sensors/<device-addr>/state`, or Home Assistant MQTT
discovery for near-zero-config HA). Downstream-of-verification means
integrations see deduplicated, authenticated data; gateway-decrypted MQTT
would hand them duplicates and replays. This republish is the last, optional
step — everything works without it.

## Concepts this exercise touches

- Tagged encodings and the TLV-family taxonomy (TLV vs TV vs LV — 3GPP
  TS 24.007) — why the ID should own semantics+repr+scale, and where the
  self-describing road ends (SenML/CBOR)
- Fixed-point wire encodings vs floats for telemetry
- AEAD: nonce-uniqueness discipline, associated data as context binding,
  why verify=forge with symmetric keys shapes *where* keys can live
- Flash wear-leveling for monotonic counters (checkpoint + jump-ahead)
- nRF52840 retained RAM across System OFF
- End-to-end principle: encrypt at producer, decrypt at final consumer,
  dumb intermediaries
