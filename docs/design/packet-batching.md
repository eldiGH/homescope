# Packet batching (protocol v2 body)

> **Status: ⏳ deferred.** Idea recorded 2026-07-29 while working out the version
> charter; deliberately **not** for now. Prerequisites, in order: watchdog →
> sleep optimisation → this. Kept because it is the concrete case that justifies
> the `ver` byte existing at all, and because the schema interaction below is
> easy to get wrong if it is designed in a hurry.

## The problem it solves

Today one packet is one reading, so **sampling resolution and radio cadence are
the same number**. Better time resolution means transmitting more often; less
radio means sampling less often. Batching decouples them.

Concretely: sample every 15 s, transmit every 5 min.

| | now | batched |
|---|---|---|
| samples/hour | 60 | 240 |
| bursts/hour | 60 | 12 |
| radio time/hour (400 ms burst) | 24 s | 4.8 s |

Four times the time resolution at one fifth of the radio time. It catches
transients a 60 s cadence smooths away — a window opening, a shower running, a
door left ajar — which is the actual product argument. It is **not** primarily
a battery argument: per `docs/architecture.md`, battery life is
self-discharge-dominated at this cadence, so treat the radio saving as a bonus,
not the justification.

## Wire shape

Body only. The prefix (`magic`, `ver`, `seq`) is untouched, so **no new magic** —
this is a plain `ver` bump. See `docs/protocol.md` → *The version charter*.

```
[magic][ver=2][seq: u32]  [count: u8]  [Δt: u16][TV…]  [Δt: u16][TV…]  …
                          ^-------------- v2 body --------------------^
```

- **`Δt`** — seconds *before the packet's own emission*, as a `u16` (18 h of
  range, plenty). Wall time per reading is then `received_at − Δt`, reusing the
  existing `age_ms` machinery unchanged: the gateway already computes
  `received_at = now − age_ms` for the packet, and each reading offsets back
  from that. No clock sync, same as today.
- **`count`** — explicit, rather than "parse until the bytes run out", so a
  truncated batch is an error instead of a silently short one.
- **One `seq` per packet**, shared by every reading in the batch. `seq` stays
  packet-level: it is the dedup key, the replay counter and the AEAD nonce
  source, all per-transmission concepts.

The body is sealed like v1's: decryption happens in `packet::decode` *above* the
`ver` dispatch, so a v2 body decoder is keyless, exactly as `v1.rs` is.

Capacity: 254 B budget − 7 B prefix − 1 B count − the 16 B Poly1305 tag (landed
with v0.7) = 230 B. At 11 B per reading (2 B `Δt` + 9 B for three measurements)
that is **20 readings** — exactly the ~20 a 5-minute window at 15 s sampling
needs, with no headroom for the overlap below. Overlap would mean a shorter
window, a longer sample interval, or fewer metrics per reading.

## The two things that need real thought

**1. Loss granularity gets worse.** Losing one packet today loses one reading;
losing one batched packet loses twenty. At ~99 % delivery that trades a rare
small gap for a rare large one, which is worse for charts.

Mitigation: **overlap the batches** — each packet carries the last N readings
rather than only the new ones, so a single loss is repaired by the next
transmission. It costs airtime that is nearly free, but see the capacity note
above.

**2. Overlap breaks the current uniqueness key.** This is the trap. The
constraint today is `UNIQUE (device_id, seq, time)`, and `seq` is *per packet*.
Two overlapping packets carrying the same reading have **different `seq`**, so
the constraint does not collide and the reading is stored twice.

If overlap is adopted, the readings key has to become `UNIQUE (device_id,
time)` — `time` is the reading's identity, `seq` is the transmission's. Decide
that *before* writing the encoder, because it also changes what the planned
per-device seq check (see
[ingest-db-error-handling.md](ingest-db-error-handling.md)) can dedup.

(Non-overlapping batches are fine under the existing key, since every reading
has a distinct `time`. The interaction only bites with overlap.)

## Code impact

Small, and the seams are already in the right place as of the v0.6 work:

- `common/src/packet/v2.rs` — a new body decoder. `v1.rs` is untouched.
- The dispatch in `packet.rs` gains one arm.
- **Cardinality is the one real change.** `Option<T>` expresses "this version
  didn't carry that field"; it cannot express "this version carries N of them".
  So the decode seam goes from returning one set of measurements to returning an
  iterator of `(Δt, measurements)`, and `v1` yields exactly one item at `Δt = 0`
  — v1 is a batch of one. The canonical `SensorReading` never changes shape.
- `SensorReading::open_envelope(&envelope, &cipher)` becomes 1→N. Keeping all
  `SensorReading` construction inside that one function is what makes this a
  local change rather than a scattered one.
- The sensor side is the bigger lift: sampling has to decouple from advertising
  (a ring buffer of pending readings plus a separate sample timer), and the
  buffer has to survive System OFF once sleep optimisation lands — which means
  it belongs in retained RAM, alongside the seq counter's fast path.

## Why not now

- It needs sleep optimisation to be worth anything. Batching while the node is
  awake between bursts saves nothing.
- The `Δt` encoding and the overlap/uniqueness decision both want to be made
  against measured delivery rates, not guessed ones — and the soak test has not
  produced those numbers yet.
- The fleet is still tiny. The migration cost only grows, but not fast enough to
  rush a schema decision.
