# Mosquitto auth + per-gateway ACLs

> **Status: 🔶 partly done (2026-10-05).** Both clients authenticate
> (`host_util::mqtt`: username plus a password read from a file), and the dev
> broker refuses anonymous clients and enforces the production ACL
> (`deploy/mosquitto/dev/`). The production broker on srv01 is asgard's, so its
> users and ACL lines are handed over rather than shipped. The broker that
> homescope's own `broker` component runs is still anonymous — the deploy
> rework settles it. Users are named `homescope-api` and `homescope-<site>`,
> not as below. ⚠️ Verified on 2.0.22: a missing **read** rule does not refuse
> the subscription. It is granted and every delivery is then dropped, so no
> client-side check can see it.
>
> Original status: Checked 2026-09-13: both
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

⚠️ *2026-10-05:* the impersonation half of this predates AEAD (2026-07-31). With
keyless gateways, no topic layout lets a gateway forge a reading. The ACL's job
is now **containment**:

- A homescope credential can write only under its own `homescope/<site>/`
  prefix. That is what makes the topic's site trustworthy provenance.
- It can read nothing.
- On a *shared* broker, the ACL protects the other systems, and that matters
  more than anything homescope-internal. Without it, the network's most exposed
  component could publish into `zigbee2mqtt/#` and drive somebody else's
  devices.

See [site-room-topology.md](site-room-topology.md#the-topic-prefix-re-argued-after-aead-2026-10-05).

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

  ⚠️ *2026-10-05: names.* On a shared broker every user is named after its
  service, so these become `homescope-<site>` (one per site, used by that
  site's gateway or bridge) and `homescope-api`. MQTT client ids are
  `homescope-…` too. See [deployment-topology.md](deployment-topology.md).

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
  a fresh session and orphans the queued messages. ⚠️ *2026-10-05:* renamed to
  `homescope-api` anyway, at the one moment it was free — the move to a new
  broker, which held no session to orphan.
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
  gateway user, so one user per *site* may be enough. ⚠️ *Updated 2026-10-04:*
  the bridge is now the recommended default for remote sites, and the central
  ACL uses one user per site — see
  [deployment-topology.md](deployment-topology.md).
- The central broker may be a shared, general-purpose broker that other systems
  on the site also use. Then homescope does not own its `mosquitto.conf`: ship
  the user list and ACL lines it needs, not a broker configuration.
