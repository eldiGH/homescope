# Homescope — project orientation

**Homescope** is an ambient-sensor stack: battery-powered BLE 5.0 sensors broadcasting temperature / humidity / pressure via **Coded-PHY extended advertising**, picked up by a dedicated **nRF52840 USB-CDC receiver dongle** plugged into a Raspberry Pi gateway, which decodes the framed packets and publishes them to a Mosquitto MQTT broker (payload crypto planned — decrypt happens in the API, not the gateway). The **`homescope-api`** service subscribes to MQTT and stores readings in **TimescaleDB**, visualized in **Grafana**; everything Pi-side runs as rootless Podman quadlets with CI-built images. Hardware is mid-migration (2026-07): XIAO nRF52840 Plus dev boards (retired from RF duty — measured antenna verdict below) → Raytac MDBT50Q-DB-40 eval boards → custom PCB with the MDBT50Q module.

Layout: monorepo with two Cargo workspaces split by target architecture (firmware vs host), plus a shared `common` crate referenced by both.

**Uncommitted scratch plans**: `NOTES-*.md` files at the repo root (untracked by design) hold the owner's worked-out plans for upcoming backend/deploy tasks — graceful shutdown, ingest DB-error handling / manual acks, site+room topology, mosquitto ACLs, udev device activation, USB-CDC tightening, the packet TV + seq-persistence + AEAD block (`NOTES-packet-tv-aead.md`), and device provisioning (`NOTES-provisioning.md`). When one of those topics comes up, read the matching NOTES file first; the decisions in them are settled.

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
├── Cargo.toml              # host-target workspace: gateway, api, common, host-util
├── justfile                # dev workflow: just api / gateway / db-seed / grafana-pull / dev
├── compose.dev.yml         # dev stack: mosquitto + TimescaleDB + Grafana (localhost:4000)
├── common/                 # shared types — homescope-common (no_std, NO default features; features: codec, serde, defmt)
│   └── src/
│       ├── lib.rs          # crate-level feature docs; unconditional #![no_std]
│       ├── device_addr.rs  # DeviceAddr([u8; 6]) — BLE advertising address (AdvA, FICR-derived), 12-hex Display/serde
│       ├── wire.rs         # Wire trait (fixed-size LE codec) + wire_units! macro — one declaration per unit emits Wire + Display + defmt::Format + as_f64 (Millivolts, CentiCelsius, CentiPercent, Dbm) + Truncated/BufferTooSmall
│       ├── measurement.rs  # TV measurement-ID registry — Measurement enum (id ⇒ semantics+repr+scale), macro-generated encode/decode
│       ├── packet.rs       # SensorPacket (borrowed view) — air payload [seq: u32][TV section]; encode/parse + Measurements iterator; MAX_WIRE_LEN=252
│       ├── observation.rs  # SensorObservation (borrowed view): 11-B header (device_addr, age_ms, rssi) + opaque packet = rest; impls frame::Encode
│       ├── frame.rs        # content-agnostic framing: Encode trait + encode/parse — [magic HS][len u16][payload][crc], cap 1024, streaming 3-outcome parse (Corrupt carries a discard hint)
│       ├── observation_envelope.rs  # ObservationEnvelope (serde camelCase) — gateway→API MQTT JSON: cleartext addr/rssi/receivedAt + base64 packet blob
│       └── reading.rs      # SensorReading — the API's decoded type: cleartext header + one Option per metric; TryFrom<&ObservationEnvelope> + SensorReadingError
├── gateway/                # Pi-side bridge: USB-CDC frames → opaque MQTT envelope — homescope-gateway
│   ├── Containerfile
│   └── src/                # main.rs, config.rs (env), decoder.rs (tokio_util Decoder over frame::parse)
├── api/                    # homescope-api — MQTT → TimescaleDB ingest (HTTP endpoints next; axum staged)
│   ├── Containerfile
│   ├── migrations/         # sqlx: readings hypertable, devices table
│   └── src/                # main.rs, config.rs, db.rs, ingest/ (+unknown.rs), devices/ (registry/cache/store)
├── host-util/              # homescope-host-util — shared init() (dotenv+tracing) + env_var_or
├── firmware/
│   ├── Cargo.toml          # firmware workspace: sensor, receiver, board
│   ├── .cargo/config.toml  # cross-compile target (thumbv7em-none-eabi)
│   ├── rust-toolchain.toml
│   ├── board/              # homescope-board — Board struct + board!(p) macro (features: db40 / xiao)
│   │   ├── build.rs        # picks memory-*.x by board feature
│   │   ├── memory-db40.x   # bare board — app at 0x00000000
│   │   ├── memory-xiao.x   # UF2 bootloader — app at 0x00027000
│   │   └── src/lib.rs
│   ├── sensor/             # homescope-sensor — BLE-advertising firmware
│   │   ├── flash_uf2.sh    # UF2 backup flow (calls tools/uf2/uf2conv.py)
│   │   └── src/            # main.rs, ble_advertise.rs, sensors.rs + sensors/ (sht4x, battery), packet_builder.rs, seq_counter.rs (flash-checkpointed seq)
│   └── receiver/           # homescope-receiver — USB-CDC BLE scanner dongle
│       ├── flash_uf2.sh
│       └── src/            # main.rs, ble_scan.rs, scanned_packet.rs (owned channel message), lru_cache.rs
├── deploy/                 # production deployment (Pi)
│   ├── deploy.sh           # idempotent converge script (root phase + homescope-user phase)
│   ├── backup-db.sh
│   ├── quadlets/           # mosquitto/timescaledb/api/gateway/grafana .container + network/volumes
│   ├── grafana/            # provisioning + committed dashboard JSON (round-trip: just grafana-pull)
│   ├── mosquitto/          # mosquitto.conf (prod: persistence) + mosquitto.dev.conf
│   ├── timescaledb/        # init roles + seed.dev.sql
│   ├── systemd/            # podman-auto-update timer override
│   └── udev/               # 99-homescope-receiver.rules → /dev/homescope-receiver
├── .github/workflows/      # build-{gateway,api}.yml → build-image.yml: ARM images → ghcr.io/eldigh/*
├── tools/
│   └── uf2/                # vendored microsoft/uf2 tooling (MIT) — see tools/uf2/README.md
├── docs/
│   ├── architecture.md
│   ├── flashing.md
│   └── protocol.md         # USB-CDC wire protocol between receiver and gateway
├── NOTES-*.md              # untracked scratch plans for upcoming work (see intro)
└── CLAUDE.md
```

**Two separate Cargo workspaces** (one at repo root for host-target, one at `firmware/` for `thumbv7em-none-eabi`). This split is intentional: a single workspace with mixed targets confuses rust-analyzer (it picks one default target and the other side errors out). The `common` crate is referenced from both workspaces via `path = "../common"`.

## Current state

- ✅ **Sensor firmware** (`firmware/sensor/`): true extended advertising on Coded PHY via `advertise_ext` (`ExtNonconnectableNonscannableUndirected`; trouble's plain `advertise()` is legacy-only — it burned us, see field findings), `primary_phy`/`secondary_phy` explicitly `LeCoded` (trouble defaults to 1M — that also burned us). 20 ms interval, advertiser held ~400 ms → ~20 events/burst. TX +8 dBm via `Builder::default_tx_power(8)` (the per-set HCI field is ignored by the SDC). Reads **SHT45** over async TWIM (`sht4x` driver, optional power-gated rail), samples battery voltage via SAADC, packet cadence 60 s. Emits **TV packets** (`PacketBuilder` + `Measurement` registry) with a **flash-persisted seq counter** (`seq_counter.rs`: two-page circular append log, reservation block 1024, jump-ahead on boot).
- ✅ **Board abstraction** (`firmware/board/`): `Board` struct holds *only board-varying* resources (LED / I²C / sensor-power-gate pins as `Peri<'static, AnyPin>`, SAADC battery input as `AnyInput`, divider ratio); the cfg'd `board!(p)` macro constructs it via partial moves so `Peripherals` stays usable in `main` for chip-fixed peripherals (RNG, PPI, TWIM, SAADC, MPSL set). Deliberately **not** an owning BSP struct — see docs/architecture.md. Caveat: cfg'd macro arms only compile under their own feature — check both configs (`cargo clippy --workspace` per board) before calling a change done.
- ⏳ **XIAO alkaline soak test** (planned 2026-07-10): bare XIAO Plus (no expansion board) + 2× AA alkaline → 3V3 pin; SHT45 direct-wired (SDA P1.14, SCL P1.13, power-gate P1.15); UF2-flashed, no probe attached (SWD debug mode inflates sleep current). Measures delivery reliability + battery longevity of *current pre-sleep-optimization* firmware via per-minute battery_mv telemetry on the gateway. Prerequisite advised: watchdog — without a probe, a panic is a silent HardFault spin that drains the pack.
- ✅ **Receiver firmware** (`firmware/receiver/`): extended scanning on Coded PHY (`scan_ext` + `on_ext_adv_reports`), v0.5 variable-length frames over USB-CDC. **Semantics-blind**: reads only `seq` (fixed cleartext offset) for per-device LRU dedup; packet bytes forwarded opaquely. ⚠️ The dedup cache is `LruCache<DeviceAddr, u32, 32>` and the only gate in front of it is "company ID `0xFFFF` and ≥4 bytes" — so foreign advertisers claim slots and can **evict real sensors**, after which a burst's ~20 events all forward instead of one. This is the main reason for v0.6's magic (below), not bandwidth. Scan handler → drop-oldest `ScannedPacket` channel (owned message: captured_at + addr + rssi + packet bytes; depth 512 ≈ 139 KB — deliberate, fits with ~103 KB RAM to spare) → writer stamps `age_ms` at send time, builds the `SensorObservation` view, chunked CDC writes (64 B FS bulk limit + ZLP on exact multiples). Robust to host disconnect/reconnect — DTR-aware writes with disconnect-race in `select`, post-DTR grace period.
- ✅ **Common crate** (TV redesign landed 2026-07-25): `wire.rs` (`Wire` trait + unit newtypes + layered errors), `measurement.rs` (**TV measurement-ID registry**: 0x01 battery mV u16, 0x02 temperature centi-°C i16, 0x03 humidity centi-%RH u16), `SensorPacket` (borrowed view over `[seq: u32][TV section]`, `MAX_WIRE_LEN` 252, `Measurements` iterator), `SensorObservation` (borrowed view: 11-byte header device_addr+age_ms+rssi, then opaque packet = rest; implements `frame::Encode`), **content-agnostic framing** in `frame.rs`: `Encode` trait (+ per-type compile-time cap check via inline `const` block), `frame::encode`/`frame::parse` — `[magic "HS"][len: u16 LE][payload][crc]`, CRC over len+payload, `MAX_PAYLOAD_LEN` 1024 (transport cap, anti-stall), parse = three-outcome streaming (Incomplete = wait, not error / Ok{payload, consumed} / Corrupt{error, discard} — discard hint computed in-parser via memchr to the next magic candidate, always ≥1) — see docs/protocol.md **v0.5**. `ObservationEnvelope` (2026-07-26): the gateway→API MQTT JSON — cleartext deviceAddr/rssi/receivedAt + base64 `packet` blob, serde+alloc, `from_observation(obs, now)` takes the clock explicitly. Features (**none on by default**): `codec` (crc, memchr), `serde` (serde+chrono+base64, pulls `alloc`), `defmt`. Renamed from `wire` — a feature and a module sharing a name is permanent confusion. Every optional dep carries `default-features = false`; without it `serde` drags in `std` and breaks `thumbv7em-none-eabi`, which a per-crate `cargo check -p` will *not* catch (it resolves features narrowly — test the powerset on both targets). Tests: frame/observation/packet/measurement round-trip + boundaries, plus (2026-07-28) `wire.rs` unit rendering (zero-padded fractions, sign of −0.50 °C, LE byte order) and `reading.rs` decode (partial packets, duplicate/unknown ID, `NoMeasurements`). ⚠️ `cargo test -p homescope-common` alone reports **0 tests** — `reading` needs `--features serde,codec`. The `defmt` half of `wire_units!` is untestable on the host (defmt interns format strings at compile time and emits only on-device): the shared `format_number` arithmetic is covered, but the two format literals must be kept in step by reading them.
- ✅ **Gateway** (v0.5 migration landed 2026-07-26): a pure, semantics-blind bridge — `tokio_util` `Decoder` loops over `frame::parse` (Incomplete → `Ok(None)`; Ok → `SensorObservation::parse`, bad observation = drop + continue, never `Ok(None)` after consuming; Corrupt → advance `discard`), `received_at = now − age_ms`, then publishes the opaque `ObservationEnvelope` JSON (QoS 1) to `homescope/sensors/<device-addr>/envelope` (new-schema-new-topic: the old `…/reading` name is retired with the decoded-JSON payload it carried) — packet blob forwarded byte-for-byte as base64, never decoded gateway-side. Env-configured (`MQTT_HOST`, `MQTT_PORT`, `RECEIVER_PATH` default `/dev/homescope-receiver`) via `host-util`. The old range-survey page (port 3000) was retired to the `reliability-benchmark` branch — check it out to rerun a survey.
- ✅ **API** (`homescope-api`, v0.5 migration landed 2026-07-28): MQTT→TimescaleDB ingest — rumqttc durable session (`clean_session=false`, QoS 1, client id `api`) subscribing `homescope/sensors/+/envelope` → bounded mpsc(256, try_send drop+warn) → sqlx writer. Per-envelope work is an `#[instrument]`ed `handle_envelope` (device_addr on the span, not repeated per event). Decode = `SensorReading::try_from(&envelope)` in `common`: base64 → `SensorPacket::parse` → `Measurements` folded into one `Option` per metric. **Reject vs store rule**: unknown ID / truncated / duplicate ID / zero measurements ⇒ reject the packet; a *missing* metric ⇒ store with NULL (metric columns went nullable in the same release; `time`/`device_id`/`seq`/`rssi` stay NOT NULL). `devices` table (id, unique `device_addr`, name) loaded into a `DeviceRegistry` cache at startup; readings FK to `devices.id`; **unknown devices warn-once + drop, no auto-registration** (table becomes the key registry when AEAD lands). `UNIQUE (device_id, seq, time)` + `ON CONFLICT DO NOTHING` makes MQTT redelivery idempotent (`time` must be in the key — TimescaleDB rejects unique indexes omitting the partitioning column); multi-receiver dedup still needs the per-device seq check. Migrations at startup with `RUN_MIGRATIONS=true`. HTTP endpoints not started (axum in deps). ⚠️ Known gap: DB insert errors log-and-continue while rumqttc auto-acks → readings lost during DB outages (see NOTES-ingest-db-error-handling.md; end-state = manual acks + seq dedup). ⚠️ `api/.sqlx` is keyed by a hash of the query *string* — reformatting SQL orphans the entry and breaks the `SQLX_OFFLINE=true` container build; re-run `cargo sqlx prepare`.
- ✅ **Deployment**: rootless Podman quadlets (mosquitto w/ persistence, TimescaleDB 2.x/PG18, api, gateway, grafana) under a `homescope` user; ARM images from GitHub Actions → ghcr.io/eldigh/* with `AutoUpdate=registry` + auto-update timer; idempotent `deploy/deploy.sh` (secrets generated once); udev symlink rule; `backup-db.sh`. Grafana: provisioned datasource + committed dashboard, anonymous viewer, port 4000.
- ✅ **Dev workflow**: `justfile` + `compose.dev.yml` — `just api`/`just gateway` auto-start deps, `just db-seed` (~90 days fake data), `just grafana-pull` (dashboard → repo), `just dev` (zellij), `just fmt` (both workspaces). Two path gotchas: (a) the `.env.default` files live in `api/`+`gateway/` and `dotenvy` only searches the CWD *upward*, so the run recipes carry `[working-directory('api'|'gateway')]` — invoking `cargo run -p homescope-api` from the repo root fails with `DB_USER must be set`; the `*-watch` recipes' watchexec paths are correspondingly `-w . -w ../common`. `api/.env` (sqlx `DATABASE_URL`, compile-time) stays where it is. (b) `cargo fmt --all` covers workspace *members* only, and `common` is a path dependency of the firmware workspace, not a member — hence `just fmt`.
- ⏳ **S=8 forcing** (raw-HCI `LeSetExtAdvParamsV2` on the sensor) — next firmware task, worth +4-5 dB.
- ✅ **Hardware migration, stage 1 (2026-07-03)**: 2× Raytac MDBT50Q-DB-40 in hand, whole-house survey **passed** (worst spot ≥85 % delivery after minor repositioning) → **MDBT50Q-1MV2 validated as the production module**. Remaining: custom PCB (MDBT50Q module, VDDH + gated sensor-rail LDO power topology — see Key facts).
- ⏳ **Planned backend/deploy work** (each has a worked-out NOTES-*.md at repo root): API graceful shutdown; ingest DB-error handling → manual MQTT acks + per-device seq check (replay/dedup/idempotency, one mechanism — note the `UNIQUE (device_id, seq, time)` constraint added 2026-07-28 already covers MQTT *redelivery*, but not multi-receiver dedup, since two receivers stamp different `received_at`); `devices.site`/`room` columns + gateway `SITE` topic prefix (`homescope/<site>/sensors/...`, one PR); mosquitto password_file + per-gateway ACLs (before the two-house VPN rollout); udev-driven gateway activation (`TAG+="systemd"` + `BindsTo=`); real VID/PID (pid.codes) replacing embassy's `0xc0de:0xcafe`.
- 🔶 **Packet redesign + crypto block** (settled 2026-07-16, see `NOTES-packet-tv-aead.md`; owner's order): ① ✅ DeviceAddr refactor → ② 🔶 TV measurement encoding — ✅ `common` registry + sensor encode + receiver forwarding (2026-07-25); ✅ **gateway migration** (2026-07-26: `frame::parse` decoder loop + `ObservationEnvelope` MQTT publish); ✅ **API TV decode** (2026-07-28: base64 → `SensorPacket::parse` → `Measurements` walk; metric columns went nullable in the same release — the earlier "defer nullability to the BMP581 node" plan was reversed once the reject-vs-store rule was settled, since partial data is the point of TV) → ③ ✅ seq persistence on the sensor (flash checkpoint w/ jump-ahead, `seq_counter.rs`; AEAD-nonce prerequisite done) → ④ ⏳ **v0.6 magic + version header** (see the wire-protocol section below) → ⑤ ⏳ ChaCha20-Poly1305 AEAD (decrypt-in-API reaffirmed; keyless gateways; AAD = `device_addr` + `ver` + `seq`, magic excluded — it's constant and stripped at the receiver; integrations later get plaintext via API republish of verified readings, not via gateway decryption).
- ⏳ **Provisioning** (settled 2026-07-20, see `NOTES-provisioning.md`): per-device AEAD key lives in **UICR** (`CUSTOMER[0..7]`), written once by a new `homescope-provision` workstation CLI (probe-rs reads FICR `DEVICEADDR` off a *blank* chip → `POST /devices` → API generates + returns the key once → UICR write + verify → flash). One firmware binary for the whole fleet — supersedes the build-time `DEVICE_KEY` env/`link_section` plan. Admin-token-authenticated issuance endpoint; device keys **envelope-encrypted at rest** (KEK in API secrets) — do this before deployment, backups otherwise leak the fleet.
- ⏳ **Remaining firmware work**: watchdog (soak-test prerequisite), BMP581 + LTR390 drivers, sleep optimization, HTTP API.

## Build & flash

### Primary path: probe-rs + VSCode debugger

The standard workflow uses **probe-rs** with a SWD probe (e.g., Pi Pico DAPLink) for both flashing and debugging. VSCode launch configs in `.vscode/launch.json` provide one-click flash + run + RTT log capture for both sensor and receiver firmware. See **"Debug nrf52840-* (debug build)"** launches.

The `firmware/.cargo/config.toml` sets `runner = "probe-rs run --chip nRF52840_xxAA"`, so `cargo run` from inside any firmware crate also flashes via probe and streams defmt-RTT logs.

### Board selection

**Neither firmware crate has a default board feature** (removed 2026-07; commit 63df8e9) — every build/check/clippy invocation must pass exactly one of `--features board-db40` / `--features board-xiao`, or `board/build.rs` fails with "no board feature selected". Don't enable both (trips the `compile_error!` guard):

```bash
cargo run --release --features board-db40   # DB-40 eval board
cargo run --release --features board-xiao   # XIAO (UF2 layout)
```

The board feature selects the `board!` macro arm *and* the generated linker script (app at `0x0` for DB-40, `0x27000` above the UF2 bootloader for XIAO). ⚠️ Flashing a DB-40-linked image to a XIAO over probe-rs erases the XIAO's MBR/SoftDevice/UF2 bootloader. ⚠️ `flash_uf2.sh` runs bare `cargo build --release` — it now fails at build.rs unless you add the XIAO feature flag to it.

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

- **Boards**: Raytac **MDBT50Q-DB-40** eval boards are primary since 2026-07-03 (house survey passed) → custom PCB with the validated MDBT50Q-1MV2 module next. Seeed XIAO nRF52840 **Plus** stays as a bench mule and alkaline soak-test node (retired from RF duty — chip antenna ~10 dB short: −67 dBm @ 1 m @ +8 dBm). The XIAO-specific flash layout below applies to XIAO units only.
- **DB-40 board facts**: no factory bootloader or SoftDevice — application links at `0x00000000` (own `memory.x`, full 1 MB); LEDs LED1/2/3 = P0.13/14/15 (XIAO LED was P0.30), buttons P0.11/12/24/25; SWD via the 1.27 mm Cortex debug header (J1; a 1.27→2.54 adapter bridges to the Pico probe); mini-USB is the nRF52840's own USBD (receiver firmware works as-is). Optional: flash the Adafruit UF2 bootloader (supported target) to restore drag-drop updates — that moves the app base, adjust `memory.x` accordingly.
- **Target**: `thumbv7em-none-eabi` (Cortex-M4F on nRF52840)
- **Bootloader**: Adafruit UF2 v0.9.2 **with Nordic SoftDevice S140 7.3.0 pre-installed** (Board-ID: `nRF52840-SeeedXiao-v1`)
- **Flash layout** (1 MB total):
  - `0x00000000–0x00000FFF`: Nordic MBR (4 KB)
  - `0x00001000–0x00026FFF`: SoftDevice S140 7.3.0 (152 KB, **never started** by our firmware — we use `nrf-sdc` instead, S140 just sits inert in flash)
  - `0x00027000+`: Application (868 KB available)
- **UF2 family ID**: `0xADA52840` (Adafruit nRF52 series)
- **Application base address**: `0x00027000` — set in each `firmware/*/memory.x` and in the `--base` arg of `tools/uf2/uf2conv.py`
- **Power (sensor, revised 2026-07-10)**: dev boards: 2× AA → `3V3` pin direct (L91 lithium for deployment-representative tests; plain alkaline for bench soak — ~3.2 V fresh, safely inside limits; **never battery + USB together** — the on-board LDO back-feeds/trickle-charges the pack). Custom PCB: battery → **VDDH** (internal REG0 buck, `REGOUT0 = 3.0 V`, **powers the nRF only** — kills the fresh-L91 ~3.6 V absolute-max edge) **plus a separate enable-gated LDO** off the battery rail powering all peripherals (SHT45/BMP581/LTR390 + I²C pull-ups). A GPIO drives the LDO EN (supersedes the dev-board GPIO-as-power-rail trick): true zero sensor sleep draw, clean regulated supply, and headroom for the SHT45 heater's ~75 mA pulses without touching the MCU rail. Battery ADC follows the rail the battery is on: `VddInput`/ratio 1 on dev boards, `VddhDiv5Input`/ratio 5 on the custom PCB. See [docs/architecture.md](docs/architecture.md#power).
- **Power (receiver)**: USB bus power from the Pi. Plug-and-play.
- **Sensors (decided 2026-05, revised 2026-07)**: all battery nodes use **SHT45** (T/H, ±0.1 °C / ±1 % RH; its on-die heater — for condensation recovery, mainly outdoors — is freely usable on the custom PCB's gated LDO rail as of 2026-07-10; the "heater" objection below was about the BME688 *gas* heater). **Pressure (BMP581) on exactly one designated *indoor* node** — pressure is house-wide and indoor ≈ outdoor, so the barometer gets friendly conditions and the gateway stays a pure bridge (supersedes the earlier BMP390-on-outdoor-node plan; BMP581 = newer part, async `bmp5` Rust driver). Outdoor node: SHT45 + optional **LTR390** (light/UV). **BME688 / air quality dropped from the battery fleet** (raw gas ≠ IAQ without BSEC, and the gas heater self-heats T/H); IAQ deferred to an optional USB/mains-powered BME68x + BSEC node. Node variants are one codebase behind Cargo features. See [docs/architecture.md](docs/architecture.md#sensors).
- **BLE/SDC gotchas (hard-won 2026-07)**: SDC features are build-time opt-ins (`support_ext_adv`, `support_le_coded_phy`, `support_ext_scan`); ext adv / coded PHY / ext scan exist **only in the multirole SDC library** (enable both `peripheral` + `central` cargo features on nrf-sdc); TX power only via `default_tx_power()`; trouble's `advertise()` is legacy-only (use `advertise_ext`); `panic-probe` needs the `print-defmt` feature or panics are silent halts. Full list: [docs/architecture.md — field findings](docs/architecture.md#field-findings--rf-debugging-2026-07).
- **TWIM gotcha (2026-07-29)**: `embassy-nrf` 0.10's async TWIM is **not cancel-safe** — `Twim::transaction` starts EasyDMA then awaits a bare `poll_fn` with no `OnDrop` guard (the only `Drop` belongs to the `Twim` struct, which we never drop). So `with_timeout` around an I²C transfer, on firing, leaves the peripheral running, events uncleared, and DMA writing into the dropped future's stack frame; the *next* transaction then sees a stale `EVENTS_STOPPED` and returns instantly. Signature: the first timeout looks like a one-off, then everything fails forever. `sensors/sht4x.rs` uses exactly this pattern and has no recovery path. Separate the two questions when debugging — *why did it hang once* is wiring/power (stuck SCL/SDA, pull-ups on a gated rail), *why does it never recover* is this.
- **Probe**: SWD probe (Pi Pico DAPLink or similar) wired and working. Enables defmt-RTT log capture and breakpoint debugging via the VSCode probe-rs-debugger extension.
- **Logging**: `defmt-rtt`. Logs visible in the VSCode Debug Console during a debug session.

## BLE design summary

- **Advertising mode**: non-connectable, non-scannable, undirected **extended advertising** (`ExtNonconnectableNonscannableUndirected` via `Peripheral::advertise_ext`)
- **PHY**: Coded PHY (primary + secondary). Coding is currently the SDC default; forcing **S=8** (−103 dBm sensitivity) via `LeSetExtAdvParamsV2` is the next firmware task
- **TX power**: +8 dBm via nrf-sdc `Builder::default_tx_power(8)` — the per-set HCI request field is ignored by the SDC
- **Interval/burst**: 20 ms × ~20 events (~400 ms); AUX payloads channel-hop per event, so a burst doubles as frequency diversity (per-packet RSSI swings ±10-15 dB indoors — judge medians)
- **Burst cadence**: ~0.5 s during benchmarking; production target is 1–5 min with System OFF sleep between bursts
- **Payload**: `ManufacturerSpecificData` with company ID `0xFFFF` (testing; treat as shared airspace — never trust its contents) carrying the **TV measurement encoding** — `[seq: u32][id][value]…` with the measurement-ID registry in `common` (ID = semantics + repr + scale; ID implies length; unknown/truncated/duplicate/**empty** ⇒ drop whole packet, but a *missing* metric is fine and stores as NULL — "reject when unusable or untrustworthy, otherwise store what arrived"); `MAX_WIRE_LEN` 252; device identity = AdvA, not a payload field. v0.6 prepends `[magic b"HM"][ver: u8]` (see below). Extended adv gives 254 B headroom (the planned AEAD tag never fit legacy's 31 B).
- **Security (planned)**: ChaCha20-Poly1305 AEAD with per-device keys (registry = `devices` table, encrypted at rest under a KEK; decrypt in the API — gateways keyless; sensor-side key in UICR, see `NOTES-provisioning.md`); TV section encrypted, `device_addr`+`ver`+`seq` as AAD (magic excluded — constant and stripped at the receiver); nonce derived from the **persisted** seq counter (retained RAM + flash checkpoint on the sensor); the API's per-device seq monotonicity check = replay protection + multi-receiver dedup + MQTT-redelivery idempotency. Not implemented yet — see `NOTES-packet-tv-aead.md`.

## USB-CDC wire protocol (receiver → gateway)

See [docs/protocol.md](docs/protocol.md) for the full spec (v0.5, 2026-07-25). Quick summary:

- Content-agnostic variable-length frame (6+N bytes): `[magic "HS"][len: u16 LE, cap 1024][payload: N][crc: u16 LE over len+payload]`. Payload = `SensorObservation`: 11-byte header (`device_addr` from AdvA, `age_ms` u32 LE, `rssi` i8) then the air packet = all remaining bytes (no observation-level length — the frame delimits). Observation payload ≤ 263; 30-byte frames with today's three measurements.
- Air packet: `[seq: u32 LE][id][value]…` — the TV encoding; the receiver reads only `seq` (dedup), everything downstream of the sensor is semantics-blind until the API.
- ⏳ **v0.6 (designed 2026-07-29, not implemented)**: air packet becomes `[magic b"HM"][ver: u8][seq: u32 LE][TV…]`. Magic is **air-side only** — the receiver checks it, drops non-matching `0xFFFF` traffic, and **strips** it, so `SensorPacket` downstream is `[ver][seq][TV…]`. Declared as `[u8; 2]`, not `u16` (no endianness question). `ver` goes **before** `seq` (a version field must be readable before the fields ahead of it, or locating it requires already knowing `seq`'s width). The **receiver never reads `ver`** — magic is constant so filtering on it never forces a dongle reflash, and a node on stale firmware must stay visible rather than silently vanish. API dispatches on `ver`: one parse fn per version, all producing `SensorReading`; today one arm + a catch-all `UnsupportedVersion`. Do this **before** AEAD (one node in the field = cheapest migration).
- Downstream of the gateway, observations travel as the opaque `ObservationEnvelope` JSON (cleartext `deviceAddr`/`rssi`/`receivedAt` + base64 `packet` blob, serde camelCase) on `homescope/sensors/<device-addr>/envelope`; planned topic shape adds a per-gateway site prefix (`homescope/<site>/sensors/...`). The API consumes this end to end as of 2026-07-28. Post-AEAD, the API republishes decoded readings on a `…/state` topic for integrations.

## Where to find things

- [README.md](README.md) — top-level overview, pointers into the crates
- [docs/architecture.md](docs/architecture.md) — full design rationale: protocol choice, sensor selection, power topology, security model, BLE vs ESB tradeoff, API/deployment architecture
- [docs/protocol.md](docs/protocol.md) — USB-CDC wire protocol between receiver and gateway (v0.5, variable-length TV frames)
- [docs/flashing.md](docs/flashing.md) — UF2 build & flash workflow (XIAO-only), mount setup, troubleshooting
- `deploy/deploy.sh` header comment — the deployment model (two-phase, idempotent, ownership rules; **never chown into `~/.local/share/containers`**)
- `NOTES-*.md` (repo root, untracked) — settled plans for upcoming backend/deploy tasks; read before working on those topics
- `~/.claude/plans/let-s-analyze-that-my-glowing-peacock.md` — original full design exploration (lives in Claude's plan store, not committed)
