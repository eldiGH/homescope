# Homescope — project orientation

**Homescope** is an ambient-sensor stack: battery-powered BLE 5.0 sensors broadcasting temperature / humidity / pressure via **Coded-PHY extended advertising**, picked up by a dedicated **nRF52840 USB-CDC receiver dongle** plugged into a Raspberry Pi gateway, which decodes the framed packets and publishes them to a local Mosquitto MQTT broker (payload crypto planned). A separate API service will subscribe to MQTT and store data in TimescaleDB for visualization in Grafana. Hardware is mid-migration (2026-07): XIAO nRF52840 Plus dev boards (retired from RF duty — measured antenna verdict below) → Raytac MDBT50Q-DB-40 eval boards → custom PCB with the MDBT50Q module.

Layout: monorepo with two Cargo workspaces split by target architecture (firmware vs host), plus a shared `common` crate referenced by both.

## How to collaborate with the project owner

The owner is learning Rust, Embassy, and embedded systems through this project. Work with the following defaults:

- **Do NOT edit source code (.rs, Cargo.toml, etc.) unless the owner explicitly asks for an edit.** They want to write code themselves to learn.
- **Default mode is knowledge + code review.** When asked about an error, a concept, or a design choice — explain, point at relevant docs/source, identify the root cause, propose options. Do not jump to making the change.
- **Acceptable to edit without asking**: documentation files (`CLAUDE.md`, `docs/*.md`, `README.md`), and tests when explicitly asked to generate them.
- **When reviewing code**: point out bugs, idiom violations, footguns, and learning opportunities. Explain *why* something is non-idiomatic, not just *that* it is.
- **When the owner asks "how do I…"**: explain enough that they can implement it themselves. Give code snippets only as illustration, not as ready-to-paste solutions, unless they ask for that.
- **The owner has deep general-software experience but limited embedded Rust experience** — frame embedded concepts (memory mapping, interrupts, no_std, async on bare metal) explicitly; don't assume familiarity.

## Repository layout

```text
homescope/
├── Cargo.toml              # host-target workspace: gateway, common
├── common/                 # shared types — homescope-common (no_std-by-default)
│   └── src/
│       ├── lib.rs
│       ├── device_id.rs    # DeviceId(u64) newtype (FICR-sourced)
│       ├── packet.rs       # SensorPacket (repr(C, packed)) — over-the-air payload
│       ├── observation.rs  # SensorObservation = packet + receiver RSSI/age
│       ├── frame.rs        # Frame: magic + payload + CRC-16/IBM-SDLC
│       └── reading.rs      # SensorReading (serde, human units)
├── gateway/                # Pi-side receiver decoder + MQTT publisher + benchmark page
│   └── src/main.rs         # homescope-gateway
├── firmware/
│   ├── Cargo.toml          # firmware workspace: sensor, receiver
│   ├── .cargo/config.toml  # cross-compile target (thumbv7em-none-eabi)
│   ├── rust-toolchain.toml
│   ├── sensor/             # homescope-sensor — BLE-advertising firmware
│   │   ├── memory.x
│   │   ├── flash_uf2.sh    # UF2 backup flow (calls tools/uf2/uf2conv.py)
│   │   └── src/
│   └── receiver/           # homescope-receiver — USB-CDC BLE scanner dongle
│       ├── memory.x
│       ├── flash_uf2.sh
│       └── src/
├── tools/
│   └── uf2/                # vendored microsoft/uf2 tooling (MIT) — see tools/uf2/README.md
│       ├── uf2conv.py
│       ├── uf2families.json
│       ├── LICENSE
│       └── README.md
├── api/                    # (planned) HTTP API + MQTT subscriber + TimescaleDB
├── deploy/                 # (planned) Podman quadlets + k8s pod YAML for Pi
├── docs/
│   ├── architecture.md
│   ├── flashing.md
│   └── protocol.md         # USB-CDC wire protocol between receiver and gateway
└── CLAUDE.md
```

**Two separate Cargo workspaces** (one at repo root for host-target, one at `firmware/` for `thumbv7em-none-eabi`). This split is intentional: a single workspace with mixed targets confuses rust-analyzer (it picks one default target and the other side errors out). The `common` crate is referenced from both workspaces via `path = "../common"`.

## Current state

- ✅ **Sensor firmware** (`firmware/sensor/`): true extended advertising on Coded PHY via `advertise_ext` (`ExtNonconnectableNonscannableUndirected`; trouble's plain `advertise()` is legacy-only — it burned us, see field findings). 20 ms interval, advertiser held ~400 ms → ~20 events/burst. TX +8 dBm via `Builder::default_tx_power(8)` (the per-set HCI field is ignored by the SDC).
- ✅ **Receiver firmware** (`firmware/receiver/`): extended scanning on Coded PHY (`scan_ext` + `on_ext_adv_reports`), framed `SensorObservation`s (packet + RSSI + age_ms) over USB-CDC. Robust to host disconnect/reconnect — DTR-aware writes with disconnect-race in `select`, drop-oldest backlog channel, sequence-based dedup, post-DTR grace period.
- ✅ **Common crate**: `SensorPacket` (air), `SensorObservation` (receiver→gateway), `SensorReading` (app), `DeviceId`, `Frame` (magic + payload + CRC-16/IBM-SDLC) — see docs/protocol.md v0.2 (30-byte frames).
- ✅ **Gateway**: serial decode (`tokio_util` `Decoder` over `BytesMut`), MQTT publish to `homescope/sensors/<device-id>/reading`, and a live range-survey page on port 3000 (10 s rolling delivery %, RSSI stats, sensor-reboot-safe).
- ⏳ **S=8 forcing** (raw-HCI `LeSetExtAdvParamsV2` on the sensor) — next firmware task, worth +4-5 dB.
- ✅ **Hardware migration, stage 1 (2026-07-03)**: 2× Raytac MDBT50Q-DB-40 in hand, whole-house survey **passed** (worst spot ≥85 % delivery after minor repositioning) → **MDBT50Q-1MV2 validated as the production module**. Remaining: custom PCB (MDBT50Q module, VDDH battery topology).
- ⏳ **API, deploy, sensor drivers, crypto, sleep optimization**: not yet started.

## Build & flash

### Primary path: probe-rs + VSCode debugger

The standard workflow uses **probe-rs** with a SWD probe (e.g., Pi Pico DAPLink) for both flashing and debugging. VSCode launch configs in `.vscode/launch.json` provide one-click flash + run + RTT log capture for both sensor and receiver firmware. See **"Debug nrf52840-* (debug build)"** launches.

The `firmware/.cargo/config.toml` sets `runner = "probe-rs run --chip nRF52840_xxAA"`, so `cargo run` from inside any firmware crate also flashes via probe and streams defmt-RTT logs.

### Backup path: UF2 via mass-storage bootloader

For sensor units deployed in sealed enclosures where SWD pads aren't accessible, the Adafruit UF2 bootloader is the fallback. From `firmware/sensor/`:

```bash
./flash_uf2.sh
```

The script (and the equivalent one at `firmware/receiver/flash_uf2.sh`) calls into the shared `tools/uf2/uf2conv.py` to produce a `.uf2` from the built ELF:

```bash
cargo build --release
cargo objcopy --release -- -O binary firmware.bin
python ../../tools/uf2/uf2conv.py firmware.bin \
    --family 0xADA52840 --base 0x27000 --output firmware.uf2
sync
```

To flash: double-tap RESET on the XIAO so the bootloader USB drive appears, then copy `firmware.uf2` onto the mount (or run the script and then `cp`). See [docs/flashing.md](docs/flashing.md) for mount setup, troubleshooting, and why `--base 0x27000` matters.

## Key facts

- **Boards**: Raytac **MDBT50Q-DB-40** eval boards are primary since 2026-07-03 (house survey passed) → custom PCB with the validated MDBT50Q-1MV2 module next. Seeed XIAO nRF52840 **Plus** stays as a bench mule only (retired from RF duty — chip antenna ~10 dB short: −67 dBm @ 1 m @ +8 dBm). The XIAO-specific flash layout below applies to XIAO units only.
- **DB-40 board facts**: no factory bootloader or SoftDevice — application links at `0x00000000` (own `memory.x`, full 1 MB); LEDs LED1/2/3 = P0.13/14/15 (XIAO LED was P0.30), buttons P0.11/12/24/25; SWD via the 1.27 mm Cortex debug header (J1; a 1.27→2.54 adapter bridges to the Pico probe); mini-USB is the nRF52840's own USBD (receiver firmware works as-is). Optional: flash the Adafruit UF2 bootloader (supported target) to restore drag-drop updates — that moves the app base, adjust `memory.x` accordingly.
- **Target**: `thumbv7em-none-eabi` (Cortex-M4F on nRF52840)
- **Bootloader**: Adafruit UF2 v0.9.2 **with Nordic SoftDevice S140 7.3.0 pre-installed** (Board-ID: `nRF52840-SeeedXiao-v1`)
- **Flash layout** (1 MB total):
  - `0x00000000–0x00000FFF`: Nordic MBR (4 KB)
  - `0x00001000–0x00026FFF`: SoftDevice S140 7.3.0 (152 KB, **never started** by our firmware — we use `nrf-sdc` instead, S140 just sits inert in flash)
  - `0x00027000+`: Application (868 KB available)
- **UF2 family ID**: `0xADA52840` (Adafruit nRF52 series)
- **Application base address**: `0x00027000` — set in each `firmware/*/memory.x` and in the `--base` arg of `tools/uf2/uf2conv.py`
- **Power (sensor)**: 2× AA Energizer Lithium L91 (Li-FeS₂) → XIAO 3V3 pin direct on dev boards; the custom PCB feeds **VDDH** instead (internal REG0 buck, `REGOUT0 = 3.0 V`) to eliminate the fresh-pair ~3.6 V absolute-max edge. See [docs/architecture.md](docs/architecture.md#power).
- **Power (receiver)**: USB bus power from the Pi. Plug-and-play.
- **Sensors (decided 2026-05, revised 2026-07)**: all battery nodes use **SHT45** (T/H, ±0.1 °C / ±1 % RH, no heater). **Pressure (BMP581) on exactly one designated *indoor* node** — pressure is house-wide and indoor ≈ outdoor, so the barometer gets friendly conditions and the gateway stays a pure bridge (supersedes the earlier BMP390-on-outdoor-node plan; BMP581 = newer part, async `bmp5` Rust driver). Outdoor node: SHT45 + optional **LTR390** (light/UV). **BME688 / air quality dropped from the battery fleet** (raw gas ≠ IAQ without BSEC, and the gas heater self-heats T/H); IAQ deferred to an optional USB/mains-powered BME68x + BSEC node. Node variants are one codebase behind Cargo features. See [docs/architecture.md](docs/architecture.md#sensors).
- **BLE/SDC gotchas (hard-won 2026-07)**: SDC features are build-time opt-ins (`support_ext_adv`, `support_le_coded_phy`, `support_ext_scan`); ext adv / coded PHY / ext scan exist **only in the multirole SDC library** (enable both `peripheral` + `central` cargo features on nrf-sdc); TX power only via `default_tx_power()`; trouble's `advertise()` is legacy-only (use `advertise_ext`); `panic-probe` needs the `print-defmt` feature or panics are silent halts. Full list: [docs/architecture.md — field findings](docs/architecture.md#field-findings--rf-debugging-2026-07).
- **Probe**: SWD probe (Pi Pico DAPLink or similar) wired and working. Enables defmt-RTT log capture and breakpoint debugging via the VSCode probe-rs-debugger extension.
- **Logging**: `defmt-rtt`. Logs visible in the VSCode Debug Console during a debug session.

## BLE design summary

- **Advertising mode**: non-connectable, non-scannable, undirected **extended advertising** (`ExtNonconnectableNonscannableUndirected` via `Peripheral::advertise_ext`)
- **PHY**: Coded PHY (primary + secondary). Coding is currently the SDC default; forcing **S=8** (−103 dBm sensitivity) via `LeSetExtAdvParamsV2` is the next firmware task
- **TX power**: +8 dBm via nrf-sdc `Builder::default_tx_power(8)` — the per-set HCI request field is ignored by the SDC
- **Interval/burst**: 20 ms × ~20 events (~400 ms); AUX payloads channel-hop per event, so a burst doubles as frequency diversity (per-packet RSSI swings ±10-15 dB indoors — judge medians)
- **Burst cadence**: ~0.5 s during benchmarking; production target is 1–5 min with System OFF sleep between bursts
- **Payload**: `ManufacturerSpecificData` with company ID `0xFFFF` (testing) carrying a `#[repr(C, packed)]` `SensorPacket` struct — direct binary, no serialization framework; extended adv gives 254 B headroom (the planned AEAD tag never fit legacy's 31 B)
- **Security (planned)**: ChaCha20-Poly1305 AEAD with per-device keys, 4-byte sequence counter for replay protection. Not implemented yet.

## USB-CDC wire protocol (receiver → gateway)

See [docs/protocol.md](docs/protocol.md) for the full spec. Quick summary:

- 30-byte frame: 2-byte magic `HS` + 26-byte `SensorObservation` (air-packet fields + receiver-side `rssi`/`age_ms` + 64-bit `DeviceId`) + 2-byte CRC-16/IBM-SDLC over the payload (little-endian on the wire).
- Gateway uses `tokio_util::codec::Decoder` over `BytesMut` with magic-search via `memchr` and frame validation via `Frame::try_from_bytes`.
- The actual decoder implementation is shorter than the spec — `common` encapsulates magic/CRC/serialization.

## Where to find things

- [README.md](README.md) — top-level overview, pointers into the crates
- [docs/architecture.md](docs/architecture.md) — full design rationale: protocol choice, sensor selection, power topology, security model, BLE vs ESB tradeoff analysis
- [docs/protocol.md](docs/protocol.md) — USB-CDC wire protocol between receiver and gateway
- [docs/flashing.md](docs/flashing.md) — UF2 build & flash workflow, mount setup, troubleshooting
- `~/.claude/plans/let-s-analyze-that-my-glowing-peacock.md` — original full design exploration (lives in Claude's plan store, not committed)
