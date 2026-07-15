# Homescope

Battery-powered ambient sensors (temperature & humidity per room, plus barometric pressure on one designated indoor node) → BLE 5.0 Coded-PHY extended advertising → nRF52840 USB-CDC receiver dongle → Raspberry Pi gateway → MQTT → Rust ingest API → TimescaleDB → Grafana.

Designed for **multi-year battery life** on 2× AA lithium cells, with **per-device encrypted payloads** (planned) and a deliberately simple wire protocol. See [docs/architecture.md](docs/architecture.md) for the full design rationale and tradeoffs.

## Layout

This repo is a monorepo with **two Cargo workspaces** split by target architecture (host vs `thumbv7em-none-eabi`) — a single workspace mixing the two breaks rust-analyzer.

| Path | Crate | Target | What it does |
|------|-------|--------|--------------|
| [`common/`](common/) | `homescope-common` | `no_std`-by-default | Shared `SensorPacket`/`SensorObservation` (wire formats), `SensorReading` (app/JSON), `HardwareId`, framing, CRC. |
| [`gateway/`](gateway/) | `homescope-gateway` | host (Pi) | Reads framed packets from the receiver over USB-CDC, decodes, publishes JSON readings to MQTT. |
| [`api/`](api/) | `homescope-api` | host (Pi) | Subscribes to MQTT, resolves devices against a registry, stores readings in TimescaleDB. HTTP endpoints planned. |
| [`host-util/`](host-util/) | `homescope-host-util` | host | Shared host-side init (dotenv + tracing) and env-var helpers. |
| [`firmware/board/`](firmware/board/) | `homescope-board` | `thumbv7em-none-eabi` | Board abstraction (`board!(p)` macro + per-board linker scripts; features `board-db40` / `board-xiao`). |
| [`firmware/sensor/`](firmware/sensor/) | `homescope-sensor` | `thumbv7em-none-eabi` | Battery-powered nRF52840 firmware. Reads sensors (SHT45), broadcasts BLE advertisements. |
| [`firmware/receiver/`](firmware/receiver/) | `homescope-receiver` | `thumbv7em-none-eabi` | USB-CDC dongle firmware. Scans for sensor advertisements, forwards framed packets to the gateway. |

Plus [`deploy/`](deploy/) — production deployment for the Pi: rootless Podman quadlets (mosquitto, TimescaleDB, api, gateway, Grafana), an idempotent `deploy.sh`, udev rule, Grafana provisioning, DB backup script. Container images are built per-crate in CI (GitHub Actions, ARM) and published to ghcr.io with `AutoUpdate=registry`.

## Hardware

- **Sensor & receiver MCU**: nRF52840 (Cortex-M4F, BLE 5.x). Currently Raytac **MDBT50Q-DB-40** eval boards (whole-house survey passed 2026-07-03 → the MDBT50Q-1MV2 module is validated for the upcoming custom PCB). The Seeed XIAO nRF52840 **Plus** boards are retired from RF duty (chip antenna measured ~10 dB short) but remain bench mules; they ship with Nordic SoftDevice S140 in flash, so their application offset is `0x27000`. See [docs/flashing.md](docs/flashing.md) and [docs/architecture.md](docs/architecture.md#hardware-platform).
- **Sensors**: SHT45 (temperature/humidity) on every node; BMP581 (pressure) on one designated indoor node, since pressure is house-wide; optionally LTR390 (light/UV) on the outdoor node. Air quality (BME68x + BSEC) is an optional future, separately-powered node. See [docs/architecture.md](docs/architecture.md#sensors).
- **Sensor power**: 2× AA Energizer Lithium L91 (Li-FeS₂) → XIAO 3V3 pin direct on dev boards; the custom PCB will use the nRF52840's VDDH input instead. Expected battery life 5–10+ years at 1–5 min reporting cadence.
- **Receiver power**: USB bus power from the Pi.
- **Gateway host**: Raspberry Pi (any model with USB-A or USB-C and Linux). Everything runs as rootless Podman quadlets: Mosquitto, TimescaleDB, the Rust gateway + API, Grafana. See [`deploy/`](deploy/).

## Building & running

### Firmware (sensor or receiver) — primary flow

With a SWD probe (e.g. Pi Pico DAPLink) wired to the target:

```bash
cd firmware/sensor       # or firmware/receiver
cargo run --release      # flashes via probe-rs + streams defmt logs
```

Or in VSCode: press F5 with a `Debug nrf52840-*` configuration selected. See `.vscode/launch.json`.

### Firmware (sensor) — UF2 backup flow

When the probe isn't available (sealed deployment, field update):

```bash
cd firmware/sensor
./flash_uf2.sh            # builds + converts to UF2 via tools/uf2/uf2conv.py
```

Then double-tap RESET on the XIAO to enter the bootloader and copy the produced `firmware.uf2` onto the mounted drive. See [docs/flashing.md](docs/flashing.md) for the mount setup, `0x27000` offset rationale, and troubleshooting.

The `firmware/receiver/flash_uf2.sh` script is the parallel for the receiver, though the receiver normally just uses probe-rs since it lives at the bench.

### Host services (gateway + API) — dev workflow

The [`justfile`](justfile) drives everything; dev dependencies (Mosquitto, TimescaleDB, Grafana on `localhost:4000`) come from [`compose.dev.yml`](compose.dev.yml) and are started automatically:

```bash
just gateway    # runs homescope-gateway (auto-starts the MQTT broker)
just api        # runs homescope-api (auto-starts broker + Postgres)
just db-seed    # loads ~90 days of fake per-minute readings
just dev        # zellij workspace with the whole stack
```

Both services are configured via env vars (`MQTT_HOST`, `MQTT_PORT`; gateway: `RECEIVER_PATH`, default `/dev/homescope-receiver`; api: `DB_*`, `RUN_MIGRATIONS`), loaded from `.env`/`.env.default` files.

### Production (Raspberry Pi)

```bash
git pull && sudo ./deploy/deploy.sh
```

Idempotent — creates the `homescope` user, installs the udev rule and quadlets, generates secrets once, and converges the stack. CI-built images auto-update via `podman-auto-update`.

## Wire protocol (receiver → gateway)

27-byte frames over USB-CDC:

```text
+--------+--------+--------------------------+---------------+
| 0x48   | 0x53   | SensorObservation (23 B) | CRC-16 (2B LE)|
+--------+--------+--------------------------+---------------+
```

CRC is CRC-16/IBM-SDLC over the payload bytes. `SensorObservation` is the over-the-air `SensorPacket` plus receiver-observed metadata (RSSI, age). Both ends share framing via `Frame` from `common`. See [docs/protocol.md](docs/protocol.md) for the full spec.

The gateway republishes each observation as JSON on MQTT: `homescope/sensors/<hardware-id>/reading`.

## Status

- ✅ Sensor firmware: Coded-PHY extended advertising, +8 dBm, ~20-event bursts, SHT45 + battery SAADC
- ✅ Receiver firmware: coded extended scanning, USB-CDC framing, robust to host disconnect/reconnect
- ✅ Gateway: USB-CDC decoder → MQTT publish (pure bridge; the range-survey page lives on the `reliability-benchmark` branch)
- ✅ API: MQTT → TimescaleDB ingest, device registry (unknown devices dropped, no auto-registration), sqlx migrations
- ✅ Grafana: provisioned datasource + dashboard, anonymous viewer
- ✅ Deployment: Podman quadlets + CI images (ghcr.io) + idempotent deploy script
- ✅ Hardware: Raytac MDBT50Q-DB-40 survey passed → custom PCB with MDBT50Q-1MV2 is next
- ⏳ S=8 coding refactor (raw HCI `LeSetExtAdvParamsV2`)
- ⏳ Ingest durability (seq dedup, manual MQTT acks), graceful shutdown, site topology + broker ACLs
- ⏳ Per-device ChaCha20-Poly1305 AEAD (decrypt in the API; gateways stay keyless)
- ⏳ Sleep/power optimization (System OFF + RTC wakeup), watchdog, BMP581/LTR390 drivers

See the **Implementation roadmap** in [docs/architecture.md](docs/architecture.md#implementation-roadmap) for the full plan.

## Docs

- [docs/architecture.md](docs/architecture.md) — design rationale, hardware choices, BLE vs ESB tradeoff, security model
- [docs/protocol.md](docs/protocol.md) — USB-CDC wire protocol between receiver and gateway
- [docs/flashing.md](docs/flashing.md) — UF2 build & flash workflow, mount setup, troubleshooting
- [CLAUDE.md](CLAUDE.md) — orientation file for AI-assisted development sessions

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

Vendored third-party code under [`tools/uf2/`](tools/uf2/) is licensed
separately under its own MIT license; see [`tools/uf2/LICENSE`](tools/uf2/LICENSE).

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
