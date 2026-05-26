#!/bin/bash
# Build the receiver firmware and convert to a UF2 image for mass-storage flashing.
#
# The receiver normally lives at the dev bench with the probe attached, so
# `cargo run` (probe-rs via firmware/.cargo/config.toml) is the usual flow.
# This script is here for parity with the sensor, in case the probe is
# unavailable. See docs/flashing.md for the double-tap-RESET procedure.
set -e

UF2_TOOLS="$(dirname "$0")/../../tools/uf2"

cargo build --release
cargo objcopy --release -- -O binary firmware.bin
python "$UF2_TOOLS/uf2conv.py" firmware.bin \
    --family 0xADA52840 \
    --base 0x27000 \
    --output firmware.uf2
sync
