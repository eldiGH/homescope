# Operations — running a homescope host

Day-to-day production management: what a host is made of, how to read logs,
update, reach the database and handle secrets. For *why* the stack is shaped
this way see [architecture.md](architecture.md) and
[design/deployment-topology.md](design/deployment-topology.md); for the deploy
mechanics, the header of [`deploy/deploy.sh`](../deploy/deploy.sh).

Two tools do almost everything:

- **`deploy/deploy.sh`** converges the host to `/etc/homescope/deploy.toml`.
- **`homescope`**, which the deploy installs to `/usr/local/bin`, operates it
  afterwards: `sudo homescope help`.

## What a host is: `/etc/homescope/deploy.toml`

The config file declares the host; the deploy makes it so. Rerunning it is
always safe:

```bash
git pull && sudo ./deploy/deploy.sh
```

| Key | Meaning |
| --- | --- |
| `components` | any of `broker`, `api`, `gateway`. `api` brings TimescaleDB and Grafana with it. A local `broker` currently needs the other two beside it (the all-in-one shape). |
| `site` | this host's site (`odin`, `thor`, …): the gateway's `SITE`, the topic level, the broker user `homescope-<site>` |
| `data_dir` | database, Grafana and broker data; default `/var/lib/homescope/data` |
| `[mqtt]` | an external broker: `host`, `port`, `api_user`, `gateway_user` (omit with a local broker) |
| `[api]` | `publish`, `db_publish` — loopback by default |
| `[grafana]` | `enabled`, `publish`, `root_url`, `allow_embedding` |
| `[backup]` | `dir` — where on-demand dumps go (default `<data_dir>/backups`). Scheduling backups is the host's job, see [Backup and restore](#backup-and-restore). |
| `[images]` | `api`, `gateway` — default `ghcr.io/eldigh/homescope-*:latest`; pin a `:<git sha>` to stop following `main` |

The file is validated strictly: an unknown key, a wrong type or a table for a
component the host does not run is an error, never silently ignored. Starting
points for each shape are in [`deploy/examples/`](../deploy/examples/):

```bash
sudo ./deploy/deploy.sh init server    # or all-in-one, gateway
sudo ./deploy/deploy.sh --check        # validate and show the plan; changes nothing
```

**Secrets never go in this file.** They are podman secrets — see
[Secrets](#secrets).

The first deploy of a host must say whether it is a new installation or a
restore, because the KEK must never be minted by accident:

```bash
sudo ./deploy/deploy.sh --new-kek                     # fresh installation
sudo ./deploy/deploy.sh --import-kek kek-file \
    --import-admin-token admin-token-file             # restoring one
```

Without either flag it stops before touching anything that matters.

With an **external broker**, the deploy also needs the passwords of the MQTT
users the broker's owner created. It **asks for any it does not have yet**
(input hidden) and carries on, so a first deploy is one run:

```bash
sudo ./deploy/deploy.sh --import-kek kek-file --import-admin-token admin-token-file
#   homescope needs the password of MQTT user homescope-api on host.containers.internal:1883
#     Password for homescope-api (input hidden, empty to skip for now):
```

Without a terminal, pass them as files: `--mqtt-api-password FILE` and
`--mqtt-gateway-password FILE`. Only with neither does the deploy stop at an
**ACTION NEEDED** banner (exit 0): set them with `sudo homescope secret set …`,
then run the plain deploy again. The one-off flags are not needed a second
time. Repeating a full first-deploy command is still harmless: an identical
`--import-kek` / `--import-admin-token` file is recognised, `--new-kek` keeps a
KEK that exists, and only a *different* KEK is refused.

When the containers are up, the deploy checks that the API, and a running
gateway, **logged in to the broker**. A refused login is reported with the
fix, not left to be discovered as missing readings.

## Machine layout

| What | Where |
| --- | --- |
| Host config | `/etc/homescope/deploy.toml` |
| Service user | `homescope` (lingering, home `/var/lib/homescope`) |
| Data | `<data_dir>/timescaledb`, `grafana`, `mosquitto`; backups in `<data_dir>/backups` |
| Quadlets | `/var/lib/homescope/.config/containers/systemd/` — each `homescope-*.container` plus a generated `<unit>.container.d/50-host.conf` |
| Generated env files, DB passwords | `/var/lib/homescope/.config/homescope/` |
| Staged deploy tree | `/var/lib/homescope/deploy-src/` (what the last deploy shipped) |
| Admin command | `/usr/local/bin/homescope`, helpers in `/usr/local/lib/homescope/` |
| Dump tool | `/usr/local/sbin/homescope-backup-db` (root-owned copy), behind `homescope backup` |
| Data-disk dependency | `/etc/systemd/system/user@<uid>.service.d/homescope-data.conf` |
| Receiver dongle | `/dev/homescope-receiver` (udev symlink, gateway hosts only) |

Containers, all on the `systemd-homescope` podman network, each a user unit of
the same name:

| Unit / container | Component | Host port (default) |
| --- | --- | --- |
| `homescope-db` | api | `127.0.0.1:5432` |
| `homescope-api` | api | `127.0.0.1:4001` |
| `homescope-grafana` | api, unless `grafana.enabled = false` | `4000` (all interfaces) — `127.0.0.1:4000` behind a proxy |
| `homescope-mqtt` | broker | — (podman network only) |
| `homescope-gateway` | gateway | — |

The API and Postgres stay on loopback: the API's admin token is a bearer token,
and Postgres authenticates with a password in cleartext. Reach them over an SSH
tunnel or through a TLS-terminating reverse proxy, never directly across the
LAN.

The generated drop-in holds everything host-specific podman reads — the data
bind mounts, the published ports, the image. The deploy refuses to start a
container whose drop-in did not apply: without it the database would write
into its container layer instead of `data_dir`.

## The `homescope` command

```bash
sudo homescope status                  # units and containers
sudo homescope logs api -f             # api, db, grafana, gateway, mqtt
sudo homescope restart gateway         # start | stop | restart, or `all`
sudo homescope psql                    # psql as postgres, in the database
sudo homescope podman ps               # podman as the homescope user
sudo homescope secret list             # set | show | rm <name>
sudo homescope backup                  # on-demand dump; --snapshot DIR for the host's backup job
sudo homescope restore <dump>          # replace the database with a pg_dump archive
sudo homescope config                  # the effective deploy.toml
```

It hides what rootless podman otherwise needs by hand: running as the
`homescope` user with its `XDG_RUNTIME_DIR`, from a directory that user can
enter. For raw commands, that gotcha still applies — **rootless podman fails if
your current directory is one `homescope` cannot read**, because it re-execs
inside a user namespace that `chdir()`s back to it:

```
cannot chdir to /home/pi/Projects/homescope/deploy: Permission denied
```

`cd /` first, or open a full session with `sudo machinectl shell homescope@.host`.

## Logs

`homescope logs <component>` shows what the container printed together with
systemd's own start/stop/restart records for its unit — a crash loop shows up
only in the latter. Extra arguments go to `journalctl`:

```bash
sudo homescope logs api -f
sudo homescope logs gateway -n 200
sudo homescope logs api --since '30 min ago' -p warning
```

## Service control

```bash
sudo homescope status
sudo homescope restart api
sudo homescope podman inspect homescope-api --format '{{.ImageName}}  created {{.Created}}'
```

Quadlets are generated units: you cannot `systemctl edit` them. Change the repo
or `deploy.toml` and rerun the deploy, which regenerates and reloads them.

**Without its data disk nothing runs.** When `data_dir` is on a separate disk
(srv01: `/srv`, mounted `nofail`), the homescope user manager requires that
mount. A missing disk at boot leaves the whole stack stopped instead of
starting an empty database on the SD card, and `homescope status` says so.
⚠️ It does **not** appear in `systemctl --failed` — a dependency failure leaves
the unit inactive, not failed. Monitoring has to ask directly:

```bash
systemctl is-active user@$(id -u homescope).service      # expect: active
```

## Updating

Images: push to `main` → GitHub Actions builds the ARM image → ghcr.io → the
host's auto-update timer pulls it within 5 minutes and restarts the container.
A host with `[images]` pinned to a `:<sha>` tag does not follow `main`; change
the pin and rerun the deploy.

```bash
sudo homescope podman auto-update --dry-run      # what would change
```

`podman auto-update` rolls back a container whose new image fails to start —
but "starts and then exits" is a successful start, so a binary that crashes on a
bad migration will loop, not roll back.

Image builds only fire on paths the workflows watch
(`.github/workflows/build-{api,gateway}.yml`). **A red build means the host
silently keeps running the last green image** — check before assuming an update
landed:

```bash
gh run list --workflow build-api.yml --limit 5                  # from the workstation
sudo homescope podman inspect homescope-api --format '{{.ImageName}}  created {{.Created}}'
```

Config, quadlet or script changes need a converge — image pulls alone do not
carry them. The deploy never overwrites a secret, stops and removes the units of
components no longer listed, and leaves their data in place.

## Secrets

| Secret (`homescope secret …`) | What | Set by |
| --- | --- | --- |
| `kek` | wraps every device key in `devices.key` | `--new-kek` or `--import-kek` on the first deploy |
| `admin-token` | bearer token for `/devices` | generated, or `--import-admin-token` |
| `mqtt-api`, `mqtt-gateway` | the API's and gateway's broker passwords | generated with a local broker; **the operator** with an external one |
| `mqtt-passwd`, `mqtt-acl` | the local broker's password file and ACL | generated from the above and `deploy.toml` |

All are podman secrets, reaching the containers as files under `/run/secrets/`
— never environment variables, so never in `/proc/<pid>/environ` or `podman
inspect`. The database passwords are the exception: generated once into
`db.env`, `api.env` and `grafana.env` (mode 600) under
`/var/lib/homescope/.config/homescope/`, along with Grafana's admin password.

```bash
sudo homescope secret list
sudo homescope secret show admin-token                  # for homescope-provision login
sudo homescope secret set mqtt-api                      # prompts; or < file
```

With an external broker the deploy asks for missing MQTT passwords, or takes
`--mqtt-*-password FILE` (see [above](#what-a-host-is-etchomescopedeploytoml)). A password generated earlier for a local
broker does not count — it would never log in elsewhere. During a first deploy,
rerun the deploy after `secret set`; on a running host, `homescope restart api`
applies a new `mqtt-api`. With a local broker, always rerun the deploy, which
rebuilds the broker's password file.

⚠️ **The KEK is not in the database backups, on purpose.** A dump plus the KEK
is the whole fleet; a dump alone is inert — only while the two are stored apart.
Back the KEK up somewhere other than wherever the dumps go:

```bash
sudo homescope secret show kek > kek-backup         # byte-exact; store it off this machine
```

Losing it means re-provisioning every sensor by hand. Replacing it is refused
outright (`secret set kek` on a host that has one), because it orphans every
device key.

The admin token is *not* worth backing up. Revoking it is
`sudo homescope secret set admin-token` (any 32+ characters) followed by
`sudo homescope restart api`.

## Database

### From the host

```bash
sudo homescope psql                                     # always works
sudo homescope psql -c 'SELECT count(*) FROM readings'
psql -h 127.0.0.1 -U api -d homescope                   # via the published port
```

Bare `psql` without `-h` tries the Unix socket inside the container and fails
with "No such file or directory". Use `127.0.0.1`, never `localhost`: the port is
published on IPv4 loopback only, and `localhost` usually resolves to `::1`
first, which looks like `Connection refused`. Roles are `api` (owns the schema),
`grafana` (read-only) and `postgres` (superuser); passwords are in `db.env`.

Useful one-liners:

```sql
\dt                                    -- tables
SELECT * FROM _sqlx_migrations ORDER BY version;   -- what has actually applied
SELECT lpad(to_hex(device_addr), 12, '0') AS addr, name,
       key IS NOT NULL AS has_key, key_valid_from FROM devices ORDER BY name;
SELECT count(*), min(time), max(time) FROM readings;
SELECT d.name, max(r.time) FROM devices d
  LEFT JOIN readings r USING (device_addr) GROUP BY d.name;
```

`to_hex(device_addr)` renders the same 12-hex string as the MQTT topic and the
provisioning CLI. It is always exactly 12 characters: the top two bits of an
AdvA are forced to 1 (static-random marking), so the leading byte is never below
`0xC0`. The column is a BIGINT because a 48-bit address always fits one,
positively.

Going the other way — writing an address you have as hex — needs care:

```sql
-- correct
SELECT x'cea99627bd3f'::bigint;                                   -- 227227763981631
SELECT ('x' || lpad('CEA99627BD3F', 16, '0'))::bit(64)::bigint;   -- same
-- WRONG: 'x…'::bit(64) pads on the RIGHT, so the address lands in the high bits
SELECT ('x' || 'CEA99627BD3F')::bit(64)::bigint;                  -- -3555145333409382400
```

A valid address in this column is **positive and exactly 12 hex digits** — a
negative value means the MSB is set, which a 48-bit address can never do.

That the displayed hex works as a plain big-endian number is not a coincidence:
`encode_hex` walks the byte array reversed (MSB-first, normal BLE notation)
while `as_i64` is little-endian over the same array, so the two reversals
cancel. `common/src/device_addr.rs` pins it.

### From your workstation (lazysql, DBeaver, psql…)

Postgres is published on the host's **loopback**, so the way in is an SSH
tunnel:

```bash
ssh -fN -o ExitOnForwardFailure=yes -L 15432:127.0.0.1:5432 <host>
ss -ltnp | grep 15432     # verify the listener before blaming the database
lazysql 'postgres://api:<API_DB_PASSWORD>@127.0.0.1:15432/homescope?sslmode=disable'
```

The password is `API_DB_PASSWORD` from `db.env` (user `api`), or
`POSTGRES_PASSWORD` for the superuser. `openssl rand -hex 24` generates them, so
they need no URL escaping.

Three things that go wrong here, in the order you'll hit them:

**`Connection refused` on the local port** means the tunnel isn't running — not
that the database is down. `-f` forks after authentication, so the tunnel
outlives the terminal. `ExitOnForwardFailure=yes` is load-bearing: without it,
an ssh that cannot bind the local port prints a warning and **carries on without
the forward**, so a client then reaches whatever else is on that port. The dev
stack in `compose.dev.yml` listens on 5432 of your workstation with the same
tables — forwarding to a busy 5432 without this flag is how you end up reading
dev while believing it's production. Using 15432 locally sidesteps that.

**`SSL is not enabled on the server`** from lazysql, DBeaver or anything else on
Go's `lib/pq`: append `?sslmode=disable`. Their default is `require`, libpq's is
`prefer`. Disabling it is correct here — the bytes are inside the SSH tunnel and
both ends of the Postgres connection are `127.0.0.1`.

**Saved connection strings hold the production password in cleartext**
(`~/.config/lazysql/config.toml`) — `chmod 600`, and name the entry
`homescope-prod` so it is distinguishable from the dev stack at a glance.

For a durable setup, put the forward in `~/.ssh/config`:

```
Host homescope-db
    HostName <host>
    LocalForward 15432 127.0.0.1:5432
    ExitOnForwardFailure yes
```

A published port is needed even on the host itself: a rootless container's IP
is **not routable from the host namespace**. Never widen it beyond loopback.

For a container port that *isn't* published (the local broker, or the API's
3000 inside its namespace), bridge it for as long as the command runs:

```bash
sudo homescope podman run --rm --network systemd-homescope -p 127.0.0.1:1883:1883 \
    docker.io/alpine/socat TCP-LISTEN:1883,fork,reuseaddr TCP:homescope-mqtt:1883
```

### SQL clients

⚠️ **Do not delete or edit `readings` rows through a result grid.** The table
has no primary key (only `UNIQUE (device_addr, seq, time)`), so a client that
offers row-level edits falls back to `ctid` — and on a hypertable the rows live
in chunk tables under `_timescaledb_internal`, where the same `ctid` value exists
in every chunk. A `ctid`-targeted delete routed through the parent can match a
*different* row than the one selected, with no error. Name the row instead:

```sql
DELETE FROM readings
WHERE device_addr = x'cea99627bd3f'::bigint AND seq = 12345 AND time = '2026-09-21 13:21:37.712+00';
```

For bulk removal, drop chunks rather than rows:

```sql
SELECT drop_chunks('readings', older_than => INTERVAL '90 days');
```

`devices` has a primary key (`device_addr`), so grid edits there behave
normally.

### Backup and restore

**Scheduling and keeping backups is the host's job, not homescope's.** The host
already backs up its files on its own schedule. A second schedule inside
homescope could only race it, so homescope schedules nothing. It offers the one
thing a file backup cannot do by itself: a consistent snapshot of a live
database. The host's backup job calls it right before it snapshots files:

```bash
homescope backup --snapshot /srv/.backup-staging/homescope
```

- **`--snapshot DIR`** writes `homescope.dump` and `globals.sql` into `DIR`
  (absolute path), replacing what is there. The host's backup holds the
  history. The dump is uncompressed on purpose, so a deduplicating backup
  (restic) stores only what changed. `DIR` is created `0700` if missing; an
  existing one keeps its permissions. A non-zero exit means no new snapshot,
  and what that means for the rest of the run is the host's call.
- **On demand**, e.g. before a migration: `sudo homescope backup` writes a
  timestamped, compressed pair into `backup.dir` and deletes nothing.
- **What the host's file backup should cover:** the snapshot directory and
  `<data_dir>/grafana`. **Exclude `<data_dir>/timescaledb`**, the live database,
  which a file copy can catch mid-write. The KEK lives in podman's secret store
  under `/var/lib/homescope`, deliberately not under `data_dir`, and is backed
  up separately.

A host without a backup system of its own can still get a nightly snapshot from
a plain systemd timer whose service runs
`/usr/local/bin/homescope backup --snapshot <dir>` — it then owns that timer
like any other host job.

**Restore** uses the dump file as it is:

```bash
sudo homescope restore /srv/archive/thor-rpi5-2026-10-04/<…>.dump
```

It runs as root, so a root-only archive works directly. It refuses a file that
is not a `pg_dump` archive, shows the archive's date and source, and asks you to
type `restore`. Then it:

1. stops the API, the only writer;
2. drops and recreates the database;
3. runs `CREATE EXTENSION IF NOT EXISTS timescaledb` and
   `timescaledb_pre_restore()`;
4. runs `pg_restore`, then `timescaledb_post_restore()`;
5. starts the API, which applies any newer migrations;
6. reports the migrations, the readings, the devices, and whether every device
   key opened.

The `IF NOT EXISTS` in step 3 is the one step that differs from a textbook
restore, and it is not in the dump. It is there because the timescaledb image
installs the extension into `template1`, so a freshly created database already
has it. The pre/post-restore calls make TimescaleDB's own catalog come back as
data, instead of being re-created by its DDL hooks.

The TimescaleDB version must match the dump's, so restore onto the same image
tag. Expect a minute or two for thor's dump on an HDD. About 10 s of that is
stopping the API, which does not handle SIGTERM yet
([api-graceful-shutdown.md](design/api-graceful-shutdown.md)). "Some device keys
did not open" means the imported KEK is not the one the dump was made under;
the database is fine, so import the right KEK and restart the API. The manual
steps are in the header of [`deploy/backup-db.sh`](../deploy/backup-db.sh). To
rehearse on a workstation first: `just db-restore <dump>`, then
`RUN_MIGRATIONS=true just api`.

### Migrations

The API runs pending migrations at startup (`RUN_MIGRATIONS=true`). Each runs in
its own transaction, so a failure leaves the database **untouched** — the API
just exits and systemd restarts it every 5 s. Read the first error of any
restart cycle; the rest is noise.

A migration that fails on a constraint is telling you the *data* is wrong, not
the migration. Fix the rows, then restart the API — it picks up where it
stopped.

Worked example (2026-09-21, from before devices were keyed by `device_addr`):
`20260716201805` renamed `hardware_id` to `device_addr` and asserted it fits 48
bits. It failed because the rows predated the identity refactor — `devices` had
been seeded from the 64-bit FICR `DEVICEID`, and no arithmetic turns a
`DEVICEID` into a `DEVICEADDR`; they are different registers. The fix was to
rewrite each row with the board's real AdvA, or delete it and its readings. Two
lessons that cost time: until a rename migration applies, the column still has
its old name; and a constraint is checked across every row, so one leftover
blocks it exactly as before. Check the next migration's precondition yourself
rather than waiting for the crash loop to tell you.

A device whose key is NULL reports as `MISSING` and its packets are dropped; a
`homescope-provision rotate` against that board mints one.

## API

```bash
curl -s http://127.0.0.1:4001/health

TOKEN=$(sudo homescope secret show admin-token)
curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:4001/devices | jq
```

Routes: `GET /health` (public), and behind the token `POST /devices`,
`GET /devices`, `GET /devices/<addr>`, `POST /devices/<addr>/rotate-key`.

A device whose `keyStatus` is `MISSING` or `UNOPENABLE` is skipped at startup
and **every packet it sends is dropped**. `UNOPENABLE` after a restore means the
wrong KEK was imported.

From the workstation, tunnel and point the provisioning CLI at it.
`homescope-provision` requires HTTPS *or* a literal loopback host, so
`http://127.0.0.1:…` is accepted by design:

```bash
ssh -fN -o ExitOnForwardFailure=yes -L 4001:127.0.0.1:4001 <host>
homescope-provision login --profile prod --api-url http://127.0.0.1:4001
homescope-provision whoami                                  # is it accepted?
homescope-provision list
```

`login` prompts for the token and verifies it before storing it, so a typo fails
there rather than later with a board in your hand and a key already minted:

- **`--api-url` is required the first time a profile is used** — without it,
  `login` reports `unknown profile`. On a re-login it is optional.
- **Always name the profile.** `login` falls back to the configured default, so
  a bare `login --api-url <prod>` repoints your *existing* default profile at
  production instead of creating a new one.

Profiles live in `~/.config/homescope/config.toml` (0644) and their tokens in
`~/.config/homescope/credentials.toml` (0600). The first profile saved becomes
the default; change `default_profile`, or select per invocation with `-p prod` /
`HOMESCOPE_PROFILE=prod`.

## MQTT

Every broker homescope uses authenticates and carries an ACL, local or not. The
users are `homescope-api` (reads `homescope/+/sensors/+/envelope`) and
`homescope-<site>` (writes `homescope/<site>/#`). Topics are
`homescope/<site>/sensors/<device-addr>/envelope`: the site level is the
publishing gateway's `SITE`; the address level is the device address in 12
hex — a live way to learn a board's AdvA without a probe.

With a **local broker** (`homescope-mqtt`, not published), watch it from inside
as the API's user:

```bash
PW=$(sudo homescope secret show mqtt-api)
sudo homescope podman exec -it homescope-mqtt mosquitto_sub -u homescope-api -P "$PW" \
    -t 'homescope/+/sensors/+/envelope' -v
```

With an **external broker**, use any client with a user allowed to read those
topics. ⚠️ An ACL denial is **silent** on Mosquitto: a denied publish is
acknowledged and dropped, and a denied subscription is *granted* and then
receives nothing (verified on 2.0.22) — no client-side check can see it. To see
a publish refusal, publish as MQTT v5: `mosquitto_pub -V 5 -q 1 -u … -t …`
prints `Not authorized`. Traffic on the topic means the receiver and gateway are
healthy, regardless of what the API is doing.

A broker on the *same host* is reached as `host.containers.internal`, not by the
host's LAN address: rootless containers share the host's IP inside their own
network namespace, so that address leads back into the container.

## Receiver and gateway

```bash
ls -l /dev/homescope-receiver                 # udev symlink present?
sudo homescope logs gateway -n 50
```

The symlink comes from `deploy/udev/99-homescope-receiver.rules`, matched on
VID/PID `c0de:cafe` plus the product string. If it is missing after replugging,
`sudo udevadm control --reload-rules && sudo udevadm trigger --subsystem-match=tty`.
The gateway container maps the symlink in directly (`AddDevice=`), so it must be
restarted after the dongle is re-enumerated:

```bash
sudo homescope restart gateway
```

While the dongle is unplugged the unit sits in `auto-restart`, retrying every
30 s; the deploy reports that as a warning, not a failure.

## Troubleshooting

| Symptom | Likely cause | First command |
| --- | --- | --- |
| Nothing runs after a reboot; `homescope status` warns about the user manager | The data disk did not mount | `systemctl status user@$(id -u homescope).service`, `findmnt /srv` |
| Deploy: "no KEK yet. Say which case this is" | First deploy without `--new-kek` / `--import-kek` | decide which case it is — never guess on a restore |
| Deploy: "ACTION NEEDED — … not started yet" | External broker passwords not given, and no terminal to ask on | the `homescope secret set …` lines it printed, then the plain deploy again |
| Deploy: "cannot log in to … as homescope-api" | Wrong password, or the broker's owner has not created the user yet | `sudo homescope secret set mqtt-api && sudo homescope restart api` |
| Deploy: "…is under /srv, which fstab lists but is not mounted" | Data disk missing; the deploy refuses to write underneath it | mount it |
| Deploy: "generated without its host drop-in" | Podman older than 5.0 | upgrade podman |
| API log: `ConnectionRefused(NotAuthorized)` | Wrong or missing broker password | `homescope secret set mqtt-api`, then restart |
| API subscribed, but no readings and no errors | ACL lacks the read rule, or gateway publishes under another site — both silent | `mosquitto_pub -V 5` as the gateway user; check `SITE` |
| `curl: (56) Recv failure: Connection reset by peer` | Container up, nothing listening inside — usually a stale image | `sudo homescope podman inspect homescope-api --format '{{.ImageName}} {{.Created}}'` |
| `curl: (7) Connection refused` | Container not running | `sudo homescope status` |
| API restart loop right after an update | Failed migration; database intact | `sudo homescope logs api -n 100` |
| Device visible on MQTT but never in `readings` | Key `MISSING` / `UNOPENABLE`, so its packets are dropped | `GET /devices` |
| An update "did nothing" | Image build failed in CI; the host kept the last green image | `gh run list --workflow build-api.yml` |

Two rules from the 2026-09-21 incident, when several of these fired at once:
**check the image date before debugging the code**, and **a green `git pull` on
the host says nothing about whether an image was built**.
