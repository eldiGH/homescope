# Site/room + MQTT topology

> **Status: 🔶 partly done.** Decisions settled 2026-07-14. The architecture
> decisions below are built — decrypt-in-API with keyless gateways, the device
> registry handle, warn-once for unknown devices. The two changes this record
> exists for are not started: the `devices.site`/`room` columns and the
> gateway's `SITE` topic prefix. Nor is the per-device seq check.

Deployment picture: **two houses on a VPN**, one central Mosquitto broker and a
single API instance; the remote house gets its own Pi running a gateway, a
receiver and its sensor fleet.

## The two changes (do together, one PR-sized unit)

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
  Only worth it if VPN reliability turns out to be a real problem.
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
