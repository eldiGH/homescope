#!/bin/bash
# Build the sensor firmware and convert to a UF2 image for mass-storage flashing.
#
# Primary dev flow is `cargo run` (probe-rs via firmware/.cargo/config.toml).
# This script is the fallback for deployed units without SWD access — see
# docs/flashing.md for the double-tap-RESET bootloader procedure.
set -e

UF2_TOOLS="$(dirname "$0")/../../tools/uf2"

cargo build --release
cargo objcopy --release -- -O binary firmware.bin
python "$UF2_TOOLS/uf2conv.py" firmware.bin \
    --family 0xADA52840 \
    --base 0x27000 \
    --output firmware.uf2
sync
