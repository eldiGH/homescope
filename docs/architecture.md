# Homescope — Architecture

## Goal

Battery-powered ambient sensors that report temperature, humidity, pressure (and air quality, internal only) every 1-5 minutes. Optimized first for **battery longevity** (years on AA cells), then for **reliability** and **cost**. Two physical variants: external (outdoor) and internal (with air quality).

The full stack: BLE sensors → Raspberry Pi gateway (Rust + bluer + MQTT publish) → Mosquitto broker → Rust API → TimescaleDB → Grafana. All Pi-side services run as Podman containers managed by systemd quadlets.

## Non-goals

- Mains-powered operation
- Sub-minute reporting (HVAC fast control)
- Smart-home ecosystem compatibility (HomeKit/Matter/Google Home) — see [Future: Matter](#future-matter)

---

## Hardware platform

### MCU: Seeed XIAO nRF52840 Plus

- ARM Cortex-M4F @ 64 MHz, 1 MB flash, 256 KB RAM
- Built-in 2.4 GHz radio: BLE 5.x + IEEE 802.15.4
- USB-C, on-board UF2 bootloader (Adafruit v0.9.2), LiPo charger circuit
- System OFF sleep current: ~0.4 µA (chip), ~1.5-2 µA (XIAO board total)
- Used for **both development and production** at this scale — bare modules (Raytac MDBT50Q etc.) only pay off at hundreds of units

**Flash layout note**: the Plus variant ships with Nordic SoftDevice S140 7.3.0 pre-installed (`0x1000–0x26FFF`, 152 KB). Application must start at `0x27000`, not `0x26000`. The SoftDevice is never started by our firmware — we use `nrf-sdc` directly — but it occupies the flash region. See [docs/flashing.md](flashing.md#critical-this-board-ships-with-softdevice-s140-installed).

### Sensors

| Variant | Sensors | Why |
|---|---|---|
| **External** | SHT41 (T/H) + BMP390 (P) | Best-in-class accuracy, lowest power. Both I²C; ~0.4 µA + 3.4 µA sleep. |
| **Internal** | BME688 (T/H/P + VOC) | Adds indoor air quality. Gas heater dominates power; run gas measurement only every 5-10 min. |

All sensors share the same I²C bus on the XIAO. Distinct addresses (SHT41 `0x44`, BMP390 `0x76`, BME688 `0x77`) → no conflicts.

### Power

- **Cells**: 2× AA **Energizer Lithium L91** (3.0 V, ~3000 mAh, -40 to +60 °C, 15-year shelf life)
- **Topology**: AA pair → directly to XIAO `3V3` pin (bypasses on-board LDO; nRF52840 operates 1.7-3.6 V; SHT41/BMP390/BME688 all operate at 1.7-3.6 V)
- **Required passives**:
  - 10 µF ceramic + 100 nF ceramic across 3V3/GND near the MCU (transient decoupling)
  - 22-100 µF (tantalum or low-ESR electrolytic) across battery terminals (buffers ~5 mA radio TX bursts against rising internal resistance of aging cells)
- **No TPL5111** nanopower timer needed — nRF52840's System OFF (0.4 µA) is already at the practical floor; the TPL5111's 35 nA advantage adds reset/state-restore complexity for negligible gain
- **Expected battery life**: 5-10+ years at 1-5 min reporting (dominated by self-discharge, not active draw)

---

## Wireless stack

### Choice: BLE 5.0 advertising (broadcast / beacon mode)

Selected over Thread and ZigBee because:

1. **Lowest energy per cycle for sleep-mostly leaves** — no parent-polling overhead (Thread/ZigBee Sleepy End Devices must wake to poll their parent router on every cycle)
2. **Best Rust ecosystem** — `trouble` (pure-Rust BLE host) is mature in 2026 and integrates cleanly with Embassy
3. **Mesh provides no value here** — all devices are sleepy leaves with no router peers, so Thread/ZigBee mesh benefit is theoretical only
4. **Single radio for both protocols** — if we ever need Thread later, same XIAO hardware (different firmware)

### Stack details

- **PHY**: BLE 5.0 **Coded PHY S=2** by default (~4× range vs 1M PHY at 2× airtime cost). Drop to **1M PHY** for short-range/dense deployments.
- **Mode**: non-connectable, non-scannable undirected advertising (`ADV_NONCONN_IND`). Sensors never accept connections during normal operation — saves power, eliminates connection-state attack surface.
- **Advertising burst per cycle**: 3 advertising events × 3 channels (37, 38, 39) = 9 transmissions. Total radio time ~60 ms.
- **Manufacturer-specific data field** carries the encrypted payload (max 31 bytes legacy adv, more with extended adv if needed).

### Reliability

| Configuration | Expected delivery rate (indoor) |
|---|---|
| Default (3-event burst, 1 gateway) | ~99 % |
| 5-event burst | ~99.5 % |
| 3-event + 2 gateways (independent capture) | ~99.99 % |
| 3-event + buffer-and-dump connected mode every 10 min | ~99.95 % |

**Ship default config (3-event burst)**. Add gateways or buffer-dump only if observed gap rate proves intolerable. For ambient sensing, occasional missed readings appear as small gaps in charts, not data loss.

### Why not Enhanced ShockBurst?

ESB is Nordic's proprietary 2.4 GHz protocol — same band/MCU as BLE, but a different link layer with hardware-level auto-ACK and auto-retransmit. Worth a serious second look since our dedicated nRF52840 receiver removes the usual deal-breaker (Pi BLE radios can't speak ESB).

Tradeoffs that informed the decision to stay on BLE for now:

| Axis | BLE adv (current) | ESB |
|---|---|---|
| Range | Coded PHY S=2 gives ~4× over 1M PHY | Equivalent at matched bitrates (250 kbps slightly exceeds S=2) |
| Radio energy per cycle | ~3.5-4 ms radio time (3 events × 3 channels) | ~0.5-0.7 ms happy path (1 tx + ACK) — ~6× lower |
| Practical battery-life delta at 5-min cadence | self-discharge-dominated (per architecture above) | same — savings disappear into self-discharge |
| Reliability | 3-channel diversity, no ACK (~99 % indoor) | 1-channel + hardware ACK + retries (~99.9 % if channel is clear) |
| Failure mode | resilient to single-channel interference | brittle on a single channel under persistent interference unless SW channel hopping is added |
| Rust / Embassy support | `trouble-host` + `nrf-sdc`, mature, Embassy-native | `esb` crate works but isn't Embassy-native; you write async glue |
| Flash footprint | ~100-160 KB | ~5-15 KB (irrelevant at our headroom) |
| Debugging | nRF Connect on any phone shows packets | needs another nRF chip in promiscuous mode + custom tooling |
| Vendor lock-in | open spec, multi-vendor | Nordic silicon only (covers nRF52840 / nRF52833 / nRF54L15) |
| Effort to migrate | n/a (already working) | rewrite firmware radio layer + dongle firmware + Pi-side parser |

**Net**: ESB's structural advantages (lower radio energy, hardware ACK) don't translate into outcomes we'd measure at our cadence and reliability target. The case for switching from a working BLE setup is weak; the case for prototyping it later as a learning exercise or to address a measured deployment issue is reasonable.

**When to reconsider:**

- Measured delivery rate stays <95 % in deployment and the only remaining mitigation is hardware ACK + retry
- Topology changes such that the receiver also wants to sleep (e.g., battery-powered relay nodes)
- Latency-sensitive use case needing sub-100 ms wake → TX → confirm cycles (ESB stack startup is leaner than BLE's)
- Sustained interest in learning the lower-level Nordic radio stack (valid project goal, just not a migration trigger)

---

## Security

### Threat model

- **Confidentiality**: nobody reads our sensor values
- **Integrity**: nobody can forge plausible-looking readings
- **Replay**: nobody can capture an advertisement and re-broadcast it later

### Mechanism: ChaCha20-Poly1305 AEAD with per-device keys

Each sensor has a **unique 32-byte ChaCha20-Poly1305 key** baked into firmware at flash time. Per-device (not network-wide) so extracting one device's firmware does not compromise the rest.

### Payload layout

```
+--------------+--------------+----------------+----------------------+
| device_id    | seq counter  | nonce (lower)  | AEAD ciphertext+tag  |
| 1 byte       | 4 bytes      | 4 bytes        | N + 16 bytes         |
+--------------+--------------+----------------+----------------------+
   ^plaintext^   ^plaintext^   ^plaintext^      ^encrypted+authenticated^
```

- `device_id` (1 B, plaintext) — gateway uses this to look up the right per-device key
- `seq_counter` (4 B, plaintext) — monotonic, increments every advertisement, never repeats over device lifetime. Used as the upper 32 bits of the AEAD nonce → **automatic replay protection** (gateway tracks last-seen counter per device, rejects ≤)
- `nonce_lower` (4 B, plaintext) — random per-message, completes the 96-bit ChaCha20 nonce
- AEAD ciphertext encodes: temperature, humidity, pressure, battery voltage, [gas reading for internal variant]
- 16 B Poly1305 authentication tag detects any tampering

### Key & ID provisioning

Embedded at compile time via build script reading env vars:

```bash
DEVICE_ID=3 DEVICE_KEY=<hex> cargo build --release
```

The build script writes them into a `link_section` placed at a known flash page so that future OTA updates can preserve identity. (Alternative: separate JSON config flashed to dedicated page; deferred until needed.)

### What we explicitly skip

- **BLE pairing / LE Secure Connections**: requires connectable mode (power cost) and a bonding store. Our beacon-only mode means BLE-native security doesn't apply — the payload-level AEAD does the same job at lower cost.
- **Network-wide key**: rejected because firmware extraction from any single device would compromise everyone.

---

## Gateway & API integration

```
+------------+  BLE adv  +-----------+  USB-CDC  +-----------+  MQTT pub  +-----------+  sub  +-----------+
| Sensor 1   | --------> |           |           |           |            |           |       |           |
+------------+           | nRF52840  | --------> |  Pi GW    |   ------>  | Mosquitto | ----> |  User API |
+------------+  BLE adv  | receiver  |           |  (Rust)   |            |  broker   |       |           |
| Sensor 2   | --------> |  dongle   |           |           |            |           |       |           |
+------------+           |           |           |           |            |           |       |           |
+------------+  BLE adv  |           |           |           |            |           |       |           |
| Sensor 3   | --------> |           |           |           |            |           |       |           |
+------------+           +-----------+           +-----------+            +-----------+       +-----------+
```

A dedicated **nRF52840 dongle** runs BLE scanning firmware and exposes received advertisements to the Pi over **USB-CDC** (USB serial). This is the architecture from day one — a Pi-direct BlueZ approach was prototyped and dropped early after measuring a significant packet loss rate on advertisements: the Pi's host-side BLE stack drops adverts under load and is unreliable as a scanner for low-duty-cycle beacons. A dedicated radio with deterministic firmware is far more dependable and decouples scanning availability from Pi load.

### Receiver firmware responsibilities

1. **Scan**: continuously listen for our manufacturer-ID advertisements (Coded PHY S=2 by default)
2. **Forward**: emit framed packets to the Pi over USB-CDC (magic + payload + CRC; see [protocol.md](protocol.md))

### Pi gateway responsibilities

1. **Read**: parse framed packets from the receiver dongle (`/dev/ttyACM0` or a udev symlink) using `serial2-tokio` + `tokio_util::codec::Decoder` over `BytesMut`
2. **Authenticate**: lookup `device_id` → per-device key; verify AEAD tag and check sequence counter > last-seen (planned — not yet implemented)
3. **Decrypt**: extract sensor readings from the AEAD ciphertext (planned)
4. **Publish**: emit JSON to MQTT topics like `sensors/external/garden/tph` and `sensors/internal/living/aqi` (planned)

### MQTT broker (Mosquitto, local on the Pi)

- Topic hierarchy: `sensors/<variant>/<location>/<measurement>`
- Persistence enabled — survives API downtime, replays on reconnect
- Multiple consumers can subscribe independently (API, Home Assistant, Grafana, etc.)

### Why MQTT over direct HTTP POST

- **Decouples** the radio-side ingest from API availability — API outage doesn't lose data
- **Trivially multi-consumer** without changing the gateway
- **Industry-standard** for IoT, well-supported tooling
- Cost: ~10 MB RAM for Mosquitto on the Pi; negligible

---

## Software updates

| Stage | Mechanism | When |
|---|---|---|
| **v1** | UF2 reflash via physical double-tap-reset | Initial deployment, development iteration |
| **v2** | BLE DFU via the Adafruit bootloader, triggered by double-tap-reset, updated via nRF Connect mobile app | Once enclosures are sealed and physical USB access is awkward |
| **v3** (optional) | Always-on OTA: sensor wakes every ~6 h to listen briefly for incoming connection | Only if frequent updates are needed and the ~1 % battery overhead is acceptable |

The Adafruit UF2 bootloader on the XIAO supports BLE DFU out of the box; no extra firmware needed to enable v2.

---

## Project structure

Monorepo with **two separate Cargo workspaces** split by target architecture:

```text
homescope/
├── Cargo.toml             # host-target workspace: gateway, common
├── common/                # shared types (`homescope-common`, no_std-by-default)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── packet.rs      # SensorPacket (repr(C, packed)), framing, CRC
│       └── reading.rs     # SensorReading (serde, human units)
├── gateway/               # Pi-side receiver-decoder + (future) MQTT publisher
│   ├── Cargo.toml         # `homescope-gateway`
│   └── src/main.rs
├── firmware/
│   ├── Cargo.toml         # firmware workspace: sensor, receiver
│   ├── rust-toolchain.toml
│   ├── .cargo/config.toml # cross-compile target (`thumbv7em-none-eabi`)
│   ├── sensor/            # `homescope-sensor` — BLE-advertising sensor firmware
│   │   ├── Cargo.toml
│   │   ├── build.rs
│   │   ├── memory.x       # FLASH @ 0x27000, RAM @ 0x20000000 (256 K)
│   │   ├── flash_uf2.sh   # UF2 backup flow (calls tools/uf2/uf2conv.py)
│   │   └── src/
│   │       ├── main.rs
│   │       └── ble_advertise.rs
│   └── receiver/          # `homescope-receiver` — USB-CDC BLE-scanning dongle
│       ├── Cargo.toml
│       ├── build.rs
│       ├── memory.x
│       ├── flash_uf2.sh
│       └── src/
│           ├── main.rs
│           └── ble_scan.rs
├── tools/
│   └── uf2/               # vendored microsoft/uf2 tooling (MIT) — used by both flash_uf2.sh
│       ├── uf2conv.py
│       ├── uf2families.json
│       ├── LICENSE
│       └── README.md
├── api/                   # HTTP API + MQTT subscriber + TimescaleDB (planned)
├── deploy/                # Podman quadlets + k8s pod YAML (planned)
└── docs/
    ├── architecture.md
    ├── flashing.md
    └── protocol.md
```

Rationale:

- **Two workspaces, not one.** A single workspace mixing `thumbv7em-none-eabi` firmware with host-target gateway breaks rust-analyzer — it picks one default target and the other side errors out. Splitting at the `firmware/` boundary lets each IDE session resolve a consistent target. The `common` crate is referenced from both workspaces via `path = "../common"` so the type definitions stay deduplicated.
- **`firmware/sensor/` and `firmware/receiver/` named by role, not chip.** Both currently target nRF52840; the role is what distinguishes them. If/when a different chip family enters the stack (`firmware/sensor-esp32/` etc.), the suffix grows from the role.
- **`common` crate** avoids duplicating `SensorPacket` between firmware (encoder) and gateway (decoder). `no_std` by default with optional `packet` and `reading` features. Frame layout (magic + payload + CRC), CRC algorithm, and parse/build logic all live here — both ends use `SensorPacket::write_frame` / `SensorPacket::parse_frame`.
- **Two structs in `common`**: `SensorPacket` (`repr(C, packed)`, wire format) and `SensorReading` (normal layout, serde-derived, human units). Conversion via `From<SensorPacket> for SensorReading` on the gateway side. **Don't combine into one struct** — serde on a packed struct generates unaligned-reference code (undefined behaviour).
- **`.cargo/config.toml` lives at `firmware/.cargo/`**, not at repo root — it sets the cross-compile target only for firmware workspace members.
- **Future internal/external sensor variants**: separate binaries inside `firmware/sensor/` (e.g. `src/bin/external.rs`, `src/bin/internal.rs`) sharing helpers via a library module — both share the same `memory.x` and bootloader offset.

---

## Implementation roadmap

1. ✅ **Sensor firmware skeleton** — Embassy + nrf-sdc + trouble-host. BLE advertising works end-to-end, visible in nRF Connect.
2. ✅ **Repo restructure** — `firmware/sensor/`, `firmware/receiver/`, `gateway/`, `common/` established. Two Cargo workspaces split by target.
3. ✅ **`common` crate** — `SensorPacket` (wire format, `repr(C, packed)`), `SensorReading` (app type, serde), framing (`write_frame`/`parse_frame`), CRC-16/IBM-SDLC. Shared by all crates.
4. ✅ **Receiver dongle firmware** — `firmware/receiver/`. Scans for our manufacturer-ID advertisements (Coded PHY S=2), forwards framed packets over USB-CDC. Robust to host disconnect/reconnect (DTR-aware writes, drop-oldest backlog).
5. ✅ **Gateway v1 receiver path** — `gateway/` reads `/dev/ttyACM0` via `serial2-tokio` + `tokio_util::codec::Decoder`, validates magic + CRC, decodes `SensorPacket`, converts to `SensorReading`. No MQTT yet — just prints.
6. ⏳ **Gateway v1 MQTT publish** — `rumqttc` client, JSON encoding, topics like `sensors/external/garden/tph`.
7. ⏳ **API v1** — Rust + `rumqttc` subscribes to sensor topics, validates, stores into TimescaleDB hypertables. Plain Postgres metadata tables for device registry.
8. ⏳ **Grafana** — TimescaleDB datasource, per-sensor dashboards, kiosk mode. Replaces the custom Svelte frontend from the previous project.
9. ⏳ **Containerization** — Podman quadlets composing mosquitto + gateway + api + grafana. `.kube` unit files referencing a Kubernetes Pod YAML.
10. ⏳ **Sensor drivers** — SHT41 + BMP390 over I²C using `sht4x` and `bmp390` crates; verify readings via defmt-rtt logging (probe required).
11. ⏳ **Payload + crypto** — define `SensorPacket` payload with ChaCha20-Poly1305 AEAD wrapper, end-to-end encrypt + broadcast + decrypt on gateway.
12. ⏳ **Sleep & power optimization** — replace per-cycle 5 s timer with System OFF + RTC wakeup. Gate I²C bus power during sleep. Measure draw with PPK2.
13. ⏳ **Internal variant** — BME688 driver, gas measurement scheduling (every 5-10 min, not every cycle).
14. ⏳ **Provisioning** — build-time `DEVICE_ID` + `DEVICE_KEY` injection via env vars + build.rs.

---

## Future: Matter

**Not pursued now.** Matter (Apple/Google/Amazon smart-home standard) runs over Thread or Wi-Fi and adds value only if we want our sensors discoverable by mainstream smart-home hubs. We have our own API, so Matter would be added complexity without payoff.

If we ever want Home Assistant integration, the simpler path is **MQTT discovery** — Home Assistant auto-discovers MQTT-published sensors via the `homeassistant/` topic convention. No firmware changes needed.
