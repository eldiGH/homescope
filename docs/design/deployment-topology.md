# Multi-site deployment — roles, star of brokers, edge spooling

> **Status: ⏳ planned.** Written 2026-10-04. Turns the "Mosquitto bridge per
> site" item flagged in [site-room-topology.md](site-room-topology.md) from
> "only if the VPN flaps" into the recommended default for every remote site,
> and defines the deployment roles `deploy/deploy.sh` should offer. Nothing
> here is built yet: today `deploy.sh` installs the whole stack, including its
> own broker, on one Pi.
>
> **Extended 2026-10-05** for the first real deployment. That host is `srv01`
> in the `odin` network: server and gateway on one machine, publishing to a
> broker homescope does not own. The host's own configuration lives in the
> separate `asgard` infrastructure repo. The extension adds the deploy-script
> mechanics, data placement, restore, backups and site/room ideas, and
> corrects three details of the bridge sketch (⚠️ notes beside it). Sections
> marked **proposed** await the owner's decision. Everything else stands as
> written on 2026-10-04.

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

⚠️ *Corrections, 2026-10-05:*

- **The loopback listener does not fit separate containers.** Gateway and
  broker are separate containers on `homescope.network`, as today's mosquitto
  and gateway are. Loopback inside the broker's container is unreachable from
  the gateway's, so "publishes to `localhost`" (step 1 above) does not hold.
  Either keep today's layout, with `listener 1883` on all of the *container's*
  interfaces, no `PublishPort`, and the gateway connecting to `homescope-mqtt`,
  which is still never the LAN. Or put both containers in one Quadlet `.pod`,
  where they share a namespace and loopback is right. The first option changes
  nothing that already works.
- **Mosquitto has no file option for `remote_password`.** It must appear in
  the config text. Render the whole `connection` stanza at deploy time and
  deliver it as a podman secret mounted into an `include_dir`
  (`uid=1883,gid=1883,mode=0400`, so podman does the user-namespace ownership
  mapping). The password then never lands in git or in a world-readable file.
  An included file is read after the main config, so `max_queued_messages`
  still precedes the `connection` block.
- **Names on a shared central broker.** There, every user is named after its
  service (asgard's broker has `z2m`, `ha`, `srv01-monitor`), so
  `homescope-<site>` replaces `site-<site>`, and `homescope-<site>-bridge`
  replaces `bridge-<site>`. Inside the broker's namespace, `site-odin` does not
  say whose user it is. The same goes for the client ids of everything homescope
  connects; see [Naming](#naming-units-and-client-ids--proposed).
- **Add `notification_topic homescope/<site>/bridge/state`.** The default
  notification topic `$SYS/broker/connection/<id>/state` lies outside the ACL
  and is dropped silently. Under the site prefix, the central side gets a
  retained `1`/`0` meaning "bridge up", which Home Assistant can alarm on. The
  API's `…/sensors/+/envelope` subscription never matches it. The
  no-retain rule below is about envelopes, so this does not break it.

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
  (`site-<site>`, ⚠️ `homescope-<site>` since 2026-10-05), not one per gateway.
  Several receivers at one site share it.
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

⚠️ *2026-10-05:* the first real host (srv01) is exactly that last case:
`server` plus a direct gateway. The proposal below keeps the three roles as the
*vocabulary*, with one example config per role, and makes the *mechanism* a
list of components. That way "both on one host" is just a longer list. See
[The deploy script](#the-deploy-script-components-and-a-per-host-config).

## Integrations

Unchanged from site-room-topology.md: integrations (e.g. Home Assistant MQTT
discovery) get plaintext readings from an **API republish after decrypt,
verify and dedup**, never from the gateways. With a shared central broker that
republish lands next to the integration's own topics — give the API a user
whose ACL allows writing exactly those topics and nothing else.

## The deploy script: components and a per-host config

*Added 2026-10-05.* ✅ **The approach was accepted by the owner on 2026-10-05.**
The script itself is deferred, and the details below are the starting point
for it. The single `deploy.sh` reads a host config file,
`/etc/homescope/deploy.conf`, which declares *what this host is*: its
components, plus the values listed under [Roles](#roles).

The owner's refinements:

- **Everything configurable lives in the file.** That includes the external
  broker's address, Grafana's exposure, the data location and the site. Today
  that list is short. Anything that becomes configurable later goes into the
  file too, never into a flag.
- **Credentials never live in the file.** For an external broker the file
  carries the address (and username), and the password is required separately,
  as a podman secret. Deploy fails closed when the file names an external broker
  and the secret is missing.
- **Three components: `broker`, `api` and `gateway`.** `api` brings its
  TimescaleDB and its Grafana with it. A database without the API stays empty,
  because the API is the only writer. Grafana reads that database directly, and
  the database is not exposed. Folding them together means invalid combinations
  cannot even be written, instead of being errors deploy has to report. A remote
  database would be a new `DB_HOST` key, not a component. ✅ *Accepted:* one
  off-switch inside `api`, `GRAFANA=none`, for a host that runs its own Grafana
  (asgard plans one for all of odin). That Grafana then reads the database
  through its loopback port, as the existing read-only `grafana` role.
- **Every broker may be external.** `api` and `gateway` each require a broker:
  either the `broker` component, or an external address in the file plus its
  credentials secret. Homescope does not need to manage any broker, but **ACLs
  are strongly advised** on an external one.
- **Recommended topology:** one central broker for the API, plus a broker per
  remote site that bridges to it and spools across link outages. At the central
  site the central broker is already local, so its gateway publishes to it
  directly. A single broker with every site's gateway publishing to it straight
  over the VPN is a valid setup, but not recommended: a link outage then loses
  readings for good (see [Why spool at the edge](#why-spool-at-the-edge-and-why-by-default)).
  When homescope's own `broker` *is* the central one, it must accept remote
  bridges. That takes a published listener, plus a user and ACL lines for each
  site, generated from the file.

```sh
# srv01 (odin) — the server role plus a direct gateway, external broker
COMPONENTS="api gateway"
SITE=odin
DATA_DIR=/srv/homescope
MQTT_HOST=host.containers.internal     # not the host's own IP, see below
MQTT_API_USER=homescope-api
MQTT_GATEWAY_USER=homescope-odin
GRAFANA_PUBLISH=127.0.0.1:4000
GRAFANA_ROOT_URL=https://grafana.odin.mari-code.pl/
GRAFANA_ALLOW_EMBEDDING=true
BACKUP_DIR=/srv/homescope/backups
BACKUP_ON_CALENDAR="*-*-* 03:00"
```

```sh
# gateway role — remote site with a bridging broker
COMPONENTS="broker gateway"
SITE=thor
BRIDGE_ADDRESS=10.49.30.10:1883
BRIDGE_USER=homescope-thor
```

```sh
# all-in-one
COMPONENTS="broker api gateway"
SITE=home
```

Reruns stay `git pull && sudo ./deploy/deploy.sh`: the host remembers its own
shape, so the command never changes. `sudo ./deploy/deploy.sh init <role>`
copies `deploy/examples/<role>.conf` into place to start a new host. The
examples are where the role names live on.

**One rule separates the two inputs: the config file says what the host *is*,
and flags only request one-off actions.** Examples are `--import-kek FILE` (see
[Restoring onto a fresh host](#restoring-onto-a-fresh-host--proposed)) and
`--check`, which prints what would change, as asgard's `odin/deploy.sh` does. A
flag never changes the host's shape.

Where the file lives is the operator's choice. On asgard hosts the asgard repo
carries it: site names, broker addresses and `/srv` paths are asgard facts, and
homescope stays generic.

### Alternatives rejected

- **Flags only** (`deploy.sh --with gateway,broker --site thor …`). In a
  converge script the invocation *is* the state. Every rerun must repeat it
  exactly, and a forgotten flag silently removes a component or renames a
  site. The real configuration ends up in shell history.
- **One script per service.** The services share state across the boundaries:
  the DB-password trio (db/api/grafana), the network, the user, linger and
  staging. Either that is duplicated N times, or it moves into a sourced lib,
  which is the recommendation again with N entrypoints. Nothing records which
  scripts a host has had run, so a `git pull` rerun means remembering the list,
  and removing a service has no script at all.
- **Roles as the mechanism** (`deploy.sh server`). Combinations such as server
  plus gateway either multiply the roles or grow flags, which is the first
  rejected option again.
- **Templating the quadlets** (`envsubst` over the repo files). `%h`, `$` and
  `${…}` already appear in them, and substitution makes the repo files
  unrunnable as written. Drop-ins (below) keep them valid.

### Mechanics worth getting right

- **Parse the config strictly; never `source` it.** Accept `KEY=value` lines and
  a known key list, and `die` on an unknown key. A typo like `COMPONENT=` that
  is silently ignored is exactly the drift a converge script exists to prevent.
- **Validate combinations up front** (the rules above). `api` and `gateway`
  require a broker: either the `broker` component, or `MQTT_HOST` plus its
  credentials secret. `BRIDGE_*` requires `broker`. `gateway` requires `SITE`.
- **Host values reach containers two ways, chosen by who reads them.** What
  the *application* reads (`MQTT_HOST`, `SITE`, usernames) goes into the
  generated per-service env files that already exist under
  `~/.config/homescope/`. What *podman* reads (bind-mount paths, `PublishPort`)
  goes into a generated quadlet drop-in, `<unit>.container.d/50-host.conf`,
  next to the unchanged repo quadlet. Check drop-in support on the oldest
  target with `man podman-systemd.unit`; srv01 runs Podman 5.4. Then raise
  `MIN_PODMAN_VERSION` to match.
- **Deselecting a component**: stop and disable its unit *before* its quadlet
  disappears. Otherwise the running container outlives its unit file. Data is
  never deleted.
- **Internal layout**: one entrypoint, with per-component code in
  `deploy/components/<name>.sh`, each defining a root-phase and a user-phase
  hook. The user phase sources those files from the staged tree instead of
  relying on `export -f`. That gives the readability of separate scripts
  without having to run them separately.

## Naming: units and client ids — proposed

*Added 2026-10-05.* This is the free moment to rename, because no deployment
exists right now (thor is archived and srv01 is fresh). Make quadlet file =
unit = container name:

| Today | Proposed |
|---|---|
| `timescaledb.service` | `homescope-db.service` |
| `api.service` | `homescope-api.service` |
| `gateway.service` | `homescope-gateway.service` |
| `grafana.service` | `homescope-grafana.service` |
| `mosquitto.service` | `homescope-mqtt.service` |

On a host that also runs a system-level `mosquitto.service` (srv01 does), two
units with the same name in different managers is a support trap. Quadlet's
dash-truncation rule also gives a shared `homescope-.container.d/` drop-in
directory that applies to every container for free.

**MQTT client ids, same reasoning.** The gateway connects as `gateway` and the
API as `api`. On a shared broker those are collision-prone: a second client
with the same id kicks the first, and they reconnect in a loop. Use
`homescope-api`, `homescope-gateway-<site>` and `homescope-<site>-bridge`.
⚠️ Renaming the API's id orphans its durable session. That is free *now*,
because the new broker has no session yet. Afterwards it is not.

## Data on a separate disk — proposed

*Added 2026-10-05.* On srv01, data lives on `/srv`, an HDD mounted with
`nofail`, and the SD card holds only the system. Postgres must not live in a
podman named volume under `/var/lib/homescope` (SD).

- **Bind mounts under `DATA_DIR`**: `$DATA_DIR/timescaledb` →
  `/var/lib/postgresql` (the pg18 layout, as today), and `$DATA_DIR/grafana` →
  `/var/lib/grafana`. Today Grafana has no volume at all, so its SQLite writes
  go to the container layer on the SD card and are lost on every restart. The
  default `DATA_DIR` is `/var/lib/homescope/data`, so all-in-one hosts need no
  setting.
- **Ownership in the user namespace.** Root creates both directories owned by
  `homescope`, which is root *inside* the container. Postgres then needs nothing
  more: the official entrypoint starts as container root and chowns `PGDATA` to
  its own (sub)uid. Grafana starts directly as uid 472 and chowns nothing, so
  it needs `podman unshare chown 472:0 $DATA_DIR/grafana` as `homescope`. Never
  chown these directories as host root afterwards, for the same reason as the
  `~/.local/share/containers` rule in `deploy.sh`.
- **The mount dependency goes on the user manager, not on the quadlets.** A
  rootless unit runs in the `homescope` *user* manager, which does not load
  fstab mount units. A `RequiresMountsFor=/srv` written inside a quadlet would
  therefore fail as "unit not found" at boot, or work by accident, depending on
  timing. A drop-in on the *system* unit does the job properly:

  ```ini
  # /etc/systemd/system/user@<uid>.service.d/homescope-data.conf
  [Unit]
  RequiresMountsFor=/srv/homescope
  ```

  If the disk is missing, the whole homescope manager does not start, and it
  shows up in `systemctl --failed` at system level, where host monitoring
  already looks. Unmounting stops the stack cleanly before the mount goes. On a
  host where `DATA_DIR` is on the root filesystem the drop-in is harmless, so
  it can be generated unconditionally. ⚠️ Verify this on srv01 by rebooting with
  the disk unplugged.

## Reaching a broker on the same host from rootless podman

*Added 2026-10-05.* ⚠️ **Not by the host's own IP.** Rootless networking is
pasta, and pasta *copies the host's addresses into the namespace*
(`man podman-run`, `--network pasta`). From inside a homescope container,
`10.49.30.10` is the namespace's own address, so `10.49.30.10:1883` connects to
nothing. Podman maps `host.containers.internal` (`169.254.1.2`, pasta's
`--map-guest-addr`) to the host, so on srv01 `MQTT_HOST=host.containers.internal`.
Verify before wiring the services:

```bash
hpodman run --rm --network systemd-homescope docker.io/library/eclipse-mosquitto:2.0.22 \
    mosquitto_sub -h host.containers.internal -u homescope-api -P '…' \
    -t 'homescope/#' -C 1 -W 5 -v
```

The fallback is `Network=host` for api and gateway (rootless supports it). It
costs the container DNS (`homescope-db`), so the API would reach Postgres on
`127.0.0.1:5432` instead.

## MQTT authentication: what the code and the deploy need

*Added 2026-10-05; complements [mosquitto-acl.md](mosquitto-acl.md).* Code
changes, which the owner writes:

- **Credentials as a file, not env.** Add `MQTT_USERNAME` and
  `MQTT_PASSWORD_PATH` to both `GatewayConfig` and `ApiConfig`, read and
  trimmed the same way as `ADMIN_TOKEN_PATH` (`api/src/http/auth.rs`), then
  passed to `MqttOptions::set_credentials`. Model them as
  `Option<MqttCredentials>`: both variables set, or neither (the dev compose
  broker stays anonymous). Exactly one set is a startup error. The password is
  never in `Debug` output.
- **Client ids** as under [Naming](#naming-units-and-client-ids--proposed)
  (`MQTT_CLIENT_ID`, with those defaults).
- **Check the SUBACK.** The broker does not log ACL denials, and asgard has
  verified that. The API logs "Subscribed to sensors" once the *request* is
  queued, so a denied subscription looks healthy forever. rumqttc surfaces
  `Packet::SubAck` with per-topic return codes, and a failure code there should
  be an error.
- **Optional: MQTT v5** (`rumqttc::v5`). Under 3.1.1 a QoS 1 publish that the
  ACL denies is still PUBACKed, so the gateway cannot see the denial. Under v5
  the PUBACK carries "Not authorized". That makes ACL mistakes visible in the
  gateway's own log, but it is a larger change.

Deploy side:

- **External broker**: the broker's owner creates the users. The passwords
  reach homescope as podman secrets (`homescope-mqtt-api`,
  `homescope-mqtt-gateway`), mounted as files the same way as the KEK. Deploy
  checks that they exist and fails closed with the exact `podman secret create`
  command.
- **Local broker** (all-in-one, gateway role): deploy generates the local
  passwords into the same secrets and writes the hashed `passwd` file through a
  throwaway `mosquitto_passwd` container. Files mosquitto must read (`passwd`,
  `acl`, the bridge stanza) are owned by uid 1883 *inside* the container. A
  podman secret mount with `uid=1883,gid=1883,mode=0400` lets podman do that
  mapping instead of a `podman unshare chown`.

### Topics and ACL on the central broker

```text
user homescope-api
topic read homescope/+/sensors/+/envelope

user homescope-odin            # a site's publisher: the gateway at odin, the bridge elsewhere
topic write homescope/odin/#
```

Later: `homescope-thor` and `homescope-freya` (bridges) get `write
homescope/<site>/#`. When the API republishes, it also needs
`write homescope/+/sensors/+/state` and `write homeassistant/+/homescope/#`.
Every other discovery publisher then needs a `deny homeassistant/+/homescope/#`.

**Topic change = expand/contract.** A broker does not queue messages on a topic
no one subscribes to, not even for a durable session. So the API must subscribe
to both `homescope/sensors/+/envelope` and `homescope/+/sensors/+/envelope` for
one release *before* any gateway switches. API and gateway images
auto-update independently, so "deploy them together" is not atomic. If a central
site goes live before the prefix exists, its ACL needs the old topic
temporarily as well (`read homescope/sensors/+/envelope` for the API, `write
homescope/sensors/#` for the site user).

**Queue depth for the API's durable session.** This is the central-side
counterpart of the bridge's queue sizing above. Mosquitto's default
`max_queued_messages` is 1000 per client. At one reading per sensor per minute
that is about 16 h of API downtime for one sensor, and about 1.6 h for ten.
Homescope's own `mosquitto.conf` sets 10000. A shared broker needs its owner to
raise it. The setting is global, which is harmless to clean-session clients.

## Restoring onto a fresh host — proposed

*Added 2026-10-05, for srv01 from the archive of thor's old Pi.* ⚠️ **The KEK
must exist before the first deploy, or deploy mints a new one.** A restored
dump wrapped under thor's KEK, opened with a fresh generation 1, fails on every
row. The API skips every device with a key fault. That is recoverable by
removing the secret, importing the right one and restarting the api, but only
if no device was provisioned in between. Make it impossible:

- **First run without a KEK secret: fail closed.** Proceed only with
  `--new-kek` (fresh install) or `--import-kek FILE` (restore). That costs one
  extra word on a brand-new install. It follows the fail-closed pattern of
  `homescope-provision`'s confirmations.
- Admin token: import it too (`--import-admin-token FILE`) so the workstation's
  provision profile keeps working, or let deploy generate one and
  `homescope-provision login` again. It derives nothing.

Sequence:

1. The broker owner creates the MQTT users and ACL. The reverse-proxy entries
   go in.
2. Images with MQTT credentials and client ids are built.
3. Write `/etc/homescope/deploy.conf`. Create the MQTT password secrets.
4. `sudo ./deploy/deploy.sh --import-kek <archive>/kek --import-admin-token <archive>/admin-token`.
   The fresh cluster's init script creates `api`/`grafana` with *new* passwords.
   Do not restore the archive's `globals-*.sql`.
5. Restore the dump with the procedure in the `backup-db.sh` header (stop api,
   drop and recreate the DB, `timescaledb_pre_restore`, `pg_restore`,
   `post_restore`, start api). ⚠️ The dump's TimescaleDB version must match the
   image. Check the tag in the archived `timescaledb.container` first.
6. Pending migrations run on API start (`RUN_MIGRATIONS=true`).
   `homescope-provision verify` against the restored sensor confirms the whole
   chain: receiver → gateway → broker ACL → API → key under the imported KEK.

## Backups — proposed

*Added 2026-10-05.*

- **Nightly dump to a stable name.** `backup-db.sh --scheduled` writes
  `$BACKUP_DIR/homescope.dump` and `globals.sql`, through the existing
  `.part`-then-rename path, overwriting yesterday's. The host's file backup
  (asgard: restic of `/srv` at 03:30) supplies the history. Timestamped files
  would only pile up on `/srv`. The on-demand mode keeps today's timestamped
  behaviour for "before a migration" dumps.
- **`pg_dump -Fc -Z0`** for the scheduled dump. A gzip stream changes from the
  first differing byte onward, so it defeats restic's content-defined dedup.
  An uncompressed dump of an append-mostly hypertable dedups nearly completely,
  and restic compresses on its own.
- **A system timer** `homescope-backup-db.timer` (03:00 on srv01). It is a
  system unit, so a failure is visible in `systemctl --failed`. Its `.service`
  runs the script as root from a **root-owned copy**
  (`/usr/local/sbin/homescope-backup-db`, installed in the root phase).
  ⚠️ It must never run the copy in `deploy-src/`: `homescope` owns that tree,
  so root executing it would let anything running as `homescope` (any
  container escape included) write code that root runs nightly. Atomic rename
  makes a collision with the file backup harmless: the backup sees yesterday's
  file or today's, never a torn one.
- **Exclude the live database** (`$DATA_DIR/timescaledb`) from file-level
  backups. Include `$DATA_DIR/grafana`. It is low-value, since everything there
  is provisioned from git, and a rare torn SQLite copy costs nothing.
- **The KEK stays out of all of it.** It lives in podman secret storage under
  `/var/lib/homescope` on the SD card, not under `DATA_DIR`, and its backup is
  the password manager. ⚠️ The thor archive holds the dump *and* the KEK
  together, inside the nightly restic set. Restic's encryption protects that
  against loss of the backup stick alone, but it breaks "dump and KEK stored
  apart" for as long as those snapshots are retained. Once the restore is
  verified, delete the KEK from the archive.

## Grafana

*Added 2026-10-05.*

- Behind a reverse proxy it needs `GF_SERVER_ROOT_URL`. To be embedded (HA
  panels) it needs `GF_SECURITY_ALLOW_EMBEDDING=true`, or Grafana sends
  `X-Frame-Options: deny`. These are host values, so they belong in
  `deploy.conf`. The default stays as today: published on the LAN, anonymous
  viewer.
- **Who owns the Grafana on a shared host.** asgard plans one Grafana for all
  of odin, with Prometheus as a second source later. Once a non-homescope
  source exists, that Grafana is infrastructure, and homescope should ship only
  its datasource and dashboards for the host's Grafana to load. Until then,
  homescope's Grafana is the cheapest correct answer. ⚠️ *2026-10-05:* Grafana
  is now part of the `api` component, so the handover is the accepted
  `GRAFANA=none` switch (see [The deploy script](#the-deploy-script-components-and-a-per-host-config)).

## What a host must provide

*Added 2026-10-05.* A checklist for the operator, or for the infra repo that
owns the host:

| Need | Why |
|---|---|
| Podman (version: see the drop-in note above), systemd, `loginctl enable-linger` | rootless quadlets |
| subuid/subgid ranges for `homescope` | rootless podman |
| A broker reachable from the containers, with the users and ACL lines above | central or external broker |
| `DATA_DIR` on durable storage, excluded from file backups except `backups/` and `grafana/` | data, backups |
| Free loopback ports 4000 / 4001 / 5432 (or overrides) | Grafana, API, Postgres |
| NTP | gateway timestamps |
| The KEK backed up apart from the DB dumps | provisioning.md §4 |
| Monitoring that also checks the *user* manager: `systemctl --user -M homescope@ --failed` | system-level `--failed` cannot see rootless units |

## Site and room: ideas for the owner — proposed

> ✅ **Resolved 2026-10-05; the decisions live in
> [site-room-topology.md](site-room-topology.md#revised-2026-10-05-sites-rooms-placement-history).**
> Ideas 2–4, 6 and 9–11 were taken as written. Two ideas were superseded:
> idea 5 (`devices.site`) and idea 7 (site NOT NULL), replaced by `rooms` plus
> a `device_placements` history, from which a device's site is derived. Idea 8
> became the move/correction operations on placements. Idea 1 is moot: both
> halves now proceed, with the schema first. The list below is kept as
> proposed.

*Added 2026-10-05, not decisions.* [site-room-topology.md](site-room-topology.md)
settles the shape: gateway `SITE` → topic prefix, `devices.site`/`room` as
semantics. Accepted ideas move there.

1. **Split the one-PR unit in two.** (a) *Transport*: gateway `SITE`, topic
   prefix, the API's wildcard subscription. It is small, it is the part every
   ACL line and the bridge topic depend on, and the central site benefits
   immediately. (b) *Semantics*: the `devices` columns and the provision flags.
   This is deeper, and nothing in the deployment depends on it. ⚠️ This changes
   step 1 of the order of work below, which keeps them together.
2. **`SITE` is required, with no default, and is one topic level**: non-empty,
   no `/`, `+` or `#`, ideally `[a-z0-9-]+`. A gateway publishing under the
   wrong site is exactly what an ACL drops *silently*, so fail at startup. This
   fits a `Site` newtype with `FromStr` in `common`: parse, don't validate, the
   way `DeviceAddr` is parsed.
3. **The topic is the authenticated provenance, the payload is not.** Under
   per-user ACLs the broker guarantees that whatever arrives on
   `homescope/thor/…` came from a client allowed to write there. A `site` field
   inside `ObservationEnvelope` would only be the publisher's own claim. So keep
   site *out* of the envelope, and have the API parse the topic into a typed
   `(Site, DeviceAddr)`. Cross-check that `DeviceAddr` against the envelope's,
   rejecting and warning on a mismatch. AEAD already binds the address to the
   key, so this is diagnostics plus defence in depth. It is also a nicely
   testable pure function.
4. **Provenance storage: a span field now, a column later.** Put `site` on
   `handle_envelope`'s span next to `device_addr`. A `readings` column only makes
   sense once the per-device seq check decides *which* receiver's copy is kept.
   Before that, "heard at" on the stored row means "whichever arrived first".
   Per-gateway RSSI for coverage maps would be its own table, later.
5. **`devices.site` as an FK to a small `sites` table** (`id`, unique `name`,
   perhaps `display_name`). Typos like `Thor` vs `thor` become impossible, a
   rename is one row, and comparing topic-site with device-site is comparing
   ids. Sites are created explicitly, not auto-created from topics, for the same
   reason there is no auto-registration of devices. No seeded names: those are
   the owner's, and the project is meant to be portable.
6. **`room` stays free text and nullable.** On asgard, match the Home Assistant
   area ids (the building-plan room numbers), so a future discovery republish
   can set `suggested_area` and sensors land in the right area by themselves.
7. **Site NOT NULL, through expand/migrate/contract.** A sensor is always meant
   for *some* house. The backfill can be plain SQL this time, because no crypto
   is involved, unlike `devices.key`.
8. **Moving a sensor is metadata, not provisioning.** `PATCH /devices/{addr}`
   for site/room, with no re-key, and `homescope-provision move`. New
   `DeviceSummary` fields need golden tests. Older provision binaries already
   tolerate unknown fields, which is the existing "never `deny_unknown_fields`"
   rule.
9. **Topic-site ≠ device-site is an `info`, not a `warn`.** It is expected near
   a house boundary, and it is the reason the two concepts are separate.
10. **The republish uses the device's site, not the provenance site.**
    `homescope/<device-site>/sensors/<addr>/state`: a consumer wants "the thor
    kitchen sensor", not "whatever gateway heard it". HA discovery goes under
    node id `homescope`, with `unique_id` `homescope_<addr>_<metric>`.
11. **Grafana**: `site` and `room` as dashboard variables drawn from these
    tables.

## Order of work

1. Site prefix + `devices.site`/`room` (site-room-topology.md) — the bridge
   topic and every ACL line depend on it.
2. Broker auth and ACLs (mosquitto-acl.md), including the `site-<site>` users.
3. Deployment roles in `deploy.sh`, external-broker support first: a central
   site that already runs a broker is the first real consumer.
4. The `gateway` role with the bridging local broker, then the first remote
   site.
5. NTP verified on every gateway before the second gateway goes live.

⚠️ *Update, later on 2026-10-05:* the owner is proceeding with step 1 now,
schema first (sites, rooms, placements), with the topic prefix as an
independent second half. The note below is kept as it was written.

⚠️ *2026-10-05, as it stands for srv01.* Step 2's code half (credentials from a
file, client ids, the SUBACK check) is the hard blocker. asgard's broker has
`allow_anonymous false`, so nothing homescope runs today can connect to it at
all. The owner has deferred step 1 for now. That leaves two orders, the owner's
call:

- **Keep step 1 first.** srv01 waits for it.
- **Split step 1** (site idea 1). The transport half goes in with step 2, and
  the semantic half follows srv01's go-live. If srv01 goes live before even the
  transport half, its ACL carries the temporary old-topic lines from
  [Topics and ACL](#topics-and-acl-on-the-central-broker).

Step 3 then means the components-and-config proposal above, plus the restore
for srv01, and the users are `homescope-<site>`.

## Concepts this exercise touches

- Store-and-forward at the edge vs reliability at the core; where in a
  pipeline durability must start (at the first hop that can lose data)
- MQTT bridges: direction, QoS across hops, persistent sessions keyed by
  client id, queue limits and what happens when they fill
- At-least-once delivery + idempotent consumer (the `seq` check) as the
  standard alternative to exactly-once
- Topology choice: why a star beats a mesh when data flows one way
- Converge scripts: declared state in a file vs state in the invocation;
  fail-closed first runs
- Rootless containers: user-namespace uid mapping for bind mounts and secret
  files, pasta's copied host address, user vs system systemd managers
- Expand/contract applied to MQTT topics, not just database columns
