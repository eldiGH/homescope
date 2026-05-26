# Homescope — project orientation

**Homescope** is an ambient-sensor stack: battery-powered BLE 5.0 sensors (Seeed XIAO nRF52840 Plus) broadcasting temperature / humidity / pressure data, picked up by a dedicated **nRF52840 USB-CDC receiver dongle** plugged into a Raspberry Pi gateway, which decodes the framed packets and (will) decrypt + publish them to a local Mosquitto MQTT broker. A separate API service will subscribe to MQTT and store data in TimescaleDB for visualization in Grafana.

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
│       ├── packet.rs       # SensorPacket (repr(C, packed)), framing, CRC
│       └── reading.rs      # SensorReading (serde, human units)
├── gateway/                # Pi-side receiver decoder + (future) MQTT publisher
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

- ✅ **Sensor firmware** (`firmware/sensor/`): BLE advertising works end-to-end. Broadcasts a `ManufacturerSpecificData` packet every 5 seconds via the `NonconnectableNonscannableUndirected` extended-advertising mode, visible in nRF Connect. 20 ms interval × 3 events per burst (~60 ms total radio time, spec minimum). LED heartbeat blinks once per burst.
- ✅ **Receiver firmware** (`firmware/receiver/`): Scans for our manufacturer-ID advertisements (Coded PHY S=2), framed packets emitted over USB-CDC. Robust to host disconnect/reconnect — DTR-aware writes with disconnect-race in `select`, drop-oldest backlog channel, sequence-based dedup, post-DTR grace period to avoid hammering kernel before its post-open ioctl chain completes.
- ✅ **Common crate**: `SensorPacket` (wire), `SensorReading` (app), `write_frame` / `parse_frame` / `checksum` / `frame()` helpers, CRC-16/IBM-SDLC. Frame layout (magic + payload + CRC) is fully encapsulated.
- ✅ **Gateway v1 receiver path**: `serial2-tokio` + `tokio_util::codec::Decoder<Item = SensorPacket>` over `BytesMut`. Reads `/dev/ttyACM0` (or `/dev/homescope-receiver` via udev symlink), validates magic + CRC, decodes packet, prints.
- ⏳ **Gateway v1 MQTT publish**: not yet wired.
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

- **Board**: Seeed XIAO nRF52840 **Plus** — the "Plus" variant matters; see flash layout below
- **Target**: `thumbv7em-none-eabi` (Cortex-M4F on nRF52840)
- **Bootloader**: Adafruit UF2 v0.9.2 **with Nordic SoftDevice S140 7.3.0 pre-installed** (Board-ID: `nRF52840-SeeedXiao-v1`)
- **Flash layout** (1 MB total):
  - `0x00000000–0x00000FFF`: Nordic MBR (4 KB)
  - `0x00001000–0x00026FFF`: SoftDevice S140 7.3.0 (152 KB, **never started** by our firmware — we use `nrf-sdc` instead, S140 just sits inert in flash)
  - `0x00027000+`: Application (868 KB available)
- **UF2 family ID**: `0xADA52840` (Adafruit nRF52 series)
- **Application base address**: `0x00027000` — set in each `firmware/*/memory.x` and in the `--base` arg of `tools/uf2/uf2conv.py`
- **Power (sensor)**: 2× AA Energizer Lithium L91 → XIAO 3V3 pin direct (no external regulator). See [docs/architecture.md](docs/architecture.md#power).
- **Power (receiver)**: USB bus power from the Pi. Plug-and-play.
- **Probe**: SWD probe (Pi Pico DAPLink or similar) wired and working. Enables defmt-RTT log capture and breakpoint debugging via the VSCode probe-rs-debugger extension.
- **Logging**: `defmt-rtt`. Logs visible in the VSCode Debug Console during a debug session.

## BLE design summary

- **Advertising mode**: non-connectable, non-scannable, undirected (`ADV_NONCONN_IND` via BLE 5.0 extended advertising)
- **PHY**: Coded PHY S=2 by default (~4× range vs 1M PHY at 2× airtime)
- **Interval**: 20 ms (spec minimum for extended non-connectable); 3 events per burst → ~60 ms radio time
- **Burst cadence**: every 5 s during testing; production target is 1–5 min with System OFF sleep between bursts
- **Payload**: `ManufacturerSpecificData` with company ID `0xFFFF` (testing) carrying a `#[repr(C, packed)]` `SensorPacket` struct — direct binary, no serialization framework
- **Security (planned)**: ChaCha20-Poly1305 AEAD with per-device keys, 4-byte sequence counter for replay protection. Not implemented yet.

## USB-CDC wire protocol (receiver → gateway)

See [docs/protocol.md](docs/protocol.md) for the full spec. Quick summary:

- 18-byte frame: 2-byte magic `HS` + 14-byte `SensorPacket` + 2-byte CRC-16/IBM-SDLC over the payload (little-endian on the wire).
- Gateway uses `tokio_util::codec::Decoder` over `BytesMut` with magic-search via `memchr` and frame validation via `SensorPacket::parse_frame`.
- The actual decoder implementation is shorter than the spec — `common` encapsulates magic/CRC/serialization.

## Where to find things

- [README.md](README.md) — top-level overview, pointers into the crates
- [docs/architecture.md](docs/architecture.md) — full design rationale: protocol choice, sensor selection, power topology, security model, BLE vs ESB tradeoff analysis
- [docs/protocol.md](docs/protocol.md) — USB-CDC wire protocol between receiver and gateway
- [docs/flashing.md](docs/flashing.md) — UF2 build & flash workflow, mount setup, troubleshooting
- `~/.claude/plans/let-s-analyze-that-my-glowing-peacock.md` — original full design exploration (lives in Claude's plan store, not committed)
