# Homescope — Architecture

## Goal

Battery-powered ambient sensors reporting every 1-5 minutes, optimized first for **battery longevity** (years on AA cells), then for **reliability** and **cost**. The fleet is split by **role**: many **indoor room nodes** report temperature & humidity (the values that actually vary room-to-room), and a single **outdoor node** additionally reports barometric pressure (house-wide, so one unit suffices) and optionally light/UV. Indoor air quality is deferred to an optional, separately-powered node — see [Sensors](#sensors).

The full stack: BLE sensors → nRF52840 receiver dongle → Raspberry Pi gateway (Rust: USB-CDC decode + MQTT publish) → Mosquitto broker → Rust API → TimescaleDB → Grafana. All Pi-side services run as Podman containers managed by systemd quadlets.

## Non-goals

- Mains-powered operation *for the battery sensor nodes* (an optional future air-quality node may be USB/mains-powered — see [Sensors](#sensors))
- Sub-minute reporting (HVAC fast control)
- Smart-home ecosystem compatibility (HomeKit/Matter/Google Home) — see [Future: Matter](#future-matter)

---

## Hardware platform

### MCU: nRF52840

- ARM Cortex-M4F @ 64 MHz, 1 MB flash, 256 KB RAM
- Built-in 2.4 GHz radio: BLE 5.x + IEEE 802.15.4
- System OFF sleep current: ~0.4 µA (chip)

Re-affirmed 2026-07 after a full architecture review: battery life is shelf-life-dominated at our cadence on any modern Nordic part, so silicon choice comes down to ecosystem maturity — embassy-nrf, nrf-sdc, trouble, and probe-rs are all mature on the nRF52840. The newer nRF54L generation offers nothing we'd measure here and costs toolchain churn; revisit at a future PCB revision.

**Board strategy (revised 2026-07):**

- **Seeed XIAO nRF52840 Plus** — the original dev-and-production board, now **retired from RF duty**: its chip antenna measured ~10 dB below a proper module (−67 dBm @ 1 m @ +8 dBm against a phone referee, identical on two units; ~0 % delivery through 2 concrete walls at 15 m). Still useful as bench mules for non-RF work. USB-C, UF2 bootloader (Adafruit v0.9.2), LiPo charger.
- **Raytac MDBT50Q-DB-40 eval boards** — in hand (2×) and **survey-passed 2026-07-03**: whole-house coverage, worst-spot delivery ≥85 % after minor repositioning. Board facts: SWD via 1.27 mm Cortex header (J1), no factory bootloader — app links at `0x0`; LEDs P0.13-15, buttons P0.11/12/24/25; mini-USB = nRF USBD.
- **Custom PCB with the Raytac MDBT50Q-1MV2** (chip antenna — no external antenna) — the deployment target, validated by the DB-40 survey: we ship exactly what was tested. Pre-certified (FCC/IC/CE/Telec). Follow the datasheet antenna keep-out (no copper, battery, or enclosure ribs in the zone): this ground-plane discipline is precisely what the XIAO physically couldn't provide. Include a 1.27 mm Cortex debug header.

**XIAO flash layout note** (XIAO boards only): the Plus variant ships with Nordic SoftDevice S140 7.3.0 pre-installed (`0x1000–0x26FFF`, 152 KB). Application must start at `0x27000`, not `0x26000`. The SoftDevice is never started by our firmware — we use `nrf-sdc` directly — but it occupies the flash region. See [docs/flashing.md](flashing.md#critical-this-board-ships-with-softdevice-s140-installed). The custom PCB will carry the open-source Adafruit UF2 bootloader (ported board definition) to keep the sealed-enclosure update flow.

### Sensors

Two node **roles** — they differ in purpose, count, and placement, not just configuration:

| Role | Count | Sensors | Why |
|---|---|---|---|
| **Indoor room node** | many | SHT45 (T/H); **one designated node also carries a BMP581 (P)** | Per-room temperature & humidity — the only values that genuinely vary room-to-room. ±0.1 °C / ±1 % RH, ~80 nA idle. The single barometer rides on one indoor node (see below). |
| **Outdoor node** | one | SHT45 (T/H) [+ optional LTR390 (light/UV)] | Only the quantities that must physically be outdoors. |
| **Air-quality node** *(future, optional)* | 0-1 | BME68x + BSEC, USB/mains-powered | Indoor air quality (VOC/IAQ). Deferred — see rationale below. |

All sensors are I²C on a shared bus, distinct addresses (SHT4x `0x44`, BMP581 `0x46`/`0x47`, LTR390 `0x53`) → no conflicts.

**SHT45 on-die heater** *(unlocked 2026-07-10 by the gated-LDO peripheral rail — see [Power](#power))*: the SHT4x family has a built-in heater for recovering from condensation and long-term high-humidity creep — most relevant on the outdoor node. It pulses up to ~75 mA, far beyond what a GPIO-powered rail could supply, which previously ruled it out; the dedicated LDO rail absorbs it without disturbing the MCU supply, so heater use is now unconstrained firmware policy. Caveat: a heater pulse warms the die — discard T/H readings taken during or immediately after a pulse, and schedule pulses away from the measurement window.

**Pressure lives on one designated *indoor* node — not on every node, not on the outdoor node, and not on the gateway.** Barometric pressure is house-wide: it varies with weather (10-30 hPa swings) and altitude (~0.12 hPa/m), not with which room you're in (floor-to-floor differences are ~0.3 hPa, below sensor noise — and indoor ≈ outdoor, buildings are not pressure vessels). One barometer therefore represents the whole deployment, and since it gains nothing from being outdoors, it shouldn't pay the outdoor risks (condensation into the pressure port, icing, temperature extremes). Hosting it on an indoor battery node keeps the gateway a single-responsibility bridge. *(Revised 2026-07: previously assigned to the outdoor node; moved indoors because placement is physically free and indoor conditions are benign. Sensor upgraded BMP390 → **BMP581** — a generation newer, ~10× lower noise, ~half the active current, with an async Rust driver ([`bmp5`](https://docs.rs/bmp5)) that fits Embassy directly. If a harsh-environment barometer is ever needed, the gel-protected BMP585 is the drop-in sibling.)* On the custom PCB, put the BMP581 footprint on every board and populate it on exactly one.

#### Why not BME688 / air quality on the battery nodes

An earlier revision put a BME688 on indoor nodes for air quality. We dropped it from the battery fleet:

- **Raw gas resistance is not air quality.** The BME68x gas element reports plate resistance that trends inversely with VOCs, but a calibrated IAQ index requires Bosch's **BSEC** — a closed-source binary blob with its own duty-cycle and persistent-calibration-state requirements. Without BSEC you get an uncalibrated, drifting, per-unit-incomparable number.
- **The gas heater self-heats the die**, which corrupts both temperature *and* humidity (a warmed humidity element measures a genuinely lower RH at the hot die — physics, not a driver bug), directly fighting the accuracy goal.
- **Stripped of gas, the BME688 is a mediocre T/H/P part** — ±0.5-1 °C / ±3 % RH versus the SHT45's ±0.1 °C / ±1 % RH, at higher power and with the self-heating baggage.
- **BSEC on a battery node is viable but a real detour.** Its ULP mode runs at ~3-4 µA average (one gas reading per 300 s), but you must persist BSEC state across System OFF and tolerate days-long calibration convergence. Better hosted on a dedicated, mains/USB-powered node where power and persistence are free.

So: dedicated parts (SHT45 + BMP581) for the battery fleet; BME68x + BSEC reserved for an optional, mains-powered IAQ node if/when we want it. This also nets a software win — `sht4x` is embedded-hal **1.0** native and `bmp5` is embedded-hal-**async** native (clean Embassy fit), whereas the `bme680` crate is embedded-hal **0.2** blocking.

#### Temperature accuracy

The SHT45 is factory-calibrated to ±0.1 °C — the sensor is not the limiting factor. Residual error comes from **self-heating and placement**: the die reads its own temperature, which is ambient plus heat from the nearby nRF (especially during radio TX). Mitigations: keep the sensor physically away from the MCU on the PCB, take the reading early after wake before the radio fires, and apply a small **per-device offset** characterized against a reference thermometer **at the production duty cycle** (self-heating depends on cadence, so calibrate at the cadence you'll ship). With no gas heater this is near-trivial — expect an offset well under 1 °C, if any.

### Power

- **Cells**: 2× AA **Energizer Lithium L91** (Li-FeS₂ — not LiFePO₄; 3.0 V nominal as a pair, ~3000 mAh, -40 to +60 °C, 15-year shelf life, and **no leakage**, unlike alkaline — the classic multi-year-deployment killer). Rechargeables rejected: 2-5 %/month self-discharge caps life at ~a year regardless of load. Li-SOCl₂ rejected: 3.6-3.67 V sits at/above the nRF's absolute max and brings passivation quirks.
- **Topology (XIAO dev boards)**: AA pair → directly to XIAO `3V3` pin (bypasses on-board LDO; nRF52840 operates 1.7-3.6 V). Caveat: a fresh L91 pair is ~3.5-3.6 V open-circuit — right at the absolute-max edge; acceptable on the bench, not the final answer. Bench soak nodes run 2× AA **alkaline** the same way (~3.2 V fresh — comfortable margin; the continuously sloping discharge makes `battery_mv` a convenient state-of-charge gauge). **Never battery + USB simultaneously**: the on-board LDO holds 3.3 V and back-feeds/trickle-charges the pack.
- **Topology (custom PCB — the proper fix; revised 2026-07-10)**: AA pair → **VDDH** (the nRF52840's high-voltage input, 2.5-5.5 V, internal REG0 buck) with `REGOUT0 = 3.0 V` feeding **the nRF only**. Eliminates the fresh-pair absolute-max edge entirely; the 2.5 V VDDH floor ≈ 1.25 V/cell, past >95 % of Li-FeS₂ capacity (flat discharge curve, then cliff). The MDBT50Q module exposes VDDH.
- **Peripheral rail (custom PCB)**: a dedicated **LDO with an enable pin**, input on the battery/VDDH node, output powering everything that isn't the nRF — SHT45, BMP581, LTR390, and the I²C pull-ups. A GPIO drives EN, replacing the dev-board trick of powering the sensor from a GPIO. Wins: (1) hard-zero peripheral draw in sleep (no LDO Iq when EN is low); (2) a clean regulated supply → repeatable sensor readings independent of battery sag; (3) current headroom — the SHT45 heater pulses up to ~75 mA, far beyond GPIO drive, and the separate rail keeps those transients off the MCU supply (no brownout risk during radio activity). *(Supersedes the earlier plan of hanging sensors on REG0's output.)* Selection notes: low quiescent current (Iq is a permanent battery load whenever EN is high), and keep the I²C pull-ups on this gated rail so an unpowered bus can't parasitically feed the sensors through their clamp diodes.
- **Required passives**:
  - 10 µF ceramic + 100 nF ceramic across 3V3/GND near the MCU (transient decoupling)
  - 22-100 µF (tantalum or low-ESR electrolytic) across battery terminals (buffers ~5 mA radio TX bursts against rising internal resistance of aging cells)
- **No TPL5111** nanopower timer needed — nRF52840's System OFF (0.4 µA) is already at the practical floor; the TPL5111's 35 nA advantage adds reset/state-restore complexity for negligible gain
- **Expected battery life**: 5-10+ years at 1-5 min reporting (dominated by self-discharge, not active draw)
- **The real battery-life risk is parasitic board draw, not the measurement.** At a 1-5 min cadence the active measurement + radio burst is a tiny blip — even a gas reading would be affordable. What kills multi-year life is firmware that never reaches System OFF, a debugger/RTT left attached, the BLE controller (MPSL) left running between bursts, or back-feeding the `3V3` pin leaking in reverse through the on-board LDO/charger. Measure actual sleep current with a Nordic **PPK2** before trusting any estimate — even at ~30 µA you still clear 5 years; the danger is an unnoticed mA-level leak.

---

## Wireless stack

### Choice: BLE 5.0 advertising (broadcast / beacon mode)

Selected over Thread and ZigBee because:

1. **Lowest energy per cycle for sleep-mostly leaves** — no parent-polling overhead (Thread/ZigBee Sleepy End Devices must wake to poll their parent router on every cycle)
2. **Best Rust ecosystem** — `trouble` (pure-Rust BLE host) is mature in 2026 and integrates cleanly with Embassy
3. **Mesh provides no value here** — all devices are sleepy leaves with no router peers, so Thread/ZigBee mesh benefit is theoretical only
4. **Single radio for both protocols** — if we ever need Thread later, same XIAO hardware (different firmware)

### Stack details

- **Advertising**: BLE 5.0 **extended advertising**, non-connectable non-scannable undirected, via trouble's `advertise_ext` (`AdvertisementSet` + handles). Beware: trouble's plain `advertise()` is legacy-only by design — it maps the non-`Ext` enum variants to legacy 1M PDUs regardless of PHY params (this bit us for weeks; see [Field findings](#field-findings--rf-debugging-2026-07)). Sensors never accept connections — saves power, eliminates connection-state attack surface.
- **PHY**: **Coded PHY**, primary and secondary. Target coding is **S=8** (−103 dBm sensitivity, ~8 dB better than 1M); under the v1 HCI command the SDC picks the coding itself, so forcing S=8 requires `LeSetExtAdvParamsV2` with PHY options — a pending refactor to raw HCI on the sensor (a beacon barely needs a host stack, and the V2 command also returns the controller-selected TX power). S=8's extra airtime is irrelevant at our duty cycle.
- **TX power**: **+8 dBm** via `nrf-sdc`'s `Builder::default_tx_power(8)` — the per-advertising-set power field in the HCI command is a request the SDC ignores. Verified on-air with a −40 dBm A/B test (~46 dB measured swing).
- **SDC feature gates** (all build-time opt-ins): `support_ext_adv()` + `support_le_coded_phy()` on the sensor; `support_ext_scan()` + `support_le_coded_phy()` on the receiver. Extended advertising / coded PHY / extended scanning symbols exist **only in the multirole SDC library**, selected by enabling both `peripheral` and `central` cargo features on `nrf-sdc`.
- **Burst**: 20 ms advertising interval, advertiser held ~400 ms → ~20 events per burst. Each event's payload (`AUX_ADV_IND`) hops to a different data channel, so a burst samples ~20 frequencies — repetition is *frequency diversity* against indoor multipath, not just insurance. (Consequence: per-packet RSSI legitimately swings ±10-15 dB indoors; judge medians, never single readings.)
- **Burst cadence**: ~0.5 s during benchmarking; production target 1-5 min with System OFF sleep between bursts.
- **Payload**: `ManufacturerSpecificData` (company ID `0xFFFF` during testing) carrying `SensorPacket` — currently a fixed 10-byte struct; migrating to the TV measurement encoding below. Extended advertising allows up to 254 B — required headroom for the planned AEAD (payload + 16 B tag never fit legacy's 31 B).
- **Device identity**: the advertising address (AdvA), a random static address derived from FICR `DEVICEADDR` — the packet carries no identity field of its own. *(DeviceAddr refactor, 2026-07 — replaces the earlier FICR-`DEVICEID`-in-payload design.)*

### Reliability

Delivery is the product of two independent factors: **link margin** (RSSI at the receiver vs. the PHY's sensitivity floor) and **burst diversity** (events per burst × channel hopping). The table below assumes adequate link margin — the 2026-07 field campaign showed that margin is what fails first (see [Field findings](#field-findings--rf-debugging-2026-07)), so treat margin as the primary design variable and burst statistics as the secondary one.

| Configuration | Expected delivery rate (indoor, *given adequate link margin*) |
|---|---|
| 20-event burst, 1 receiver (current) | ~99 %+ |
| 20-event burst + 2nd receiver (independent capture) | ~99.99 % |
| + buffer-and-dump connected mode every 10 min | ~99.95 % |

**Receiver diversity is the designed escape hatch for hostile topology** (rooms behind multiple concrete walls): broadcast means any number of receivers hear the same packets. A second receiver is a Pi Zero + dongle running the *unchanged* gateway binary into the same MQTT broker; the API deduplicates by `(device, seq)` (the planned per-device seq monotonicity check — the same mechanism as AEAD replay protection and MQTT-redelivery idempotency). Design the data layer for N receivers from day one; deploy 1 until measurements demand more.

Per-room acceptance metric: **burst delivery rate over a 10 s window** at the sensor's real mounting spot. ≥95 % = green; 70-95 % = marginal (S=8 will likely flip it); below = fix margin (placement/hardware), not statistics. Occasional missed readings appear as small gaps in charts, not data loss. The live range-survey page that measured this during the 2026-07 campaign lives on the `reliability-benchmark` branch — it was removed from `main` to keep the gateway a single-responsibility bridge; check out that branch to rerun a survey. Long-term, per-device delivery telemetry comes from seq-gap analysis in Grafana instead.

### Field findings — RF debugging (2026-07)

A week-long range campaign produced these load-bearing facts (full story in the `reliability-benchmark` branch history):

1. **The firmware had never actually been on Coded PHY.** trouble's `NonconnectableNonscannableUndirected` + `advertise()` transmit *legacy 1M* PDUs regardless of PHY params; the receiver's `scan()` was likewise the legacy path — and because both ends matched, everything "worked", masking the bug. Fix: `ExtNonconnectableNonscannableUndirected` + `advertise_ext` / `scan_ext` + `EventHandler::on_ext_adv_reports`.
2. **Every SDC capability is a build-time opt-in.** Missing `sdc_support_*` calls surface as HCI errors — or, without `panic-probe`'s `print-defmt` feature, as silent unwrap-halts with a frozen LED. Bring up firmware probe-attached; prefer `defmt::unwrap!` so the error value prints.
3. **TX power**: the per-set HCI field is ignored; `Builder::default_tx_power()` is the real knob. Verify radio changes with large A/B deltas (+8 vs −40 dBm), not eyeballed RSSI — channel hopping makes instantaneous RSSI swing ±10-15 dB.
4. **The XIAO's chip antenna is the hard limit**: with all of the above fixed, −67 dBm @ 1 m @ +8 dBm against a phone referee (a good antenna reads ~−45), identical across two boards; ~0 % delivery at 15 m through 2 concrete walls. This drove the module migration (see [Hardware platform](#hardware-platform)).
5. **RSSI is a debugging proxy; delivery rate is the product metric.** During the campaign the gateway served a live 10 s-window reliability page (port 3000) for room-by-room surveys — robust to sensor reboots (seq-reset detection) and stray devices (locks onto one, counts foreign packets). That survey tooling now lives on the `reliability-benchmark` branch; `main`'s gateway is a pure bridge.

### Why not Enhanced ShockBurst?

ESB is Nordic's proprietary 2.4 GHz protocol — same band/MCU as BLE, but a different link layer with hardware-level auto-ACK and auto-retransmit. Worth a serious second look since our dedicated nRF52840 receiver removes the usual deal-breaker (Pi BLE radios can't speak ESB).

Tradeoffs that informed the decision to stay on BLE for now:

| Axis | BLE adv (current) | ESB |
|---|---|---|
| Range | Coded PHY S=8: −103 dBm sensitivity, ~8 dB over 1M | 1M/2M GFSK only — the nRF52840 has **no 250 kbps proprietary mode** and ESB can't use the coded PHYs → surrenders ~8 dB vs our config |
| Radio energy per cycle | ~40-60 ms radio time (~20 coded events per burst) | ~0.5-0.7 ms happy path (1 tx + ACK) — far lower, but see Range row |
| Practical battery-life delta at 5-min cadence | self-discharge-dominated (per architecture above) | same — savings disappear into self-discharge |
| Reliability | 3-channel diversity, no ACK (~99 % indoor) | 1-channel + hardware ACK + retries (~99.9 % if channel is clear) |
| Failure mode | resilient to single-channel interference | brittle on a single channel under persistent interference unless SW channel hopping is added |
| Rust / Embassy support | `trouble-host` + `nrf-sdc`, mature, Embassy-native | `esb` crate works but isn't Embassy-native; you write async glue |
| Flash footprint | ~100-160 KB | ~5-15 KB (irrelevant at our headroom) |
| Debugging | nRF Connect on any phone shows packets | needs another nRF chip in promiscuous mode + custom tooling |
| Vendor lock-in | open spec, multi-vendor | Nordic silicon only (covers nRF52840 / nRF52833 / nRF54L15) |
| Effort to migrate | n/a (already working) | rewrite firmware radio layer + dongle firmware + Pi-side parser |

**Net**: ESB's structural advantages (lower radio energy, hardware ACK) don't translate into outcomes we'd measure at our cadence and reliability target — and on the nRF52840 it would *give up ~8 dB of link budget*, the one resource the 2026-07 field campaign proved scarce. ACK-and-retry cannot rescue a link without margin; retransmitting into a fade is repetition of failure. The case for switching is even weaker than when this table was first written; prototyping ESB later as a learning exercise remains reasonable.

**When to reconsider:**

- Measured delivery rate stays <95 % in deployment and the only remaining mitigation is hardware ACK + retry
- Topology changes such that the receiver also wants to sleep (e.g., battery-powered relay nodes)
- Latency-sensitive use case needing sub-100 ms wake → TX → confirm cycles (ESB stack startup is leaner than BLE's)
- Sustained interest in learning the lower-level Nordic radio stack (valid project goal, just not a migration trigger)

---

## Data model — TV measurement encoding (planned; settled 2026-07-16)

The fixed `SensorPacket` struct is being replaced by a **per-measurement TV
encoding** — *type–value*: each field is a measurement ID followed directly
by its value, with the length implied by the ID. (The length-implied member
of the TLV family; 3GPP TS 24.007 calls this format "TV". There is
deliberately no per-field length byte — see below.) This is the BTHome
model — see `NOTES-packet-tv-aead.md` for the full worked-out plan. Guiding principle: **extensible, not generic** — adding
sensor type N+1 is a small local change at each layer; ingesting *unimagined*
sensor types with zero code changes is an explicit non-goal (that's a generic
telemetry platform, a crowded space; Homescope's unique value is the
long-range embedded-Rust node + dongle vertical, not the backend).

- **Air packet**: `[seq: u32][id][data][id][data]…` — `seq` stays a fixed
  cleartext header (dedup / replay / AEAD nonce); each measurement is a
  1-byte ID + value.
- **Measurement ID registry in `common`**: each ID binds semantics + wire
  representation + scale + unit (e.g. *temperature = i16, ×0.01 °C*). The ID
  implies the length — no per-field length byte. Repr/scale are fixed per ID
  (scaled integers, never floats, on the wire); a metric needing more
  range/resolution later gets a **new ID**, not a format change.
- **Strict decode posture**: unknown ID, truncated data, or duplicate ID ⇒
  drop the whole packet with a warning (mirrors the unknown-device rule).
- **Receiver and gateway are semantics-blind**: packet bytes are opaque past
  the sensor; only the API decodes. Adding a measurement type touches
  firmware + API + one DB migration — never the receiver, gateway, frame
  format, or topics. (Consequence: the USB-CDC frame becomes variable-length
  — protocol v0.5.)
- **Node variants dissolve**: a node advertises whatever its populated
  sensors measured this cycle; "SHT45+BMP581 node" is not a named layout
  anywhere downstream. Cargo features select drivers, nothing else.
- **The DB stays wide**: one nullable column per physical metric, canonical
  units (`f64` °C etc.); `time`/`device_id`/`seq`/`rssi` stay NOT NULL.
  Multiple wire encodings of one metric converge into one column. Narrow/EAV
  storage was considered and rejected: worse compression, every Grafana panel
  becomes a pivot, no per-metric types — flexibility we'd pay for daily to
  serve users we don't have. The TV wire format keeps that migration path
  open if the project ever pivots to platform ambitions.

---

## Security

### Threat model

- **Confidentiality**: nobody reads our sensor values
- **Integrity**: nobody can forge plausible-looking readings
- **Replay**: nobody can capture an advertisement and re-broadcast it later

Note that the primary value is **authenticity**, not confidentiality: without the AEAD tag, anything in radio range emitting a well-formed packet gets written to the database. Confidentiality (occupancy patterns leak from T/H data) is the secondary win — and ChaCha20-Poly1305 provides both for the same 16-byte cost.

### Mechanism: ChaCha20-Poly1305 AEAD with per-device keys

Each sensor has a **unique 32-byte ChaCha20-Poly1305 key** baked into firmware at flash time. Per-device (not network-wide) so extracting one device's firmware does not compromise the rest.

### Payload layout *(revised 2026-07-16 with the TV redesign — see `NOTES-packet-tv-aead.md`)*

```
+--------------+---------------------------+----------------+
| seq counter  | ciphertext (TV section)  | Poly1305 tag   |
| 4 bytes      | N bytes                   | 16 bytes       |
+--------------+---------------------------+----------------+
  ^plaintext^    ^encrypted^                 ^authenticates all of it^
```

- **Device identity is not in the payload** — it's the advertising address (AdvA, from FICR `DEVICEADDR`), which the receiver observes and forwards as `device_addr`. The **API** uses it to look up the per-device key (the `devices` table is the key registry; gateways stay keyless — see [Gateway & API integration](#gateway--api-integration)).
- `seq` (4 B, plaintext) — monotonic, increments every advertisement, **never repeats over device lifetime** (persisted — see below). Provides the AEAD nonce (deterministic construction from seq; no random component needed once seq is persisted) and **replay protection**: the API tracks last-seen per device and rejects `≤`. The same check deduplicates overlapping receivers and MQTT redeliveries.
- **Ciphertext** = the TV measurement section (whatever this node sampled this cycle).
- **Associated data** = `device_addr` + `seq` — the cleartext context is authenticated too, so a valid ciphertext can't be replayed against a different device identity or sequence number.

The plaintext header + opaque blob structure carries through the whole pipeline: receiver and gateway forward it untouched, MQTT transports it as a JSON envelope (routing/debugging fields readable, sensor values base64 ciphertext), and only the API decrypts. The envelope shape actually arrives *before* AEAD — the moment the TV encoding makes the payload opaque to the gateway — so the crypto step changes nothing downstream of the firmware except the API.

### Seq persistence (prerequisite — sensor side)

Nonce reuse under the same key is catastrophic for ChaCha20-Poly1305, and reboots (battery swap, watchdog reset, panic) would restart a RAM-only counter at 0. Plan: retained-RAM counter across System OFF wake cycles (no flash wear on the every-minute path) + a flash checkpoint every N counts with **jump-ahead on boot** (`resume at checkpoint + N`, never backwards). At a 60 s cadence and N=1024 that's one flash write per ~17 h against a 10k-cycle page endurance — decades of margin. This also fixes the "reboots reset seq" caveat in the API's monotonic replay/dedup check.

### Key provisioning

The device **identity** needs no provisioning — the advertising address comes from the nRF52840's FICR at runtime. The per-device **key** (planned) is written once into the chip's **UICR** by a workstation CLI. *(Settled 2026-07-20 — supersedes an earlier build-time `DEVICE_KEY` env + `link_section` sketch, rejected because it makes the firmware image per-device. Full plan: `NOTES-provisioning.md`.)*

```
homescope-provision --name "kitchen" --site home --room kitchen
  1. probe-rs reads FICR DEVICEADDR from the blank chip (no firmware needed)
  2. POST /devices to the API → it generates the 32-byte key, returns it once
  3. write the key to UICR.CUSTOMER[0..7] (+ REGOUT0 = 3.0 V on the custom PCB)
  4. read UICR back and verify, then flash the generic firmware image
```

Key properties: **one firmware binary for the whole fleet** (no per-device ELF, no key in build artifacts or CI); UICR survives ordinary reflashes, since `probe-rs`/`cargo flash` erase only the pages they write; and the custom PCB needs a UICR write for `REGOUT0` anyway. Costs: rotation means `NVMC.ERASEUICR` + rewrite (which also resets `REGOUT0` — rewrite both), a full chip erase destroys the key (recover by re-provisioning from the DB), and confidentiality against a physical attacker rests on APPROTECT, which is deliberately left off during development. Nodes destined for sealed enclosures use a dedicated flash page instead, since firmware can write that itself over USB-CDC and UF2 cannot write UICR.

The API is the key registry and the issuer: it generates the key, returns it exactly once, 409s on an already-registered address, and exposes rotation as a separate admin-authenticated endpoint. A device without a row (and key) is dropped by ingest, which is why auto-registration is deliberately absent. The probe stays on the bench — driving SWD from the API host was considered and rejected (an HTTP endpoint that writes firmware is an endpoint that writes firmware).

**Keys are encrypted at rest.** The device key can't be hashed — the API needs the plaintext to decrypt — so the `devices` key column stores it under envelope encryption: a master KEK from the API's environment/secrets, `ChaCha20-Poly1305(KEK, device_key)` with `device_addr` as AAD, decrypted into the `DeviceRegistry` cache at startup (and a hard startup failure if the KEK is missing or fails to authenticate). Without this, every `deploy/backup-db.sh` tarball is a full fleet compromise: symmetric AEAD means whoever can verify can forge.

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

1. **Scan**: continuously listen for our manufacturer-ID advertisements (extended scanning on Coded PHY — `scan_ext` + `on_ext_adv_reports`; requires `support_ext_scan` + `support_le_coded_phy` in the SDC build)
2. **Forward**: emit framed packets to the Pi over USB-CDC (magic + payload + CRC; see [protocol.md](protocol.md))

### Pi gateway responsibilities (implemented — `gateway/`)

The gateway is deliberately a **thin, keyless bridge**:

1. **Read**: parse framed packets from the receiver dongle (`RECEIVER_PATH`, default `/dev/homescope-receiver` via the udev rule in [deploy/udev](../deploy/udev/99-homescope-receiver.rules)) using `serial2-tokio` + the `tokio_util::codec::Decoder` in `gateway/src/decoder.rs`
2. **Timestamp**: convert `SensorObservation` → `SensorReading` with `received_at = Utc::now() − age_ms`
3. **Publish**: JSON (serde camelCase: `deviceAddr`, `seq`, `tempDegc`, `rhPercent`, `batteryMv`, `rssi`, `receivedAt`) to `homescope/sensors/<device-addr>/reading` at QoS 1, `<device-addr>` rendered as 12 uppercase hex chars. *(Once the TV encoding lands, the per-metric fields are replaced by an opaque `payload` — base64 of the TV bytes, later the ciphertext — and only the API interprets it.)*

Configuration comes from env vars (`MQTT_HOST`, `MQTT_PORT`, `RECEIVER_PATH`) via the shared `host-util` crate (dotenv + tracing init). Serial errors are fatal by design — the process exits and the supervisor (quadlet, `Restart=on-failure`) restarts it.

**Decryption will NOT happen here.** When AEAD lands, the gateway forwards the ciphertext opaquely inside an envelope (plaintext `deviceAddr`/`seq`/`rssi`/`receivedAt` + base64 ciphertext) and the **API decrypts**. Rationale: gateways stay keyless and thin — a remote-house Pi is the most physically exposed component; no key-distribution mechanism is needed; keys live only in API + DB; and with symmetric AEAD, *being able to verify = being able to forge*, so key-holding gateways would widen the blast radius of a compromise from "spams its own topics" to "forges any device it has keys for". The envelope keeps `mosquitto_sub` useful for debugging while readings stay confidential. *(Reverses an earlier decrypt-in-gateway plan, settled 2026-07-14; re-examined and reaffirmed 2026-07-16 — the decision is reversible in one direction only: an opt-in gateway decrypt mode can be added later without protocol changes if the dongle+gateway ever needs to work standalone for third parties. Integrations don't need it — see the decoded-readings republish below.)*

### MQTT broker (Mosquitto, container on the Pi)

- Topic hierarchy: `homescope/sensors/<device-addr>/reading`. Planned: a per-gateway site prefix — `homescope/<site>/sensors/<device-addr>/reading` — where `site` is *transport provenance* ("which gateway heard this", set per-gateway via env), distinct from the device's owner-assigned `site`/`room` metadata in the DB (a boundary device could legitimately be heard by either gateway). The prefix is also what enables per-gateway broker ACLs (each gateway's credentials publish-only under its own prefix).
- Persistence enabled (`persistence true`, `max_queued_messages 10000`) — with the API's durable session (`clean_session=false`, QoS 1) the broker queues readings across API downtime and replays them on reconnect
- Currently `allow_anonymous true` on the podman network; `password_file` + per-topic ACLs are planned before the broker is reachable beyond the Pi (two-house VPN rollout)
- Multiple consumers can subscribe independently (API, Home Assistant, Grafana, etc.)
- **Planned — decoded-readings republish (the integration surface):** once payloads are opaque on the ingest topics, the API republishes each reading as plain JSON *after* decrypt + verify + seq-dedup — e.g. `homescope/<site>/sensors/<device-addr>/state`, or Home Assistant MQTT-discovery format. Placing integrations *downstream* of verification means they see authenticated, deduplicated data (a gateway-decrypt design would hand them duplicates from overlapping receivers and unfiltered replays). Optional, last-in-line — nothing else depends on it.

### Why MQTT over direct HTTP POST

- **Decouples** the radio-side ingest from API availability — API outage doesn't lose data
- **Trivially multi-consumer** without changing the gateway
- **Industry-standard** for IoT, well-supported tooling
- Cost: ~10 MB RAM for Mosquitto on the Pi; negligible

### API service (implemented — `api/`)

`homescope-api` is the MQTT→TimescaleDB ingest service (the HTTP API is the next layer, not built yet — `axum` is already in the dependency tree):

- **Ingest pipeline**: `rumqttc` subscribes to `homescope/sensors/+/reading` (QoS 1, durable session, client id `api`) → bounded `mpsc` channel (256, `try_send` with drop-and-warn backpressure) → a writer task inserts into Postgres.
- **Device registry**: a `devices` table (`id`, unique device address — the `hardware_id` column is becoming `device_addr` with the DeviceAddr refactor — and `name`) is loaded at startup into a `DeviceRegistry` — a `Clone` handle over `Arc<Cache>` (sync `RwLock<HashMap>`; guards never held across `.await`). Readings reference `devices.id` via FK.
- **Unknown devices are dropped, warn-once** (re-reported hourly, bounded tracking with an overflow counter so an id flood can't grow memory). **No auto-registration** — once AEAD lands the `devices` table doubles as the key registry, and a row (with a key) must exist before readings are accepted; auto-registration would race key provisioning.
- **Schema**: `readings` is a TimescaleDB hypertable partitioned on `time`, indexed `(device_id, time DESC)`. Migrations run at startup when `RUN_MIGRATIONS=true` (sqlx migrate).
- **Known gap — durability during DB outages**: an insert failure currently logs and continues, but rumqttc auto-acks on poll, so readings polled during a DB outage are lost. The end-state is manual acks (`set_manual_acks`) after successful insert, with the per-device `seq` monotonicity check making redelivery idempotent — that check does triple duty (AEAD replay protection, multi-receiver dedup, MQTT redelivery dedup). Graceful shutdown (drain the channel on SIGTERM before exit) is a related pending exercise.

### Deployment (implemented — `deploy/`)

All Pi-side services run as **rootless Podman containers under systemd quadlets** (a dedicated `homescope` user with lingering enabled):

- **Quadlets** (`deploy/quadlets/`): `mosquitto`, `timescaledb` (TimescaleDB 2.x / PG 18), `api`, `gateway`, `grafana`, all on a shared podman network; named volumes for broker and DB state.
- **Images**: GitHub Actions builds ARM images per crate on push to `main` (path-filtered) → `ghcr.io/eldigh/homescope-{gateway,api}`. Quadlets use `AutoUpdate=registry` with the `podman-auto-update` timer, so the Pi picks up new images on its own.
- **`deploy/deploy.sh`**: idempotent converge script (`git pull && sudo ./deploy/deploy.sh`) — root phase creates the user, installs the udev rule, and stages the deploy tree; a user phase installs quadlets and config under `~/.config/homescope/`. Secrets are generated once and never overwritten. `deploy/backup-db.sh` handles DB dumps.
- **udev** (`deploy/udev/`): matches the receiver (VID/PID + product string) and symlinks it to `/dev/homescope-receiver`, which the gateway quadlet passes through via `AddDevice`. Planned tightening: a real VID/PID (pid.codes) + serial match, `TAG+="systemd"` device units so the container's lifecycle binds to device presence (`BindsTo=`), and `ID_MM_DEVICE_IGNORE` to keep ModemManager from writing AT commands into the CDC endpoint.
- **Grafana**: provisioned TimescaleDB datasource + committed dashboard (`deploy/grafana/`), anonymous viewer access, published on port 4000. The `just grafana-pull` recipe exports UI edits back into the repo.
- **Dev stack**: `compose.dev.yml` (mosquitto + TimescaleDB + Grafana, localhost ports) driven by the `justfile` — `just api` / `just gateway` bring up their dependencies automatically; `just db-seed` loads ~90 days of fake readings; `just dev` opens a zellij workspace.

---

## Software updates

| Stage | Mechanism | When |
|---|---|---|
| **v1** | UF2 reflash via physical double-tap-reset | Initial deployment, development iteration |
| **v2** | BLE DFU via the Adafruit bootloader, triggered by double-tap-reset, updated via nRF Connect mobile app | Once enclosures are sealed and physical USB access is awkward |
| **v3** (optional) | Always-on OTA: sensor wakes every ~6 h to listen briefly for incoming connection | Only if frequent updates are needed and the ~1 % battery overhead is acceptable |

The Adafruit UF2 bootloader on the XIAO supports BLE DFU out of the box; no extra firmware needed to enable v2. The bootloader is open source, so the custom MDBT50Q PCB will carry the same bootloader (ported board definition), preserving both the UF2 and BLE-DFU flows on our own hardware.

---

## Project structure

Monorepo with **two separate Cargo workspaces** split by target architecture:

```text
homescope/
├── Cargo.toml             # host-target workspace: gateway, api, common, host-util
├── justfile               # dev workflow: `just api`, `just gateway`, `just db-seed`, …
├── compose.dev.yml        # dev stack: mosquitto + TimescaleDB + Grafana (localhost)
├── common/                # shared types (`homescope-common`, no_std-by-default)
│   ├── Cargo.toml         # feature-gated: `wire` (bytemuck+crc), `serde` (chrono+serde)
│   └── src/
│       ├── lib.rs
│       ├── device_addr.rs # DeviceAddr([u8; 6]) — BLE advertising address (AdvA, FICR-derived), hex Display/serde
│       ├── packet.rs      # SensorPacket (repr(C, packed)) — over-the-air payload (TV redesign planned)
│       ├── observation.rs # SensorObservation = packet + receiver RSSI/age
│       ├── frame.rs       # Frame: magic + payload + CRC-16/IBM-SDLC
│       └── reading.rs     # SensorReading (serde, human units) — the MQTT JSON payload
├── gateway/               # Pi-side bridge: USB-CDC decode → MQTT publish
│   ├── Cargo.toml         # `homescope-gateway`
│   ├── Containerfile
│   └── src/               # main.rs, config.rs (env), decoder.rs (framing Decoder)
├── api/                   # `homescope-api` — MQTT → TimescaleDB ingest (HTTP API next)
│   ├── Cargo.toml
│   ├── Containerfile
│   ├── migrations/        # sqlx migrations: readings hypertable, devices table
│   └── src/               # main.rs, config.rs, db.rs, ingest/ (mqtt + writer + unknown-device tracking), devices/ (registry/cache/store)
├── host-util/             # `homescope-host-util` — shared host-side init (dotenv, tracing) + env helpers
├── firmware/
│   ├── Cargo.toml         # firmware workspace: sensor, receiver, board
│   ├── rust-toolchain.toml
│   ├── .cargo/config.toml # cross-compile target (`thumbv7em-none-eabi`)
│   ├── board/             # `homescope-board` — Board struct + board!(p) macro (features: db40 / xiao)
│   │   ├── build.rs       # picks memory-*.x by board feature
│   │   ├── memory-db40.x  # bare board — app at 0x0
│   │   ├── memory-xiao.x  # UF2 bootloader — app at 0x27000
│   │   └── src/lib.rs
│   ├── sensor/            # `homescope-sensor` — BLE-advertising sensor firmware
│   │   ├── flash_uf2.sh   # UF2 backup flow (calls tools/uf2/uf2conv.py)
│   │   └── src/           # main.rs, ble_advertise.rs, sensors.rs, battery.rs, packet_builder.rs
│   └── receiver/          # `homescope-receiver` — USB-CDC BLE-scanning dongle
│       ├── flash_uf2.sh
│       └── src/           # main.rs, ble_scan.rs
├── deploy/                # production deployment (Pi)
│   ├── deploy.sh          # idempotent converge script (root + homescope-user phases)
│   ├── backup-db.sh
│   ├── quadlets/          # podman systemd units: mosquitto, timescaledb, api, gateway, grafana + network/volumes
│   ├── grafana/           # provisioning (datasource, dashboards) + committed dashboard JSON
│   ├── mosquitto/         # broker configs (prod + dev)
│   ├── timescaledb/       # init roles + dev seed SQL
│   ├── systemd/           # podman-auto-update timer override
│   └── udev/              # receiver → /dev/homescope-receiver symlink rule
├── .github/workflows/     # per-crate ARM container builds → ghcr.io (path-filtered)
├── tools/
│   └── uf2/               # vendored microsoft/uf2 tooling (MIT) — used by both flash_uf2.sh
└── docs/
    ├── architecture.md
    ├── flashing.md
    └── protocol.md
```

Rationale:

- **Two workspaces, not one.** A single workspace mixing `thumbv7em-none-eabi` firmware with host-target gateway breaks rust-analyzer — it picks one default target and the other side errors out. Splitting at the `firmware/` boundary lets each IDE session resolve a consistent target. The `common` crate is referenced from both workspaces via `path = "../common"` so the type definitions stay deduplicated.
- **`firmware/sensor/` and `firmware/receiver/` named by role, not chip.** Both currently target nRF52840; the role is what distinguishes them. If/when a different chip family enters the stack (`firmware/sensor-esp32/` etc.), the suffix grows from the role.
- **`common` crate** avoids duplicating wire types between firmware (encoder) and gateway (decoder). `no_std` by default with feature-gated extras. Frame layout (magic + payload + CRC), CRC algorithm, and parse/build logic all live in `frame.rs` — both ends use `Frame` / `Frame::try_from_bytes`. The gateway-bound payload is `SensorObservation` (the air packet plus receiver-observed RSSI and age; see [protocol.md](protocol.md)).
- **Packed wire structs vs app struct in `common`**: `SensorPacket`/`SensorObservation` (`repr(C, packed)`, wire formats) and `SensorReading` (normal layout, serde-derived, human units). Conversion via `SensorReading::from_observation` on the gateway side. **Don't combine into one struct** — serde on a packed struct generates unaligned-reference code (undefined behaviour).
- **`.cargo/config.toml` lives at `firmware/.cargo/`**, not at repo root — it sets the cross-compile target only for firmware workspace members.
- **Node variants**: one firmware codebase, with the sensor set selected at build time via Cargo features (e.g. `--features outdoor` adds the optional LTR390; `--features barometer` adds the BMP581 on the one designated indoor node), or detected at boot by probing which I²C devices ACK. All variants share the same `memory.x` and bootloader offset. Avoid maintaining separate binaries/codebases — the only real difference is which sensors are populated.

---

## Implementation roadmap

1. ✅ **Sensor firmware skeleton** — Embassy + nrf-sdc + trouble-host. BLE advertising works end-to-end, visible in nRF Connect.
2. ✅ **Repo restructure** — `firmware/sensor/`, `firmware/receiver/`, `gateway/`, `common/` established. Two Cargo workspaces split by target.
3. ✅ **`common` crate** — `SensorPacket` (air payload), `SensorObservation` (receiver→gateway), `SensorReading` (app type, serde), `HardwareId`, `Frame` framing + CRC-16/IBM-SDLC. Shared by all crates.
4. ✅ **Receiver dongle firmware** — `firmware/receiver/`. Extended scanning on Coded PHY for our manufacturer-ID advertisements, forwards framed `SensorObservation`s (packet + RSSI + age) over USB-CDC. Robust to host disconnect/reconnect (DTR-aware writes, drop-oldest backlog).
5. ✅ **Gateway v1 receiver path** — `gateway/` reads `/dev/ttyACM0` (or udev symlink) via `serial2-tokio` + `tokio_util::codec::Decoder`, validates magic + CRC, decodes `SensorObservation`, converts to `SensorReading`.
6. ✅ **Gateway v1 MQTT publish** — `rumqttc` publishing JSON readings to `homescope/sensors/<hardware-id>/reading` at QoS 1, env-configured (`host-util`), containerized. The temporary range-survey page (axum, port 3000: 10 s rolling delivery %, RSSI stats, reboot detection) served the RF campaign and was then retired to the `reliability-benchmark` branch.
7. ✅ **Actual Coded PHY + RF field campaign (2026-07)** — migrated to real extended advertising (`advertise_ext` / `scan_ext` / `on_ext_adv_reports`), enabled the SDC feature gates + multirole library, TX power via `default_tx_power(8)`, ~20-event bursts. Verdict: the XIAO antenna is the limiting factor → module migration. See [Field findings](#field-findings--rf-debugging-2026-07).
8. ⏳ **S=8 coding refactor** — drive sensor advertising with raw HCI (`LeSetExtAdvParamsV2` + PHY options) to force S=8 and log the controller-selected TX power; a beacon doesn't need a host stack.
9. 🔶 **Hardware migration** — ✅ 2× MDBT50Q-DB-40 acquired, house survey passed (2026-07-03, worst spot ≥85 %), MDBT50Q-1MV2 confirmed as production module. ⏳ Custom PCB (VDDH topology, antenna keep-out, SHT45 thermal moat, BMP581/LTR390 footprints on all boards with selective population, 1.27 mm Cortex debug header).
10. ✅ **API v1 — MQTT→TimescaleDB ingest** (2026-07) — `homescope-api`: `rumqttc` durable-session subscriber → bounded channel → sqlx writer into a `readings` hypertable; `devices` registry table with in-memory cache; unknown devices warn-once + drop (no auto-registration); migrations at startup. HTTP endpoints not started yet (`axum` staged in deps).
11. ✅ **Grafana** (2026-07) — provisioned TimescaleDB datasource + committed dashboard, anonymous viewer access, port 4000; `just grafana-pull` round-trips UI edits into the repo. Still to add: per-device delivery-rate panels derived from seq gaps (permanent reliability telemetry).
12. ✅ **Containerization & deployment** (2026-07) — rootless Podman quadlets (mosquitto, timescaledb, api, gateway, grafana) under a dedicated user; ARM images built in CI → ghcr.io with `AutoUpdate=registry`; idempotent `deploy/deploy.sh`; udev-symlinked receiver. *(Plain quadlets won over the earlier `.kube`/Pod-YAML idea.)*
13. ⏳ **Ingest durability & lifecycle** — the next backend block: per-device `seq` monotonicity check (one mechanism = AEAD replay protection + multi-receiver dedup + MQTT-redelivery idempotency), manual MQTT acks after successful insert (rumqttc `set_manual_acks`; today a DB outage silently drops polled readings), graceful shutdown (SIGTERM → drain channel → bounded exit).
14. ⏳ **Site topology & broker hardening** — for the two-house/VPN deployment: `devices.site`/`room` columns + gateway `SITE` env var and `homescope/<site>/...` topic prefix (one change), then mosquitto `password_file` + per-topic ACLs (per-gateway publish-only credentials, API subscribe-only).
15. ⏳ **Receiver/gateway plumbing tightening** — real VID/PID (pid.codes) instead of embassy's `0xc0de:0xcafe` placeholder, udev match on FICR serial, `TAG+="systemd"` device unit + `BindsTo=` so the gateway container's lifecycle follows the dongle (replug-safe), `ID_MM_DEVICE_IGNORE`.
16. ⏳ **Sensor drivers** — ✅ SHT45 over async TWIM (`sht4x` crate); ⏳ BMP581 (`bmp5` async crate) on the designated indoor barometer node; optional LTR390 on the outdoor node.
17. ⏳ **Watchdog + XIAO alkaline soak test** — watchdog first (probe-less panic = silent HardFault spin that drains the pack), then the unattended reliability/longevity baseline on 2× AA alkaline.
18. ⏳ **Packet redesign + crypto** *(plan settled 2026-07-16 — see `NOTES-packet-tv-aead.md`; owner's order)*: ① finish the **DeviceAddr refactor** (identity = AdvA, protocol v0.4 — in flight); ② **TV measurement encoding** (measurement-ID registry in `common`, variable-length frames = protocol v0.5, opaque-payload MQTT envelope, nullable metric columns); ③ **seq persistence** on the sensor (retained RAM + flash checkpoint with jump-ahead); ④ **ChaCha20-Poly1305 AEAD** — encrypt the TV section on the sensor, `device_addr`+`seq` as AAD, forward opaquely, **decrypt in the API**. Fits extended advertising's 254 B budget (never fit legacy's 31 B).
19. ⏳ **Sleep & power optimization** — System OFF + RTC wakeup between bursts. Gate sensor rail power during sleep. Measure with PPK2.
20. ⏳ **Provisioning** *(plan settled 2026-07-20 — see `NOTES-provisioning.md`)* — `homescope-provision` workstation CLI (probe-rs: FICR read → API registration → UICR key write → verify → flash), admin-authenticated `POST /devices` key issuance in the API, and envelope-encrypted device keys at rest (KEK in API secrets). Device identity needs no provisioning — the advertising address is FICR-sourced.
21. ⏳ **Decoded-readings republish** — API republishes verified+deduped readings as plain JSON (or HA MQTT discovery) for external consumers; the integration surface. Optional, after AEAD.
22. ⏳ **(Future, optional) Air-quality node** — BME68x + BSEC on a USB/mains-powered node; persist BSEC calibration state across reboots. Separate from the battery fleet.

---

## Future: Matter

**Not pursued now.** Matter (Apple/Google/Amazon smart-home standard) runs over Thread or Wi-Fi and adds value only if we want our sensors discoverable by mainstream smart-home hubs. We have our own API, so Matter would be added complexity without payoff.

If we ever want Home Assistant integration, the simpler path is **MQTT discovery** — Home Assistant auto-discovers MQTT-published sensors via the `homeassistant/` topic convention. No firmware changes needed — and the planned decoded-readings republish (see [MQTT broker](#mqtt-broker-mosquitto-container-on-the-pi)) is the natural place to emit it: HA would consume verified, deduplicated readings straight from the API.

A related open direction: **BTHome compatibility** (<https://bthome.io>) — an open BLE advertising format with typed measurement IDs and first-class HA support. A firmware build flag emitting BTHome would make "long-range Coded-PHY BTHome node in Rust" a genuinely novel artifact, and its object-ID table is worth aligning with when assigning our TV measurement IDs. Caveat to verify first: BTHome listeners in the wild generally scan legacy 1M advertising, so Coded-PHY-only nodes would still need our dongle — which is arguably the point.
