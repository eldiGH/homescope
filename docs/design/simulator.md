# `homescope-sim` — testing without hardware

> **Status: ⏳ proposed 2026-10-05 — nothing built.** Written after step 2 of
> the srv01 rollout, where every hardware-free test had to be improvised: a
> Python one-off building a USB-CDC frame by hand, `socat` for a fake dongle,
> and `mosquitto_pub` with hand-written envelope JSON. Those covered the
> gateway, the broker ACL and the API's topic handling. They could not cover
> the one path that matters most, because none of them could produce a packet
> the API can actually open.

## The gap it closes

Everything after authentication — **decrypt, decode, insert** — needs a packet
sealed under a key the API holds. Today the only such packets come from a
provisioned board, over the air, through a receiver. In dev that rarely holds:
- **Production-keyed boards don't open in dev.** Sypialka's key is wrapped
  under the production KEK, which never comes to the PC.
- **Dev-keyed boards are lost by restores.** A board keyed against dev loses
  its key row whenever `just db-restore` replaces the dev database.

So the database insert in `ingest::handle_envelope` was first exercised at
runtime by real hardware in production. It is checked against the schema at
compile time, and nothing more. The same applies to every rejection path past
authentication:
- unknown measurement ID
- truncated TV section
- duplicate ID
- empty body
- wrong `ver`
- failed tag

## What it would be

A host binary in the workspace, built from parts that already exist:

| Need | Already in the repo |
|---|---|
| a device the API knows, and its plaintext key | `POST /devices` returns the key once; `rotate-key` returns a fresh one; `homescope-provision`'s API client and profile store |
| a sealed packet | `common`: `PacketCipher`, `SensorPacket::encode`, the `Measurement` registry |
| a USB-CDC frame | `common::frame::encode` + `SensorObservation` |
| an envelope and its topic | `ObservationEnvelope::from_observation`, `EnvelopeTopic` |
| broker credentials | the dev broker's `homescope-dev` user |

One run:

1. **Get a key.** For each simulated device, call `rotate-key`, or
   `POST /devices` on the first run. Rotating on every run is deliberate: it
   opens a new key epoch (`key_valid_from`), so the simulated `seq` may restart
   at 0 without looking like a replay. That's the same rule a re-provisioned
   board follows. It also means no key has to be stored between runs.
2. **Generate readings:** a temperature/humidity day curve plus a draining
   battery, enough for every dashboard panel.
3. **Emit them in one of two modes:**
   - **`dongle`**: open a pseudo-terminal pair itself (no `socat`), print the
     path, and write USB-CDC frames into it. Run the real gateway with
     `RECEIVER_PATH` pointed at it. This exercises the gateway's decoder,
     including resync after corrupt bytes.
   - **`mqtt`**: publish envelopes straight to the broker as the dev site's
     user. That skips the gateway and is the quickest way to test the API.
4. **Optionally inject faults,** each one mapped to the behaviour it should
   produce:

| Fault | Expected |
|---|---|
| garbage bytes / bad CRC between frames | gateway discards and resyncs, next frame decodes |
| oversize `len` | gateway's `Corrupt` path, no stall |
| tampered ciphertext or tag | API rejects: failed tag |
| packet sealed for device A, sent as device B | API rejects: failed tag (address is AAD) |
| unknown measurement ID / duplicate ID / empty body | API rejects the packet |
| a missing metric | API stores NULL for it |
| repeated `seq` with the same `receivedAt` | stored once (`UNIQUE (device_addr, seq, time)`) |
| repeated `seq` with a new `receivedAt` | stored twice today; rejected once the per-device seq check lands |
| topic address ≠ envelope address | API rejects with a warning (`decode_publish`) |
| another site's prefix | broker drops it silently (ACL); `mqtt` mode can show it with MQTT v5 |
| unknown device | API warns once and drops |

A `just sim` recipe would start the dev stack, the gateway on the simulator's
pseudo-terminal, and the simulator, all in one go.

## Rules

- **Dev only, fail-closed.** It takes a `homescope-provision`-style profile and
  refuses anything not marked dev. Rotating a key on production would
  disconnect a real sensor, which is why the profile check comes before any
  API call.
- **Recognisable addresses.** Simulated devices use the `C0FFEE` prefix, e.g.
  `C0FFEE000001`. These are valid static-random addresses (top bits `11`) and
  obvious in logs and in `GET /devices`. A real FICR value could collide only
  by chance, and since registering a sim device would then return 409 rather
  than overwrite, a collision is loud.
- **No new wire code.** Every byte is produced by the same `common` functions
  the firmware and gateway use. A simulator with its own encoder would test
  itself.

## Open questions

- **Separate crate, or a `simulate` subcommand of `homescope-provision`?**
  Provision already has the API client, profiles and fail-closed
  confirmations. But it's a hardware tool built on probe-rs, and a simulator
  should build and run anywhere. The leaning is a separate crate that reuses
  `api-types`.
- **CI.** The `mqtt` mode against `compose.dev.yml` in CI would turn the fault
  table into an integration test. It needs the dev stack inside CI, and that's
  worth doing only once the table is stable.

## Relation to other work

- [provisioning.md](provisioning.md) — key issuance and the key-epoch rule the
  simulator relies on.
- [ingest-db-error-handling.md](ingest-db-error-handling.md) — the per-device
  seq check, whose replay and dedup cases are fault rows above.
- [deployment-topology.md](deployment-topology.md) — the dev broker that
  carries the production ACL.
