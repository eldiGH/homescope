# Flashing the XIAO nRF52840 Plus

The Seeed XIAO nRF52840 **Plus** ships with the **Adafruit UF2 bootloader v0.9.2** pre-installed (Board-ID `nRF52840-SeeedXiao-v1`), which exposes the board as a USB mass-storage device when in bootloader mode. Dropping a `.uf2` file onto the mount triggers flash + automatic reboot into the new application.

**Primary development flow uses probe-rs over SWD** — see `.vscode/launch.json` (`Debug nrf52840-* (debug build)`) or just `cargo run` from inside a firmware crate (the `runner` in `firmware/.cargo/config.toml` is wired to `probe-rs run --chip nRF52840_xxAA`). This gives breakpoints, single-stepping, and defmt-RTT logs in the VSCode Debug Console.

**UF2 is the fallback path**, used when the chip can't be accessed via SWD — sensor units deployed in sealed enclosures, or any situation where the probe isn't available. The receiver dongle stays on the bench permanently and almost always uses probe-rs; the sensor firmware is where UF2 earns its keep.

## Critical: this board ships with SoftDevice S140 installed

The bootloader on the Plus variant has **Nordic SoftDevice S140 7.3.0** pre-installed at `0x1000–0x26FFF` (152 KB). This means **the application must start at `0x27000`, not `0x26000`**. If you flash to `0x26000`, the bootloader silently rejects the app as invalid and stays in DFU mode (constant fast red blink, single-tap reset enters flash mode). Both `memory.x` and the `uf2conv.py --base` flag must use `0x27000`.

The SoftDevice itself is never started by our firmware (we use `nrf-sdc` instead), but it occupies the flash region either way.

## Prerequisites (one-time)

```bash
cargo install cargo-binutils
rustup component add llvm-tools-preview
```

Get `uf2conv.py` and its companion `families.json` (both required — the script reads the family table from JSON):

```bash
curl -L https://raw.githubusercontent.com/microsoft/uf2/master/utils/uf2conv.py -o uf2conv.py
curl -L https://raw.githubusercontent.com/microsoft/uf2/master/utils/uf2families.json -o uf2families.json
```

Note: `elf2uf2-rs` does NOT work for the nRF52840 — it's RP2040-specific. Use `uf2conv.py`.

## Mount setup (one-time, per Linux system)

The user's fstab entry mounts the XIAO bootloader at `/mnt/xiao` when present (XIAO appears as `/dev/sdb` with FAT label `XIAO-BOOT`). If you need to mount manually:

```bash
sudo mkdir -p /mnt/xiao
sudo mount -o uid=$(id -u),gid=$(id -g) /dev/sdb /mnt/xiao
```

The `uid`/`gid` options are critical — without them, FAT defaults to root ownership and `cp` will get "Permission denied" even though the mountpoint directory is owned by the user.

## Build & flash

The convenience script is at `firmware/sensor/flash.sh`. From that directory:

```bash
./flash.sh
```

Which expands to:

```bash
# Build firmware
cargo build --release

# Convert ELF to raw binary
cargo objcopy --release -- -O binary firmware.bin

# Convert binary to UF2 — note --base 0x27000 (SoftDevice present, see above)
python uf2conv.py firmware.bin \
    --family 0xADA52840 \
    --base 0x27000 \
    --output firmware.uf2

# Copy to the mounted bootloader drive
cp firmware.uf2 /mnt/xiao/ && sync
```

To flash:

1. **Double-tap RESET** on the XIAO quickly — the board enters bootloader mode, USB drive mounts at `/mnt/xiao`
2. Run `./flash.sh`
3. Board flashes the UF2 and auto-reboots into the application. The mount disappears automatically.

## Critical addresses

- **UF2 family ID**: `0xADA52840` (Adafruit nRF52 series)
- **Application base address**: `0x00027000` — MBR at `0x0000–0x0FFF` + SoftDevice S140 at `0x1000–0x26FFF` precede the application
- **Application maximum length**: `868 KB` (1 MB total flash minus 4 KB MBR minus 152 KB SoftDevice)

These match the `FLASH` region in [firmware/sensor/memory.x](../firmware/sensor/memory.x). If you change one, change both.

## How to verify the bootloader expects 0x27000

Double-tap RESET to enter DFU mode, then on the mounted drive:

```bash
cat /mnt/xiao/INFO_UF2.TXT
```

If you see `SoftDevice: S140 7.3.0` in the output, the application offset is `0x27000`. If the SoftDevice line is absent, the offset would be `0x26000`. This is how we discovered the Plus variant's layout differs from the standard XIAO nRF52840.

## Suggested .gitignore additions

The following are generated locally and should not be committed:

```
/firmware.bin
/firmware.uf2
/uf2conv.py
/uf2families.json
```

(`/target` is already in `.gitignore`.)

## Troubleshooting

### `"entry point is not in mapped part of file"` from `elf2uf2-rs`
You're using the wrong tool. `elf2uf2-rs` is for RP2040, not nRF52840. Switch to `uf2conv.py`.

### `families.json: No such file or directory`
`uf2conv.py` requires `uf2families.json` (sometimes named `families.json`) in the same directory. Either rename or download from the same Microsoft UF2 repo.

### `Permission denied` when copying to mount
The mount is owned by root because the `uid`/`gid` mount options weren't passed. Remount with `-o uid=$(id -u),gid=$(id -g)`.

### Board doesn't enter bootloader mode
Double-tap must be fast (<500 ms between presses). If still no mount, try a fresh USB cable — many cables are charge-only.
