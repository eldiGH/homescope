# Site/room + MQTT topology

> **Status: 🔶 partly done.** Decisions settled 2026-07-14. The architecture
> decisions below are built — decrypt-in-API with keyless gateways, the device
> registry handle, warn-once for unknown devices. The two changes this record
> exists for are not started: the `devices.site`/`room` columns and the
> gateway's `SITE` topic prefix. Nor is the per-device seq check.
>
> ⚠️ **Revised 2026-10-05.** The data model is now `sites`, `rooms` and
> `device_placements` tables (placement history) rather than columns on
> `devices`. The topic prefix stays, re-argued for a world with AEAD. The
> schema and the prefix are now independent pieces of work. See
> [Revised 2026-10-05](#revised-2026-10-05-sites-rooms-placement-history).

Deployment picture: **two houses on a VPN**, one central Mosquitto broker and a
single API instance; the remote house gets its own Pi running a gateway, a
receiver and its sensor fleet.

## The two changes (do together, one PR-sized unit)

⚠️ *2026-10-05:* change 1 is superseded by
[the tables below](#revised-2026-10-05-sites-rooms-placement-history), and
the two changes no longer need to land together.

**1. A `devices` migration adding `site` and `room`.**

`room` — and `site` on the device row — is **device semantics**: owner-assigned
metadata. Sensors do not know where they are; a sensor moved to another shelf
does not get reflashed. This data lives only in the database and is joined in at
query time. The `POST /devices` request (and `homescope-provision provision`,
through `--site`/`--room`) gains the fields at the same time.

**2. The gateway's `SITE` setting and topic prefix.**

```text
homescope/<site>/sensors/<device-addr>/envelope
```

`site` in the topic is **transport provenance** — "which gateway heard this" —
set per gateway through the environment, in `gateway/src/config.rs` beside
`MQTT_HOST`, `MQTT_PORT` and `RECEIVER_PATH`. It is not the same concept as the
device's `site` column, even though the two normally agree: a device near a
house boundary could be heard by either gateway.

Current code to touch (line numbers as of 2026-09-13):

- `gateway/src/main.rs:39` — the topic is built as
  `format!("homescope/sensors/{}/envelope", …)`; prefix it with the configured
  site.
- `api/src/ingest.rs:123` — the subscription `homescope/sensors/+/envelope`
  becomes `homescope/+/sensors/+/envelope`.

Why the prefix is worth having before it is needed: it enables per-gateway
broker ACLs. Each gateway's credentials can be restricted to publishing under
`homescope/<its-site>/#`, so a compromised remote Pi cannot impersonate the
other house's fleet — see [mosquitto-acl.md](mosquitto-acl.md).
⚠️ *2026-10-05:* this reasoning predates AEAD; the prefix is kept for other
reasons, see
[re-argued after AEAD](#the-topic-prefix-re-argued-after-aead-2026-10-05).

## Revised 2026-10-05: sites, rooms, placement history

> **Settled with the owner 2026-10-05; supersedes change 1 above.** ✅ Schema
> built the same day: migration `20261005101457`, after `20261005100951` made
> `device_addr` the devices primary key. Nothing reads it yet — the follow-ups
> listed below are open.
> The
> principle stands: placement is owner-asserted device semantics that firmware
> never sees, so moving a sensor never means a reflash. What changes is the
> storage. Placement gets its own tables and its own timeline, instead of two
> columns on `devices`.

```text
sites ──< rooms ──< device_placements >── devices
```

- **`sites`**: `id`, a unique `name`, a nullable `display_name` and a nullable
  `elevation_m`.
  - `name` is the site's one identifier everywhere: gateway `SITE`, the topic
    segment, the broker user `homescope-<name>` and this row. All four follow
    the same rule: one topic level, `^[a-z0-9-]+$`. It is a `CHECK` here, and a
    `Site` newtype with `FromStr` in `common`.
  - `elevation_m` is the one location attribute homescope has a use for:
    reducing the BMP581's station pressure to sea level (about 0.12 hPa/m).
  - No latitude/longitude. Nothing would read them, and a nullable column is a
    trivial migration later.
  - No seeded rows: site names belong to the deployer, and the project is meant
    to be portable.
  - Sites are created explicitly, never automatically from a topic, for the same
    reason devices are not auto-registered.
- **`rooms`**: `id`, `site_id` (NOT NULL, FK), free-text `name`, and
  `UNIQUE (site_id, name)`, because thor's kitchen and freya's kitchen are
  different rooms. On asgard, room names match the Home Assistant areas, so a
  discovery republish can set `suggested_area`.
- **`device_placements`**: `device_addr` (FK to the devices primary key),
  `room_id` (nullable FK), `placed_at timestamptz`, and
  `PRIMARY KEY (device_addr, placed_at)`.
  - A placement lasts until the device's next `placed_at`. A view computes
    that end with `LEAD()`: `placement_periods(device_addr, room_id,
    valid_from, valid_to)`, where `valid_to` NULL means "still there". Overlaps are therefore impossible by construction, and there is
    nothing to enforce. The stricter shape, a `tstzrange` with an exclusion
    constraint, makes every change two statements and buys nothing here.
  - `room_id` NULL means taken down, for example back in the drawer.
- **`RESTRICT` on the FKs into `rooms` and `sites`.** A room that appears in
  any placement, past or present, cannot be deleted; rename it instead. Its
  history refers to it.
- **A device's site is derived**, through placement → room → site. `devices`
  gets neither a `site` nor a `room` column. The current placement is the
  device's latest row, found with a `LEFT JOIN LATERAL`, the same pattern
  `last_seen` uses.

Operations. The UI asks "move or correction?", not "preserve history?":

| Operation | Effect | History |
|---|---|---|
| **move** | insert a row at now, or at an explicit earlier `placed_at` | kept |
| **correction** | update the room of an existing row ("it was always in the bedroom") | rewritten |
| **take down** | insert a row with `room_id` NULL | kept |

Back-dating comes for free. For example, the soak node can be recorded at thor
from 2026-07-14 and at odin from 2026-10-04.

**Rejected: `readings.room_id`.** Storage is not the argument, since an integer
per row compresses to nothing. The real reasons:

1. **It ties ingest to rooms.** Ingest would have to stamp rooms, so the
   `DeviceRegistry`, loaded once at startup, would need them. Every room change
   through the API would then have to update that in-memory copy, or ingest
   stamps stale rooms. That is cache coherence on the hot path. With
   placements, ingest does not know rooms exist.
2. **A correction becomes an `UPDATE` across hypertable chunks.** That is slow,
   and limited on compressed chunks. With placements it is one row.
3. **It is a different kind of fact.** A reading holds what was *observed* at
   ingest: time, seq, RSSI, values, and the topic site if ever stored. A room
   is *asserted* by the owner, and assertions get corrected. They belong in a
   table with their own timeline.

**No gateways table.** The broker's users and ACL decide who may publish where.
A list in the database could not enforce anything the broker has not already
enforced, and it would drift from the ACL. Per-site liveness comes from topic
traffic and the bridge state topic (see
[deployment-topology.md](deployment-topology.md)).

Not built yet, and following the schema:

- HTTP endpoints for sites, rooms and placements.
- Optional placement at `POST /devices` time, plus the matching
  `homescope-provision` flag.
- Golden tests for every new wire field.
- Grafana `site`/`room` variables drawn from `placement_periods`.

### The topic prefix, re-argued after AEAD (2026-10-05)

The prefix stays, but for different reasons than "Why the prefix is worth
having" above gives. It is independent of the schema: neither blocks the other,
although this record originally bundled them.

- ⚠️ **The original argument predates AEAD.** It said per-site ACLs stop a
  compromised Pi from impersonating the other house, but it was written on
  07-14, and AEAD arrived on 07-31. Gateways are keyless, so impersonation is
  cryptographically impossible whatever the topics are. The argument never
  fully held anyway: under its own prefix a site credential can name any device
  address, so it also needed an API-side cross-check.
- **What the prefix gives now:**
  1. **Authenticated provenance per reading.** It is the only site information
     the broker vouches for, because each site's credential can write only
     under its own prefix.
  2. **Per-site liveness without the database.** Silence on `homescope/thor/#`
     means thor's gateway, receiver or link is down, which is distinct from a
     dead sensor. The bridge state topic covers only the link.
  3. **It is cheapest now.** Nothing is deployed and no ACL rule references the
     old topic.
- **Topic site ≠ placement site: log at `info`, never reject.** Rejecting would
  protect only against a site relaying *authentic* packets from another house,
  which is harmless data. It would also drop the readings of a sensor that was
  moved before its placement was updated.
- **Topic device address ≠ the envelope's `deviceAddr`: reject and warn.** The
  gateway builds both from one observation, so this is a framing invariant. A
  mismatch means a bug or tampering.
- **Topic site with no `sites` row: warn once and keep ingesting.** The broker
  authorized the publisher, so the missing row is configuration drift. The
  device registry is what gates data.
- **`SITE` is required on the gateway,** with no default, and validated at
  startup. A wrong site is exactly what an ACL drops silently.
- **Provenance storage.** For now, a field on `handle_envelope`'s span. A
  `readings` column comes only together with the per-device seq check, which
  decides whose copy is kept.
- **Rollout is expand/contract.** The API subscribes to both the old and the new
  topic for one release, before any gateway switches. A broker queues nothing
  on a topic nobody subscribes to, not even for a durable session.
- **Rejected: letting the bridge add the prefix** with mosquitto's topic
  remapping. The central site's gateway publishes with no bridge, so that would
  mean two mechanisms.

## Settled architecture decisions (don't relitigate)

- ✅ **Decrypt in the API, not the gateway** (built with protocol v0.7). This
  *reverses* an earlier idea of decoding in the gateway. Gateways stay keyless
  and thin — the remote-house Pi is the most exposed component, no
  key-distribution mechanism is needed, and keys live only in the API and its
  database. MQTT carries an **envelope**: cleartext `deviceAddr`, `rssi` and
  `receivedAt`, plus the base64 `packet` blob, whose cleartext header carries
  `seq`. So `mosquitto_sub` stays useful for debugging while readings stay
  confidential. **Re-examined and reaffirmed 2026-07-16** (see
  [packet-tv-aead.md](packet-tv-aead.md)): symmetric AEAD means verify = can
  forge, so key-holding gateways would widen the compromise blast radius.
  Keyless is reversible later through an *opt-in* gateway-decrypt mode (flip
  condition: third parties running the dongle and gateway standalone).
  ⏳ Integrations get plaintext from an **API republish after decrypt, verify
  and dedup** (a `…/state` topic, or Home Assistant MQTT discovery), not from the
  gateway — not built yet.
- ✅ **Device lookup in the API is a handle** — built as `DeviceRegistry` in
  `api/src/devices/registry.rs`: a `#[derive(Clone)]` struct over
  `Arc<RwLock<HashMap<DeviceAddr, Arc<Device>>>>`, constructed in `main` and
  passed to the tasks that need it — no globals, no `OnceCell`. The rule for a
  *sync* `RwLock` holds: never keep the guard across an `.await`. `get` clones
  an `Arc<Device>` out and drops the guard, so the insert can `.await` while
  holding the device.
- ✅ **Unknown devices: warn once, then drop. No auto-registration**
  (`api/src/ingest/unknown.rs`). The `devices` table is the key registry, so a
  row with a key must exist before the API accepts a device's readings.
  Auto-registration would make key provisioning a race.
- ⏳ **A per-device `seq` monotonicity check in the API** does double duty:
  AEAD **replay protection** and **multi-receiver dedup**. A second receiver and
  gateway is the BLE equivalent of a range extender — there are no Zigbee-style
  repeaters for advertising broadcasts — so overlapping receivers hearing the
  same burst is the expected scaling story, and both publish the same `seq`.
  Design settled in [ingest-db-error-handling.md](ingest-db-error-handling.md).

## Flagged for later (not this change)

- **A Mosquitto bridge per site** — a local broker at the remote house spools
  during VPN flaps and forwards to the central broker when the link returns.
  ~~Only worth it if VPN reliability turns out to be a real problem.~~
  ⚠️ *Updated 2026-10-04:* now the recommended default for every remote site —
  link outages have more causes than VPN flaps, and an envelope missed while
  the link is down is lost for good. See
  [deployment-topology.md](deployment-topology.md).
- **Broker auth** — see [mosquitto-acl.md](mosquitto-acl.md): per-gateway
  credentials that publish only under their own site prefix, and API credentials
  that only subscribe.
- ⚠️ **Clocks, once a second gateway exists.** `readings.time` is stamped on the
  gateway (`received_at = now − age_ms`), while `key_valid_from` comes from the
  database. `lastSeen`, `homescope-provision verify` and the planned seq check
  all compare the two. On a single Pi they share one clock; a remote gateway
  running ahead could make a reading received just before a key rotation count
  as seen under the new key. NTP on every gateway is a prerequisite — the failure
  modes are spelled out in
  [ingest-db-error-handling.md](ingest-db-error-handling.md) § Known costs.

## Concepts this exercise touches

- Topic design: encoding *provenance* in the topic and *semantics* in the
  database — and why conflating them bites when a device is heard across sites
- MQTT wildcard subscriptions (`+` is single-level) and ACL prefix patterns
- The handle pattern (a `Clone` struct over `Arc<lock>`) as the idiomatic
  alternative to globals for shared state in tokio apps
- Sync-lock-in-async discipline: guard lifetimes against `.await` points
- Temporal data: assertions with their own timeline (a placement log closed by
  `LEAD()`) vs observations stamped on the row; corrections vs moves
- Normalisation: a derivable attribute (a device's site) is not stored twice
