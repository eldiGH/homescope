# Operations — running the Pi

Day-to-day production management: where things live, how to read logs, how to
update, how to reach the database. For *why* the stack is shaped this way see
[architecture.md](architecture.md); for the deploy mechanics see the header
comment in [`deploy/deploy.sh`](../deploy/deploy.sh).

## The one gotcha that wastes the most time

**Rootless podman fails if your current directory is one the `homescope` user
cannot read.** It re-execs itself inside a user namespace and the child
`chdir()`s back to your cwd; from `~/Projects/homescope` (home dirs are `0700`)
that fails before podman does any work:

```
cannot chdir to /home/pi/Projects/homescope/deploy: Permission denied
```

So every ad-hoc `sudo -u homescope … podman …` needs a neutral cwd. `cd /`
first, or use `machinectl shell` below, which lands you in `/`. The scripts
handle it themselves (`deploy.sh` and `backup-db.sh` both chdir before
switching user) — this only bites interactive commands.

## Machine layout

| What | Where |
| --- | --- |
| Service user | `homescope` (lingering, `/var/lib/homescope`) |
| Quadlets | `/var/lib/homescope/.config/containers/systemd/` |
| Config + env files | `/var/lib/homescope/.config/homescope/` |
| Staged deploy tree | `/var/lib/homescope/deploy-src/` (what the last deploy shipped) |
| DB backups | `/var/lib/homescope/backups/` |
| Podman volumes | `systemd-timescaledb-data`, `systemd-mosquitto-data` |
| Receiver dongle | `/dev/homescope-receiver` (udev symlink) |

Containers, all on the `systemd-homescope` podman network:

| Unit | Container | Image | Host port |
| --- | --- | --- | --- |
| `api.service` | `homescope-api` | `ghcr.io/eldigh/homescope-api:latest` | `127.0.0.1:4001` → 3000 |
| `gateway.service` | `homescope-gateway` | `ghcr.io/eldigh/homescope-gateway:latest` | — |
| `timescaledb.service` | `homescope-db` | `timescale/timescaledb:2.28.2-pg18` | `127.0.0.1:5432` → 5432 |
| `mosquitto.service` | `homescope-mqtt` | `eclipse-mosquitto:2-openssl` | — |
| `grafana.service` | `homescope-grafana` | `grafana/grafana:12.2` | `4000` → 3000, **all interfaces** |

Only Grafana is exposed to the LAN (anonymous viewer, by design). The API and
Postgres are loopback-only — the API because its admin token is a bearer token
with no TLS in front of it, Postgres because its password travels in cleartext;
both are meant to be reached over an SSH tunnel. Mosquitto is not published at
all, so it is reachable only from inside the podman network.

## Becoming the homescope user

The full-session way, which sets `XDG_RUNTIME_DIR` correctly and starts in `/`:

```bash
sudo machinectl shell homescope@.host
```

Inside that shell, plain `podman ps` and `systemctl --user status api` work.

The one-shot way, for a single command from a root shell:

```bash
cd / && sudo -u homescope XDG_RUNTIME_DIR=/run/user/$(id -u homescope) podman ps
```

Worth putting in your `~/.bashrc` on the Pi:

```bash
hpodman() { (cd / && sudo -u homescope XDG_RUNTIME_DIR=/run/user/$(id -u homescope) podman "$@"); }
```

Every `hpodman` below means exactly that.

## Logs

Container output goes to the journal under the **container name**; systemd's own
start/stop/restart bookkeeping goes under the **unit name**. From any root
shell, any cwd:

```bash
sudo journalctl -t homescope-api -f              # what the API printed
sudo journalctl -t homescope-gateway -n 200      # ditto, gateway
sudo journalctl -t homescope-db -n 200           # postgres

sudo journalctl _SYSTEMD_USER_UNIT=api.service -f   # output + restarts + exit codes
```

Use the unit form when a service is crash-looping — it shows the restarts, not
just the messages between them.

```bash
sudo journalctl _SYSTEMD_USER_UNIT=api.service --since '30 min ago'
sudo journalctl _SYSTEMD_USER_UNIT=api.service -p warning      # warn and above
hpodman logs -f homescope-api                                  # podman's own view
```

## Service control

From a root shell, `-M homescope@` reaches the user manager:

```bash
sudo systemctl --user -M homescope@ status api.service
sudo systemctl --user -M homescope@ restart api.service
sudo systemctl --user -M homescope@ list-units --type=service

# restart count and last exit status, without scrolling the journal
sudo systemctl --user -M homescope@ show api.service -p NRestarts -p ExecMainStatus
```

Quadlets are generated units: you cannot `systemctl edit` them, and after
changing a `.container` file you must `daemon-reload` before restarting.

```bash
sudo systemctl --user -M homescope@ daemon-reload
```

## Updating

The flow is: push to `main` → GitHub Actions builds an ARM image → ghcr.io →
the Pi's auto-update timer pulls it.

```bash
# what would change, without changing it
hpodman auto-update --dry-run

# pull and restart anything stale, now
sudo systemctl --user -M homescope@ start podman-auto-update.service
```

The timer runs **every 5 minutes** (`deploy/systemd/podman-auto-update.timer.d/override.conf`).
`podman auto-update` rolls a container back to the previous image if the new one
fails to start — but "starts and then exits" is a successful start, so a binary
that crashes on a bad migration will loop, not roll back.

Image builds only fire on paths the workflow watches
(`.github/workflows/build-{api,gateway}.yml`). **A red build means the Pi
silently keeps running the last green image** — check before assuming a deploy
landed:

```bash
gh run list --workflow build-api.yml --limit 5          # from the workstation
hpodman inspect homescope-api --format '{{.ImageName}}  created {{.Created}}'
```

Config, quadlet or script changes need a converge instead — image pulls alone
won't carry them:

```bash
cd ~/Projects/homescope && git pull && sudo ./deploy/deploy.sh
```

`deploy.sh` is idempotent and safe to rerun: it never overwrites existing
secrets, and it ends by reloading systemd, running an auto-update pass and
restarting all five services.

## Secrets

| Secret | Lives in | Read it with |
| --- | --- | --- |
| DB passwords (postgres, api, grafana) | `~homescope/.config/homescope/db.env`, `api.env`, `grafana.env` (mode 600) | `sudo cat` |
| Grafana admin password | `~homescope/.config/homescope/grafana.env` | `sudo cat` |
| KEK (wraps every device key) | podman secret `homescope-kek` | below |
| Admin API token | podman secret `homescope-admin-token` | below |

```bash
hpodman secret inspect --showsecret -f '{{.SecretData}}' homescope-kek
hpodman secret inspect --showsecret -f '{{.SecretData}}' homescope-admin-token
```

Both reach the API as tmpfs files under `/run/secrets/`, never as environment
variables — so they are not in `/proc/<pid>/environ` or `podman inspect` output.

⚠️ **The KEK is not in the database backups, on purpose.** `devices.key` holds
every sensor's AEAD key wrapped under it: a dump plus the KEK is the whole
fleet, a dump alone is inert. That only holds while the two are stored apart —
back the KEK up somewhere other than wherever the DB backups go. Losing it means
re-provisioning every sensor by hand.

The admin token is *not* worth backing up. Revoking it is:

```bash
hpodman secret rm homescope-admin-token && sudo ./deploy/deploy.sh
```

## Database

### From the Pi

```bash
hpodman exec -it homescope-db psql -U postgres -d homescope
```

Useful one-liners:

```sql
\dt                                    -- tables
SELECT * FROM _sqlx_migrations ORDER BY version;   -- what has actually applied
SELECT id, to_hex(device_addr) AS addr, name,
       key IS NOT NULL AS has_key, key_valid_from FROM devices ORDER BY id;
SELECT count(*), min(time), max(time) FROM readings;
SELECT d.name, max(r.time) FROM devices d
  LEFT JOIN readings r ON r.device_id = d.id GROUP BY d.name;
```

`to_hex(device_addr)` renders the same 12-hex string as the MQTT topic and the
provisioning CLI. It is always exactly 12 characters, with no padding needed:
the top two bits of an AdvA are forced to 1 (static-random marking), so the
leading byte is never below `0xC0`. The column is a BIGINT because a 48-bit
address always fits one, positively.

### From your workstation (lazysql, DBeaver, psql…)

`timescaledb.container` publishes Postgres on the Pi's **loopback**, so the way
in is an SSH tunnel:

```bash
ssh -N -L 15432:127.0.0.1:5432 pi@rpi5-jawo
lazysql 'postgres://api:<API_DB_PASSWORD>@127.0.0.1:15432/homescope'
```

The password is `API_DB_PASSWORD` from `~homescope/.config/homescope/db.env`
(user `api`, owns the schema), or `POSTGRES_PASSWORD` for the superuser.

Why a published port is needed at all, given you could just SSH in: a rootless
container's IP is **not routable from the host namespace**, so `homescope-db`
is unreachable from the Pi's own shell too — running the client on the server
would not have avoided this. Loopback-only keeps it off the LAN; the tunnel
carries it, already authenticated and encrypted. Never widen it to
`5432:5432` — Postgres would be authenticating with a password in cleartext
across the network.

For a container port that *isn't* published (mosquitto, or the API's 3000
inside its namespace), bridge it for as long as the command runs:

```bash
hpodman run --rm --network systemd-homescope -p 127.0.0.1:1883:1883 \
    docker.io/alpine/socat TCP-LISTEN:1883,fork,reuseaddr TCP:homescope-mqtt:1883
```

Ctrl-C removes it, leaving no config behind.

### Backup and restore

```bash
sudo ./deploy/backup-db.sh          # dump + globals into /var/lib/homescope/backups
```

Take one before every migration. Restore is deliberately manual — the full
sequence (stop writers, drop, `timescaledb_pre_restore`, `pg_restore`,
`timescaledb_post_restore`) is in the header comment of
[`deploy/backup-db.sh`](../deploy/backup-db.sh). The TimescaleDB extension
version must match the dump's, so restore onto the same image tag.

### Migrations

The API runs pending migrations at startup (`RUN_MIGRATIONS=true`). Each runs in
its own transaction, so a failure leaves the database **untouched** — the API
just exits and systemd restarts it every 5s. Read the first error of any restart
cycle; the rest is noise.

A migration that fails on a constraint is telling you the *data* is wrong, not
the migration. Fix the rows, then restart the service — it picks up where it
stopped.

## API

```bash
# from the Pi
curl -s http://127.0.0.1:4001/health

# authenticated endpoints
TOKEN=$(hpodman secret inspect --showsecret -f '{{.SecretData}}' homescope-admin-token)
curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:4001/devices | jq
```

Routes: `GET /health` (public), and behind the token `POST /devices`,
`GET /devices`, `GET /devices/<addr>`, `POST /devices/<addr>/rotate-key`.

A device whose `keyStatus` is `MISSING` has no key in the database: the registry
skips it at startup and **drops every packet it sends**. That is the expected
state for a row that predates provisioning — fix it with a rotate.

From the workstation, tunnel and point the provisioning CLI at the tunnel.
`homescope-provision` requires HTTPS *or* a literal loopback host, so
`http://127.0.0.1:…` is accepted by design:

```bash
ssh -N -L 4001:127.0.0.1:4001 pi@rpi5-jawo
homescope-provision login --api-url http://127.0.0.1:4001   # prompts for the token
homescope-provision whoami                                  # is it accepted?
homescope-provision list
```

`login` verifies the token before storing it, so a typo fails there rather than
later with a board in your hand.

## MQTT

Not published; debug from inside the container.

```bash
hpodman exec -it homescope-mqtt mosquitto_sub -t 'homescope/#' -v      # everything
hpodman exec -it homescope-mqtt mosquitto_sub -t 'homescope/sensors/+/envelope' -v
```

The topic segment is the device address in 12 hex — a live way to learn a
board's AdvA without a probe. Traffic here means the receiver and gateway are
healthy, regardless of what the API is doing.

## Receiver and gateway

```bash
ls -l /dev/homescope-receiver                 # udev symlink present?
sudo journalctl -t homescope-gateway -n 50
```

The symlink comes from `deploy/udev/99-homescope-receiver.rules`, matched on
VID/PID `c0de:cafe` plus the product string. If it is missing after replugging,
`sudo udevadm control --reload-rules && sudo udevadm trigger --subsystem-match=tty`.
The gateway container maps the symlink in directly (`AddDevice=`), so the
container must be restarted after the dongle is re-enumerated:

```bash
sudo systemctl --user -M homescope@ restart gateway.service
```

## Troubleshooting

| Symptom | Likely cause | First command |
| --- | --- | --- |
| `curl: (56) Recv failure: Connection reset by peer` | Container is up, nothing listening *inside* it — usually a stale image without the HTTP server | `hpodman inspect homescope-api --format '{{.ImageName}} {{.Created}}'` |
| `curl: (7) Connection refused` | Container is not running at all | `sudo systemctl --user -M homescope@ status api.service` |
| `cannot chdir to …: Permission denied` | Ran rootless podman from a directory `homescope` cannot read | `cd /` and retry |
| API restart loop right after a deploy | Failed migration; DB is intact | `sudo journalctl _SYSTEMD_USER_UNIT=api.service -n 100` |
| Grafana empty, no new readings, no errors anywhere | Gateway and API on mismatched image versions → different MQTT topics | `mosquitto_sub -t 'homescope/#' -v` + compare image dates |
| Device visible on MQTT but never in `readings` | `devices.key` is NULL, so the API drops its packets | `GET /devices`, look for `MISSING` |
| A deploy "did nothing" | Image build failed in CI; the Pi kept the last green image | `gh run list --workflow build-api.yml` |

Two rules that come out of the 2026-09-21 incident, when all of these fired at
once: **check the image date before debugging the code**, and **a green
`git pull` on the Pi says nothing about whether an image was built**.
