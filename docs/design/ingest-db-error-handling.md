# Ingest DB error handling — bail vs log-and-continue

> **Status: ⏳ planned.** Checked 2026-09-13: the ingest writer still logs an
> insert failure and moves on, rumqttc still auto-acks on poll, and the
> per-device seq check is not built. Written 2026-07-14, when the devices-table
> PR changed insert failures from `bail!` to log-and-continue — a trade of
> durability for uptime that should be revisited deliberately. The seq-check
> design below was settled 2026-08-07.

## The problem

Today the writer — `store_envelopes`, via `handle_envelope` in
`api/src/ingest.rs` — logs `db error` and continues when `db::insert_reading`
fails.

rumqttc **auto-acks QoS 1 publishes when they are polled** from the event
loop, long before the reading reaches Postgres. So mosquitto's persistence
(`clean_session=false`, QoS 1, persistent broker storage) only protects
messages that were never *delivered* to the API. Once polled, a reading's
survival depends entirely on what the process does with it.

What each strategy loses during a **database outage**:

- **Log-and-continue (current):** the loop keeps polling, so every reading is
  acked, its insert fails, and it is dropped. **Total loss for the entire
  outage**, silent except for one `error!` line per reading.
- **Bail and restart (previous):** the process exits on the first failed
  insert, and the quadlet restarts it. While the process is down, the broker
  queues for the durable session and replays on reconnect. It loses at most
  the channel contents (≤ 256) and whatever was acked in the moments before the
  first failure — per restart cycle, not per outage. **Most data survives.**

So bail is *better for durability*, counterintuitively. Its risk is a **poison
message** — a reading that fails on every attempt (a constraint violation, bad
data) and drives an infinite crash loop. That risk shrank in the devices PR: the
foreign key is resolved through `DeviceRegistry` before the insert, so the
classic FK-violation poison case can no longer happen.

## Options, worst to best

1. **Keep log-and-continue** — acceptable only if readings are disposable during
   database outages. They are not; battery telemetry from soak tests is exactly
   the data being collected.
2. **Restore `bail!`** — cheap, durable, poison-loop risk accepted. A good
   interim.
3. **Distinguish error kinds** — `sqlx::Error::Database(e)` with a constraint
   kind is a per-row problem (log and skip); anything else — I/O, pool timeout,
   connection — is infrastructure (bail). A middle ground that still loses
   acked in-flight readings.
4. **Manual acks — the real fix.** `MqttOptions::set_manual_acks(true)`, and ack
   **after** the insert succeeds. Broker persistence then covers database
   outages end to end: unacked messages are redelivered on reconnect, and
   nothing is lost.

## What manual acks actually require

Not a one-liner:

- **The ack must travel with the envelope.** The channel carries
  `(ObservationEnvelope, Publish)` — rumqttc acks by `client.ack(&publish)` — and
  the writer acks only once the `INSERT` returns `Ok`. Decryption happens in the
  writer, after the channel, so the envelope is what travels.
- **Only a failed insert stays unacked.** An envelope from an unknown device, or
  one that fails decryption, will fail identically on every redelivery. Those
  must be acked *and* dropped, or they become exactly the poison messages that
  make redelivery loop.
- **The drop-on-full path changes meaning.** Today `try_send` on a full channel
  drops and warns. With manual acks, dropping *without* acking is correct and
  free: the broker redelivers later. The warning becomes "backpressure engaged",
  not "data lost".
- **Redelivery means duplicates.** After a crash with messages unacked, the
  broker resends some that may already be stored. That needs idempotent
  ingest — **already done** as of 2026-07-28: `UNIQUE (device_id, seq, time)` +
  `ON CONFLICT DO NOTHING`. A redelivery is the *identical* envelope with the
  *identical* `received_at`, so the triple matches and the insert is a no-op.
  (Written before that constraint existed, this bullet once said the seq check
  was needed first. It is not — that check is for multi-receiver dedup, where
  two gateways stamp *different* `received_at` values and the constraint does
  not fire. Separate PRs.)
- **It interacts with graceful shutdown** (see
  [api-graceful-shutdown.md](api-graceful-shutdown.md)): envelopes drained at
  shutdown must be acked before the disconnect, or they are redelivered on the
  next start — harmless, given the unique constraint, but noise.

## Per-device seq check — design (settled 2026-08-07)

### What it is actually for

Three jobs, with very different odds of mattering:

- **Multi-receiver dedup** — the real one, and not speculative: it arrives with
  the second gateway. Each dongle dedups its own burst through its LRU, then
  both forward one envelope. Different `received_at` ⇒ different `time` ⇒
  `UNIQUE (device_id, seq, time)` does *not* fire ⇒ two rows for one
  measurement.
- **MQTT redelivery** — already covered. A redelivery is the identical envelope
  with the identical `received_at`, so the triple matches and `ON CONFLICT DO
  NOTHING` eats it. ⚠️ This stays true under manual acks, so the durability work
  above does **not** depend on this check. The two are separable; do them as
  separate PRs.
- **Replay** — real but weak. AEAD gives authenticity, not freshness: a captured
  packet re-sent verbatim authenticates, because nothing about it was altered.
  But the attacker needs Coded-PHY range of the house and can only re-send
  values that genuinely occurred — no forgery, no chosen values. The harm is "a
  stale reading presented as fresh".

So: build it for dedup, take replay protection as a side effect, and build it
when the second gateway lands. One device and one receiver need none of it.

### The check

Monotonicity, scoped to the key epoch:

```sql
SELECT MAX(seq) FROM readings
 WHERE device_id = $1 AND time > <that device's key_valid_from>
```

Accept only `seq > MAX`; NULL (no readings under this key yet) accepts
anything.

`MAX(seq)`, **not** the last row by `time`. They diverge exactly when packets
arrive out of order, and `ORDER BY time DESC LIMIT 1` would let one late
straggler lower the watermark and reopen the window it had just closed. The
high-water mark is what is wanted.

**Why the epoch scoping.** `seq` must be unique *per key*, not per device — that
is the nonce-uniqueness requirement, nothing more. When the key rotates, the seq
space legitimately restarts, and the replay window has to restart with it, or a
re-provisioned sensor is rejected forever with a message saying "replay".
`key_valid_from` — renamed from `key_updated_at` for exactly this reason, see
[provisioning.md](provisioning.md) — is already the epoch marker, so this needs
**no new column**. A `last_seq` column would be derived state that can drift
from the table it is derived from. (Not a performance argument: at ~0.1 writes a
second, a hot-updated counter column would be free. It is redundant, and that is
the objection that survives scale.)

**Counter resets must be supported.** `ERASEUICR` leaves application flash
alone, so `seq_counter.rs`'s pages survive a normal re-provision — but
`ERASEALL` wipes them, and once APPROTECT is in use that is the *only* way back
into a locked chip. The ingest side must tolerate a reset regardless.

⚠️ *Superseded in part (2026-08-26):* this section originally concluded that the
provisioning tool should never deliberately reset the counter.
[provisioning.md](provisioning.md) §0 settled the opposite for one case — the
tool *will* clear the seq pages, but only in the same operation that installs a
new key, located from the firmware ELF's own symbols. That is safe because no
window exists in which an old key and a fresh counter coexist; a standalone
"reset seq" command stays forbidden.

Note the asymmetry, which is easy to get backwards: a **surviving counter under
a new key** is fine — nonce uniqueness is per key, so continuing at seq=50000
under K2 is unremarkable. A **reset counter under a reused key** is
catastrophic. Only the second is dangerous, and "provisioning always mints a new
key" is what makes it unreachable.

### Nonce reuse: the alarm

If the counter ever resets while the key stays the same, the sensor re-emits
nonces it has already used. Under ChaCha20-Poly1305 that is worse than the
usual "reveals the plaintext XOR": the one-time Poly1305 key is derived from
(key, nonce), so reuse leaks it, and the attacker can **forge arbitrary
packets** for that device from then on.

- Frame it as **key compromise, not sensor compromise.** Nobody touched the
  hardware; the cause is a chip erase, a botched reflash, or a bug in the
  jump-ahead. "Sensor compromised" sends you to the enclosure; "rotate this
  device's key and distrust its recent data" sends you to the bench, which is
  where the answer is.
- The API is a **detector, not the defence.** The defence is that every path
  which can reset the counter also mints a new key. This is the smoke alarm for
  when that process gets violated.
- Keep the alarm distinct from the rejection. Most rejections are ordinary
  duplicates sitting at or just below `MAX`. A reset looks different — `seq` far
  below `MAX`, arriving long after. `MAX - seq` past a threshold is a clean
  discriminator, and it is the difference between a debug line and a red error.

### Known costs

- **Strict monotonicity drops out-of-order arrivals permanently.** It only loses
  data in one shape: two receivers with *asymmetric* reception, where the slower
  path is the only one that heard a given packet (A hears seq=100 but not 101;
  B hears 101 but not 100 and is faster; 101 lands first, and 100 — the only
  copy — is rejected). That is not exotic; it is the reason to add a second
  receiver at all. But it is one reading out of a per-minute stream. Ship the
  strict version. If it ever matters, the upgrade is IPsec's anti-replay window:
  accept when `seq > MAX`, **or** when `seq > MAX - W` and that seq is not
  already stored. Two extra conditions, not a redesign.
- **Two clocks.** `readings.time` is `received_at`, computed on the *gateway* as
  `now() - age_ms`; `key_valid_from` comes from the database. Gateway slow ⇒
  fresh readings look pre-epoch, are excluded from `MAX`, and everything is
  accepted — fails open, mildly. Gateway *fast* ⇒ old-key readings land ahead of
  `key_valid_from`, count as post-epoch, `MAX` returns the old counter's
  high-water mark, and the freshly provisioned device restarting at zero is
  rejected **forever** — a fail-closed brick that looks exactly like a bad
  provisioning. NTP makes this sub-second and irrelevant; it is listed because
  the failure is silent and permanent, and the clock is the last place anyone
  would look. The same skew applies to `lastSeen` and `homescope-provision
  verify`, which bound readings by `key_valid_from` in the same way. Structural
  fix if it ever bites: an integer `key_generation` on both tables instead of a
  timestamp. Immune to skew, but it is a column on the hypertable — do not pay
  for it speculatively.
- **Atomicity rests on the single writer.** One sqlx writer draining one mpsc
  channel serializes the read-then-insert. A second writer task would quietly
  reintroduce the race, so say so in a comment — and keep `UNIQUE (device_id,
  seq, time)` as the backstop. Constraints are enforcement, queries are policy;
  enforcement should survive an application bug.

### Cost

Fine. `time > key_valid_from` constrains the partitioning column, so TimescaleDB
can exclude every chunk before the epoch, and within the survivors the
`(device_id, seq, time)` index gives a backward scan that stops at the first
row: a handful of index descents per packet, once a minute per device. The thing
that would have been expensive — an unbounded `MAX(seq)` across all chunks — is
exactly what the epoch predicate prevents, so the clause does double duty.

The closest evidence so far is the `lastSeen` query (2026-09-13), which bounds
`readings` by `key_valid_from` the same way: on the dev database it planned as
an ordered chunk append over `(device_id, time DESC)`, newest chunk first, with
older chunks never executed. There the exclusion happens at run time, since the
epoch comes from a joined row; with the epoch passed as a parameter, it can
happen at plan time.

## Suggested order

1. Restore `bail!` now (or option 3, if feeling fancy) — a one-line durability
   win.
2. The per-device seq check — needed anyway once the second gateway lands.
3. Manual acks.

## Concepts this exercise teaches

- QoS 1 semantics: ack ≠ processed; at-least-once means *duplicates by design*
- Why idempotency is the partner of every at-least-once transport
- Crash-only design: process death as a legitimate error-handling strategy,
  given a supervisor and a durable queue upstream
- The `sqlx::Error` taxonomy — transient versus permanent failures
