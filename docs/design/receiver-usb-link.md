# Receiver USB link and gateway activation

> **Status: 🔶 partly done.** Merges two scratch notes: the 2026-07-09 review of
> the receiver→gateway link (verdict: keep USB-CDC) and the later plan for
> udev-driven gateway activation. They disagreed on system- versus user-level
> systemd; the user-level answer is the one that fits the rootless deployment,
> and is the one kept below. Done: the gateway reads its configuration from the
> environment, and a udev rule gives the dongle a stable name. Open: a real
> VID/PID, a hardened udev rule, and a gateway service bound to the device.

## Done

- **Gateway configuration from the environment** — `MQTT_HOST`, `MQTT_PORT`
  and `RECEIVER_PATH` (default `/dev/homescope-receiver`), read in
  `gateway/src/config.rs`. The quadlet sets `MQTT_HOST`.
- **A stable name for the dongle** — `deploy/udev/99-homescope-receiver.rules`,
  installed by `deploy.sh`'s `setup_udev_rule`, links `/dev/homescope-receiver`
  to whichever `ttyACM<n>` the dongle enumerated as. It matches VID/PID plus the
  product string.
- **Rootless device access** — `gateway.container` sets
  `GroupAdd=keep-groups`.

## 1. VID/PID is embassy's example placeholder

`firmware/receiver/src/main.rs` uses `Config::new(0xc0de, 0xcafe)` — the
placeholder pair from embassy's examples. It works on a private LAN forever,
but the udev rule keys on it, and it collides with every other embassy example
project plugged into the same host.

- Minimum: pick a deliberate pair and keep it stable.
- Proper: request a free PID from <https://pid.codes>, which allocates PIDs
  under its VID `0x1209` for open-source hardware — the standard move for
  hobby and OSS devices.
- Whatever the VID/PID, matching on `ATTRS{serial}` is the robust part: the
  receiver already sets `serial_number` to its own device address, hex-encoded.

## 2. Harden the udev rule

Today's rule only creates the symlink. What the finished rule should also do:

1. **Match the serial**, not just VID/PID and product, so a second Homescope
   dongle — or any embassy example on the same host — cannot claim the name.
2. **Tag for systemd** — `TAG+="systemd"` makes systemd generate a `.device`
   unit for the node, which §3 binds to. Without the tag there is no unit to
   bind to.
3. **Silence ModemManager** — MM probes new `ttyACM` devices by writing AT
   commands into them, which would land as garbage bytes in the receiver's CDC
   endpoint. `ENV{ID_MM_DEVICE_IGNORE}="1"` opts out.
4. **Permissions** — `MODE`/`GROUP`, so the rootless container user can open the
   node without running privileged.
5. **Pull in the gateway** — `ENV{SYSTEMD_USER_WANTS}="gateway.service"`; §3 says
   why it is the *user* variant.

```udev
SUBSYSTEM=="tty", ATTRS{idVendor}=="c0de", ATTRS{idProduct}=="cafe", \
  ATTRS{serial}=="<device-addr>", \
  SYMLINK+="homescope-receiver", \
  TAG+="systemd", ENV{SYSTEMD_USER_WANTS}="gateway.service", \
  ENV{ID_MM_DEVICE_IGNORE}="1", \
  MODE="0660", GROUP="dialout"
```

Reload with `udevadm control --reload-rules && udevadm trigger
--subsystem-match=tty`, which is what `setup_udev_rule` already runs. Verify with
`udevadm info /dev/homescope-receiver`.

## 3. Bind the gateway service to the device

Goal: `gateway.service` runs exactly while the dongle is plugged in — started by
udev on plug, stopped cleanly by systemd on unplug. No restart loops, and an
honest `systemctl status`: inactive when unplugged, never parked-failed.

Two reasons, one from each original note:

- **Stale bind mounts.** `AddDevice=` bind-mounts the device node when the
  container starts. If the dongle re-enumerates while the container runs —
  replug, firmware reset, brownout — the mount points at a dead node, and
  nothing inside the container can fix that. Device presence can only be
  handled at the systemd/udev layer.
- **Honest status.** Today the unit is `Restart=on-failure` with
  `RestartSec=30`: while the dongle is absent, container creation fails and
  systemd retries every 30 s. It works, but it is noisy, and "unplugged" reads
  as "failed".

⚠️ **User manager, not system.** The stack runs as rootless podman quadlets
under the `homescope` user, so the gateway is a *user* unit. The earlier sketch
used `ENV{SYSTEMD_WANTS}`, which only reaches the system manager, and kept
`WantedBy=default.target` plus `Restart=always`. For a user unit the rule needs
`SYSTEMD_USER_WANTS`.

In `gateway.container`:

```ini
[Unit]
BindsTo=dev-homescope\x2dreceiver.device
After=dev-homescope\x2dreceiver.device
```

- The unit name comes from `systemd-escape --path /dev/homescope-receiver`;
  the `-` in the path escapes to `\x2d`.
- `BindsTo` is `Requires` plus "stop me when the device vanishes".
- Consider dropping `[Install] WantedBy=default.target`, so starting is purely
  device-driven. Booting with the dongle plugged in still works: udev coldplug
  events fire during boot.

`deploy.sh` names `gateway.service` among the units it manages. With `BindsTo`,
starting or restarting it fails while the dongle is unplugged, and `set -e`
would abort the deploy — use `try-restart` for the gateway, or tolerate that
failure. Check the exact command when making this change.

The gateway binary needs nothing: it has no wait-for-receiver loop to remove
(checked 2026-09-13), so a device missing at open stays a plain fatal error.

## 4. Verify before relying on it

The one uncertain link: do *user* managers on the target systemd version track
tagged device units and honour `SYSTEMD_USER_WANTS`?

1. `systemctl --version` — 250 or later is comfortable.
2. Add the rule changes, reload the rules, physically replug.
3. `systemctl --user list-units --type=device | grep homescope` — expect
   `dev-homescope\x2dreceiver.device loaded active plugged`.
4. A throwaway `receiver-test.service` (`ExecStart=/bin/sleep infinity`, the
   `BindsTo`/`After` above, `SYSTEMD_USER_WANTS` pointed at it): plug → running;
   unplug → inactive within about a second; replug → running.
5. Repeat on the Pi **as the `homescope` user** — a lingering manager with no
   login session, `systemctl --user -M homescope@ …`. That is the
   production-representative run.

## Fallback if user managers do not track the device

Keep what runs today: systemd-paced restarts, where container creation fails
while the device is absent and succeeds once it is plugged in. Functionally
fine, just noisier in the journal.
