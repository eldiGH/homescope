# Multi-site deployment — roles, star of brokers, edge spooling

> **Status: ⏳ planned.** Written 2026-10-04. Turns the "Mosquitto bridge per
> site" item flagged in [site-room-topology.md](site-room-topology.md) from
> "only if the VPN flaps" into the recommended default for every remote site,
> and defines the deployment roles `deploy/deploy.sh` should offer. Nothing
> here is built yet: today `deploy.sh` installs the whole stack, including its
> own broker, on one Pi.

## The picture

```text
 remote site A                 remote site B                 central site
 ┌───────────────────┐         ┌───────────────────┐         ┌──────────────────────────┐
 │ receiver          │         │ receiver          │         │ (receiver, optional)     │
 │   │ USB-CDC       │         │   │               │         │   │                      │
 │ gateway           │         │ gateway           │         │ gateway ─┐               │
 │   │ localhost     │         │   │               │         │          ▼               │
 │ local broker ─────┼─bridge──┼───┼───────────────┼────────►│ central broker           │
 │ (spools)          │  over   │ local broker ─────┼─bridge─►│   │                      │
 └───────────────────┘  VPN    └───────────────────┘         │ API ─► TimescaleDB ─► Grafana
                                                             └──────────────────────────┘
```

- **One API and one database, at the central site.** Unchanged from the
  existing records: decryption, the device registry, dedup and storage live in
  exactly one place.
- **One central broker.** It may be a general-purpose broker the site already
  runs for other systems (home automation, Zigbee) — homescope must not assume
  it owns the broker (see [Roles](#roles)).
- **A star, not a mesh.** Each remote site bridges to the central broker only.
  Brokers at remote sites never talk to each other: no site needs another
  site's readings, and a mesh multiplies credentials and loop risks for nothing.
- **The remote site initiates the bridge.** Its local broker connects *out* to
  the central one — this survives NAT, and the bridge credential lives at the
  edge, where the ACL already confines it.
- **A receiver at the central site needs no bridge**: its gateway publishes
  straight to the central broker.

## Why spool at the edge (and why by default)

The sensors broadcast on their own schedule whether anyone is listening or not.
A reading that reaches a gateway while the link to the central broker is down
is **lost for good** if the gateway publishes straight to the remote broker —
rumqttc keeps a small in-flight window, not an outage's worth of data, and a
gateway restart drops even that.

Remote sites lose the link for more reasons than a VPN flap: their own internet
outages, the central site's outages, router reboots and maintenance on either
end. A local broker costs one small container and buys **store-and-forward**:

1. the gateway publishes to `localhost` — a link outage is invisible to it;
2. the local broker queues the bridged topics while the remote end is down
   (QoS 1, persistent session, persistence on disk);
3. on reconnect the bridge drains the queue to the central broker;
4. the API's durable session and the planned manual acks
   ([ingest-db-error-handling.md](ingest-db-error-handling.md)) carry it the
   rest of the way into the database.

Duplicates are the expected cost of QoS 1 redelivery across two hops; the
planned per-device `seq` check already exists to absorb them (it is the same
mechanism as replay protection and multi-receiver dedup). Timestamps are not a
problem either: `readings.time` comes from the gateway's `receivedAt − age`,
not from when the message finally arrives — which makes **NTP on every gateway**
(already flagged in site-room-topology.md § Clocks) a hard prerequisite.

## Bridge configuration (sketch)

On the remote site's broker:

```text
# reachable only by the local gateway: loopback, or the podman network the
# two containers share - never the LAN
listener 1883 127.0.0.1
allow_anonymous false
password_file /mosquitto/config/passwd   # the local gateway user

persistence true
persistence_location /mosquitto/data/
# global option - must come before any `connection` block
max_queued_messages 100000            # size for the longest outage you accept

connection central
address <central-broker-host>:1883
remote_username site-<site>
remote_password <from a secret file - see mosquitto-acl.md>
remote_clientid bridge-<site>        # stable: it keys the persistent session
cleansession false                    # keep the queue across reconnects
topic homescope/<site>/# out 1       # publish-only, QoS 1, own prefix only
```

- **Queue sizing**: a sensor reporting once a minute produces ~1 440 envelopes
  a day; `sensors × days × 1440` is the bound. Envelopes are a few hundred
  bytes, so a week of a whole house fits easily on any SD card — but set the
  limit deliberately: the default is 1 000, and past the limit the broker
  silently drops new messages, i.e. the outage loses its *latest* data.
- **Direction `out` only.** Nothing flows from the central broker to a remote
  site. If a downlink is ever needed (configuration pushed to gateways), add it
  as a separate, narrow `in` topic.
- **No `retain`** on envelopes — they are events, not state.
- **One credential per site.** As noted in [mosquitto-acl.md](mosquitto-acl.md),
  a bridge user writing `homescope/<site>/#` is indistinguishable from that
  site's gateway user, so the central ACL needs one user per *site*
  (`site-<site>`), not one per gateway. Several receivers at one site share it.
- The central broker sees the bridge as an ordinary client — no special
  configuration there beyond the user and its ACL.

## Roles

`deploy/deploy.sh` today converges a single all-in-one Pi. The multi-site
picture needs it to install by **role**, so the same scripts work in any
network:

| Role | Installs | Broker |
|---|---|---|
| `all-in-one` | gateway + receiver udev rule, API, TimescaleDB, Grafana, broker | own, local (today's behaviour; single house, development) |
| `server` | API, TimescaleDB, Grafana | own **or external** |
| `gateway` | gateway + receiver udev rule, local broker as bridge | local spool, bridged to the central broker |

What the roles imply for configuration — the parts a deployer must be able to
set without editing the scripts:

- **External broker**: host, port, username; the password as a secret file
  (the project's pattern for the KEK and the admin token). An external broker
  is someone else's — homescope documents the users and ACL lines it needs
  instead of shipping a `mosquitto.conf` for it.
- **Gateway** `SITE` (topic prefix) and the central broker address for the
  bridge.
- **Data location** for the database volume — a host with a dedicated data
  disk wants it there, not on the SD card.
- **Grafana bind address** — loopback when a reverse proxy fronts it, all
  interfaces only when nothing else does.
- A `server` that also has a receiver attached is `server` + a gateway
  publishing to the central broker directly — either a combined role or simply
  both roles on one host, without the bridge.

## Integrations

Unchanged from site-room-topology.md: integrations (e.g. Home Assistant MQTT
discovery) get plaintext readings from an **API republish after decrypt,
verify and dedup**, never from the gateways. With a shared central broker that
republish lands next to the integration's own topics — give the API a user
whose ACL allows writing exactly those topics and nothing else.

## Order of work

1. Site prefix + `devices.site`/`room` (site-room-topology.md) — the bridge
   topic and every ACL line depend on it.
2. Broker auth and ACLs (mosquitto-acl.md), including the `site-<site>` users.
3. Deployment roles in `deploy.sh`, external-broker support first: a central
   site that already runs a broker is the first real consumer.
4. The `gateway` role with the bridging local broker, then the first remote
   site.
5. NTP verified on every gateway before the second gateway goes live.

## Concepts this exercise touches

- Store-and-forward at the edge vs reliability at the core; where in a
  pipeline durability must start (at the first hop that can lose data)
- MQTT bridges: direction, QoS across hops, persistent sessions keyed by
  client id, queue limits and what happens when they fill
- At-least-once delivery + idempotent consumer (the `seq` check) as the
  standard alternative to exactly-once
- Topology choice: why a star beats a mesh when data flows one way
