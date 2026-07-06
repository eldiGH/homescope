# Homescope

Battery-powered ambient sensors (temperature & humidity per room, plus barometric pressure on one designated indoor node) → BLE 5.0 Coded-PHY extended advertising → nRF52840 USB-CDC receiver dongle → Raspberry Pi gateway → MQTT → (planned) TimescaleDB → Grafana.

Designed for **multi-year battery life** on 2× AA lithium cells, with **per-device encrypted payloads** (planned) and a deliberately simple wire protocol. See [docs/architecture.md](docs/architecture.md) for the full design rationale and tradeoffs.

## Layout

This repo is a monorepo with **two Cargo workspaces** split by target architecture (host vs `thumbv7em-none-eabi`) — a single workspace mixing the two breaks rust-analyzer.

| Path | Crate | Target | What it does |
|------|-------|--------|--------------|
| [`common/`](common/) | `homescope-common` | `no_std`-by-default | Shared `SensorPacket` (wire format), `SensorReading` (app), framing, CRC. |
| [`gateway/`](gateway/) | `homescope-gateway` | host (Pi) | Reads framed packets from the receiver over USB-CDC, decodes, will publish to MQTT. |
| [`firmware/sensor/`](firmware/sensor/) | `homescope-sensor` | `thumbv7em-none-eabi` | Battery-powered XIAO nRF52840 firmware. Reads sensors, broadcasts BLE advertisements. |
| [`firmware/receiver/`](firmware/receiver/) | `homescope-receiver` | `thumbv7em-none-eabi` | USB-CDC dongle firmware. Scans for sensor advertisements, forwards framed packets to the gateway. |

## Hardware

- **Sensor & receiver MCU**: nRF52840 (Cortex-M4F, BLE 5.x). Currently Seeed XIAO nRF52840 **Plus** dev boards — being replaced for range reasons (the chip antenna measured ~10 dB short); next: Raytac MDBT50Q-DB-40 eval boards, then a custom PCB with the pre-certified MDBT50Q module. The XIAO ships with Nordic SoftDevice S140 in flash, so its application offset is `0x27000`. See [docs/flashing.md](docs/flashing.md) and [docs/architecture.md](docs/architecture.md#hardware-platform).
- **Sensors**: SHT45 (temperature/humidity) on every node; BMP581 (pressure) on one designated indoor node, since pressure is house-wide; optionally LTR390 (light/UV) on the outdoor node. Air quality (BME68x + BSEC) is an optional future, separately-powered node. See [docs/architecture.md](docs/architecture.md#sensors).
- **Sensor power**: 2× AA Energizer Lithium L91 (Li-FeS₂) → XIAO 3V3 pin direct on dev boards; the custom PCB will use the nRF52840's VDDH input instead. Expected battery life 5–10+ years at 1–5 min reporting cadence.
- **Receiver power**: USB bus power from the Pi.
- **Gateway host**: Raspberry Pi (any model with USB-A or USB-C and Linux), Mosquitto broker, Rust services as Podman quadlets (planned).

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

### Gateway

```bash
cd gateway
cargo run --release
```

Reads from `/dev/ttyACM0` (or `/dev/homescope-receiver` if a udev symlink is configured) at 115200 baud. The receiver must be plugged into the same machine.

## Wire protocol (receiver → gateway)

30-byte frames over USB-CDC:

```text
+--------+--------+--------------------------+---------------+
| 0x48   | 0x53   | SensorObservation (26 B) | CRC-16 (2B LE)|
+--------+--------+--------------------------+---------------+
```

CRC is CRC-16/IBM-SDLC over the payload bytes. `SensorObservation` is the over-the-air `SensorPacket` plus receiver-observed metadata (RSSI, age). Both ends share framing via `Frame` from `common`. See [docs/protocol.md](docs/protocol.md) for the full spec.

## Status

- ✅ Sensor firmware: Coded-PHY extended advertising, +8 dBm, ~20-event bursts
- ✅ Receiver firmware: coded extended scanning, USB-CDC framing, robust to host disconnect/reconnect
- ✅ Gateway: USB-CDC decoder, MQTT publish, live range-benchmark page (port 3000)
- ⏳ S=8 coding refactor; Raytac MDBT50Q hardware migration (the XIAO antenna is the measured bottleneck)
- ⏳ API service, TimescaleDB, Grafana, container packaging
- ⏳ Per-device ChaCha20-Poly1305 AEAD with sequence-counter replay protection
- ⏳ Sleep/power optimization (System OFF + RTC wakeup), sensor drivers

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
