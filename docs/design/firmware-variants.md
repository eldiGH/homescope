# Firmware variants: runtime detection, not Cargo features

> **Status: ⏳ planned.** Decision settled **2026-08-26**; not started — there is
> no boot-time probe yet, and the BMP581 and LTR390 drivers are unwritten. The
> artifact store in the second half is build-order step 4 of
> [provisioning.md](provisioning.md). Prerequisite: [twim-cancel-safety.md](twim-cancel-safety.md).

**Supersedes** the earlier "node variants are one codebase behind Cargo
features" plan, which `CLAUDE.md` § Key facts / Sensors now records as reversed
— and the `docs/architecture.md` sensor section, if it repeats the old plan.

Scope: how one firmware image serves nodes with different sensors fitted, and
how the provisioning tool decides which image to flash.

## The decision

**One sensor firmware per *board*, which probes I²C at boot and reports
whatever answered.** No per-peripheral Cargo features. No configurator.

Board features (`board-db40` / `board-xiao`, later the custom PCB) stay — they
select pins, the RAM window and the linker script, none of which can be
detected at runtime. That is 2 builds today, 3 when the PCB lands. A list you
pick from, not a matrix you configure.

## Why

The prompt for this was an idea for an interactive wizard that ticks a
checkbox per peripheral and builds the matching image. It is a good UI for a
problem worth not having.

**The graceful path is already written and already exercised.**
`firmware/sensor/src/sensors.rs:49`:

```rust
Err(e) => warn!("sht read failed, dropping T/H this cycle: {}", e),
```

The measurement vector simply does not get T/H, the TV body carries what it
has, and the API stores NULL. That is *exactly* the behaviour a "no pressure
sensor on this node" feature flag would produce, minus the flag. The only
change runtime detection needs is to decide once at boot instead of warning
every 60 s.

**The wire format was designed for this.** TV's whole reject-vs-store rule is
"unknown ID / truncated / duplicate / empty ⇒ reject the packet; a *missing*
metric ⇒ store NULL". Gating metrics at build time solves at compile time a
problem the packet format already solved at runtime.

**It is what [provisioning.md](provisioning.md) already asks for.** Its stated guiding
constraint is *"one firmware binary for the whole fleet — anything that makes
the image per-device is rejected on that ground alone."* Per-peripheral
features do not make the image per-device, but they make it per-variant, which
is the same direction with a smaller number.

**The parts identify themselves.** All three are I²C at distinct addresses,
and one probe primitive is already in the dependency tree: `sht4x` 0.2.0
exposes `serial_number()` — CRC-checked, address 0x44/0x45. BMP581 and LTR390
have chip-ID registers at their own addresses. ⚠️ Confirm the exact ID
register offsets and expected values against the datasheets before coding;
they are not recorded here because they were not verified.

**Timing.** The per-peripheral features do not exist yet —
`firmware/sensor/Cargo.toml` has only `board-db40`, `board-xiao`,
`wait-for-rtt`, and `Sensors` hardcodes `sht: Sht4x<I2C>` + `battery: Battery`.
The BMP581 and LTR390 drivers are unwritten. This is the cheapest moment the
decision will ever be.

## Costs, stated honestly

- **Unfitted drivers ship in every image.** A few KB against 868 KB. Not an
  argument.
- **A boot probe costs one NACK per absent address.** ~100 µs, once. Not an
  argument.
- **You lose the compile-time claim "this is the outdoor build."** What
  replaces it is strictly better: the API knows what each device reported
  yesterday, so a metric that disappears is an *alert*. That catches a bad
  solder joint **and** a sensor dying in service in year two; a feature flag
  catches neither. Worth building that alert — it is the thing that makes this
  decision safe, and it does not exist yet.
- ⚠️ **The real prerequisite is the TWIM cancel-safety bug** — see
  [twim-cancel-safety.md](twim-cancel-safety.md). A boot-time bus scan is precisely where you
  reach for a timeout, and `with_timeout` around `Twim::transaction` orphans a
  live DMA transfer. Fix `sensors/sht4x.rs` and settle the probe's wait
  strategy *first*. The probe is more transactions on the same bus, and one of
  them is against an address that may not answer.

## What the probe should do

Sketch, not settled:

- Probe once, at boot, after the sensor rail has settled (the rail guard
  already handles the 10 ms SHT4x power-up).
- Record which parts answered into a small `Sensors` value holding
  `Option<Driver>` per part.
- **Log the detected set over defmt at boot.** This is what makes a
  mis-detection debuggable, and it doubles as the L0 verification signal for
  `homescope-provision` (see [provisioning.md](provisioning.md) § verification ladder).
- An absent part is silent thereafter. A part that answered at boot and then
  starts failing keeps warning — that distinction is the point.
- Do **not** re-probe on every cycle. A device that appears mid-life is not a
  case worth supporting, and re-probing turns a stuck bus into a per-cycle
  hazard.

## If this is ever reversed

The one real argument for compile-time features is asserting intent: a node
that is *supposed* to have a BMP581 and has a cold solder joint reports T/H
and looks fine. The answer is the API-side alert above, not the feature flag —
but if that alert never gets built, revisit this.

---

# Firmware artifacts: a store, not a path

Settled 2026-08-26. This is the part of the wizard idea worth keeping, and it
holds regardless of the decision above.

## The problem

`homescope-provision flash --firmware <path>` takes a filesystem path into the
one command that can brick a board by receiving the wrong file:

- a `board-db40` image links at `0x0`; flashed to a XIAO over SWD it erases
  the MBR, SoftDevice and UF2 bootloader;
- a `board-xiao` image links at `0x27000`; flashed to a DB-40 it leaves `0x0`
  erased, so the CPU loads `0xFFFFFFFF` as its initial SP and reset vector and
  the board looks dead.

"Whatever ELF was in the directory you ran from" is not an acceptable input to
that decision, which is why there is deliberately **no CWD default**
([provisioning.md](provisioning.md)).

## The store

```
~/.local/share/homescope/firmware/
  manifest.toml
  a3f1c2….elf
```

```toml
[[firmware]]
id         = "a3f1c2"          # hash of the ELF — the artifact's real identity
name       = "sensor-xiao"     # what you type
crate      = "homescope-sensor"
features   = ["board-xiao"]
git        = "86f1625"
dirty      = true
built_at   = "2026-08-26T21:04:00Z"
app_start  = 0x00027000        # read FROM the ELF
storage    = [0x000F2000, 0x000F4000]
```

**`app_start` and `storage` are read out of the ELF, not copied from the build
config.** That is what makes them trustworthy: the manifest can be wrong about
`name`, but it cannot be wrong about where the image loads or where its
persistent storage lives. Both are what the tool needs to guard the flash and
to clear the seq counter — see [provisioning.md](provisioning.md) for both mechanisms and
the `nm` evidence that the symbols are there.

`git` + `dirty` is the provenance that matters when a sealed node misbehaves
eleven months later. Record it and surface it in the picker; do **not** refuse
to store a dirty build — every build is dirty right now, and a tool that
refuses the normal case gets bypassed.

## The tool owns the store, not the build

⚠️ **`homescope-provision` must not shell out to `cargo`.** Same separation as
"the API's job is the registry; the probe stays on the bench": a provisioning
CLI that builds firmware acquires a source checkout, a `thumbv7em` toolchain
and a rustup target as hard runtime requirements — for a tool whose direction
is consuming artifacts CI will eventually produce. It also welds the fleet
tool's release cycle to the firmware's, and makes "which commit did this node
get" mean "whatever was uncommitted at 11pm".

The seam is one subcommand:

```
homescope-provision firmware add <path> --name sensor-xiao
homescope-provision firmware list
```

`add` reads the ELF, extracts `app_start` and the storage symbols, hashes it,
copies it in, writes the manifest row. Any producer then works: a justfile
recipe, CI, or a downloaded artifact.

Build via the existing `justfile`, which already encodes the two-workspace
weirdness that bit us with `cargo fmt --all`:

```
firmware-build board:
    cd firmware && cargo build --release -p homescope-sensor --features board-{{board}}
    homescope-provision firmware add firmware/target/.../homescope-sensor --name sensor-{{board}}
```

Ten lines, no new Rust, and the same command CI runs later.

## On the wizard as a UI

The interactive value is concentrated in one narrow place: **choosing from a
list you cannot remember.** That is the artifact picker — a fuzzy select when
`--firmware` is omitted and stdin is a TTY. Build that.

Skip the rest. "Do you want to provision or update firmware" is a decision
already made before the terminal was opened, and a menu wrapping
`provision`/`flash` is a second surface to keep in sync with the first.
Subcommands are scriptable, completable, and greppable in shell history; a
menu is none of those.

**An interactive picker at one point, not an interactive mode.**
