# Mosquitto auth + per-gateway ACLs

> **Status: ⏳ planned — not started.** Checked 2026-09-13: both
> `deploy/mosquitto/mosquitto.conf` and `mosquitto.dev.conf` still set
> `allow_anonymous true`. Written 2026-07-14, against the plan of two houses on
> a VPN, each with its own gateway Pi, publishing to one central broker consumed
> by a single API instance. Do this before, or together with, exposing the
> broker to the VPN.

## Why

The VPN authenticates **machines**, not services: anything on the VPN could
connect to the broker, publish arbitrary envelopes, or subscribe to everything.
The remote-house gateway is also the most exposed component — physically
accessible, and keyless by design, since decryption lives in the API. The goal:
a compromised gateway can at worst spam *its own* site's topics. It cannot
impersonate the other house, and it cannot read anything.

## Design

Depends on the site topic prefix, `homescope/<site>/sensors/<device-addr>/envelope`
— see [site-room-topology.md](site-room-topology.md).

- **`password_file`** — one user per client:
  - `gateway-home-a`, `gateway-home-b` — one credential per gateway
  - `api` — the single consumer
  - `allow_anonymous false`
- **`acl_file`** — publish and subscribe split, topic-scoped:

  ```text
  user gateway-home-a
  topic write homescope/home-a/#

  user gateway-home-b
  topic write homescope/home-b/#

  user api
  topic read homescope/+/sensors/+/envelope
  ```

- **Credentials reach each service as files, not environment variables.**
  ⚠️ *Updated 2026-09-13:* this originally said "via env (quadlet
  `EnvironmentFile`)". The project has since settled that secrets are delivered
  as podman secrets mounted as files and named by a path variable — the pattern
  `deploy.sh` uses for the KEK, and the reason the admin token is read from a
  path. An environment variable lands in `/proc/<pid>/environ` and in `podman
  inspect`. Read the password from the mounted file and pass it to rumqttc's
  `set_credentials`.

## Notes and gotchas

- mosquitto's `password_file` needs hashed entries, generated with
  `mosquitto_passwd`. The hashed file is fine to manage in deploy config; the
  plaintext passwords belong in the podman secrets on each host.
- ACL and password-file changes need a `SIGHUP` or a restart.
- The API's durable session (`clean_session=false`) is keyed by client id —
  keep the client id `api` stable when adding credentials, or the broker starts
  a fresh session and orphans the queued messages.
- TLS is *optional* here, since the VPN already encrypts transport; auth is not.
  If the broker ever listens outside the VPN, revisit: TLS becomes mandatory, and
  `require_certificate` with per-client certificates is the next rung.
- The local dev stack can keep an `allow_anonymous true` listener on localhost
  only, or use the same password file with dev credentials. Prefer the latter,
  so dev exercises the auth path.

## Relation to other work

- The site prefix must land first: the gateway's `SITE` setting, the topic
  change, and the `devices.site`/`room` columns, as one PR.
- A mosquitto bridge per site (a local broker spooling during VPN flaps) will
  need its own bridge credentials. Design the user list with that in mind:
  `bridge-home-b` writing `homescope/home-b/#` is indistinguishable from the
  gateway user, so one user per *site* may be enough.
