[private]
default:
    @just --list

[private]
deps-gateway:
    podman compose -f compose.dev.yml up -d --wait mqtt-broker

[private]
deps-api:
    podman compose -f compose.dev.yml up -d --wait mqtt-broker homescope-db

# Bring up all dev dependencies (MQTT + Postgres)
deps:
    podman compose -f compose.dev.yml up -d --wait

# Run the gateway (auto-starts MQTT broker)
[working-directory('gateway')]
gateway: deps-gateway
    cargo run -p homescope-gateway

[working-directory('gateway')]
gateway-watch: deps-gateway
    watchexec -r --exts rs,toml -w . -w ../common -- cargo run -p homescope-gateway

# Run the API (auto-starts MQTT broker + Postgres)
[working-directory('api')]
api: deps-api
    cargo run -p homescope-api

[working-directory('api')]
api-watch: deps-api
    watchexec -r --exts rs,toml -w . -w ../common -- cargo run -p homescope-api

image crate:
    podman build -f {{crate}}/Containerfile -t homescope-{{crate}} .

dev: deps
    zellij --layout .dev/dev-layout.kdl

# Stop the dev stack (keeps volumes)
down:
    podman compose -f compose.dev.yml down

[private]
deps-db:
    podman compose -f compose.dev.yml up -d --wait homescope-db

# Seed the dev DB with ~90 days of fake per-minute readings (6 fake devices)
db-seed: deps-db
    podman compose -f compose.dev.yml exec -T homescope-db psql -U postgres -d homescope -v ON_ERROR_STOP=1 < deploy/timescaledb/seed.dev.sql

# Wipe all devices and readings from the dev DB (keeps schema/migrations)
db-clear: deps-db
    podman compose -f compose.dev.yml exec -T homescope-db psql -U postgres -d homescope -v ON_ERROR_STOP=1 -c 'TRUNCATE readings, devices RESTART IDENTITY'

[private]
deps-grafana:
    podman compose -f compose.dev.yml up -d --wait grafana

# Updates homescope's dashboard to deploy/grafana/dashboards/homescope.json
grafana-pull: deps-grafana
    #!/usr/bin/env bash
    set -euo pipefail
    dashboard="$(curl -sfu admin:dev http://localhost:4000/api/dashboards/uid/adl6m54 | jq -e '.dashboard | select(type == "object") | .id = null')"
    echo "$dashboard" > deploy/grafana/dashboards/homescope.json

# Wipe Grafana's internal state (UI-saved dashboards, users, prefs)
grafana-reset:
    podman compose -f compose.dev.yml rm -sf grafana
    podman volume rm --force homescope_grafana-data
    podman compose -f compose.dev.yml up -d --wait grafana

fmt:
    cargo fmt --all
    cd firmware && cargo fmt --all

# ---- static checks -----------------------------------------------------------

# Firmware crate/board pairs that must compile AND link.
#
# Neither firmware crate has a default board feature, so every invocation must
# name exactly one — there is no "just build the workspace" for firmware.
#
# receiver:xiao is absent on purpose: the XIAO's pre-flashed SoftDevice reserves
# the low 128 KB of RAM (memory-xiao.x starts RAM at 0x20020000), and the
# receiver's 512-deep packet channel is ~139 KB. Only the DB-40's full 256 KB
# fits it. Add it back here if the channel ever shrinks.
firmware_matrix := "sensor:db40 sensor:xiao receiver:db40"

# Feature powerset for homescope-common, checked on both targets.
common_features := "codec crypto serde defmt"

# Everything CI should run, cheapest-to-fail first
check: fmt-check lint check-features check-sqlx test build-firmware

# Report formatting violations without rewriting (`just fmt` rewrites)
fmt-check:
    cargo fmt --all --check
    cd firmware && cargo fmt --all --check

# Clippy over both workspaces, warnings fatal
lint: lint-host lint-firmware

# --all-targets is load-bearing: without it clippy never compiles test code, which
# is how three tests stayed broken for a week after a signature change.
[private]
lint-host: deps-db
    cargo clippy --workspace --all-targets --all-features -- -D warnings

[private]
lint-firmware:
    #!/usr/bin/env bash
    set -euo pipefail
    for pair in {{ firmware_matrix }}; do
        crate="${pair%%:*}"; board="${pair##*:}"
        echo ">>> clippy $crate / board-$board"
        cargo clippy --manifest-path firmware/Cargo.toml --target thumbv7em-none-eabi \
            -p "homescope-$crate" --features "board-$board" --all-targets -- -D warnings
    done

# Build every firmware combination.
#
# `cargo check` is NOT enough here, twice over: heapless guards capacity with
# post-monomorphization `const { assert!(N >= M) }` blocks that only fire at
# codegen, and section-overflow / linker-script errors only fire at link. Both
# pass `check` silently. Release, because that is what gets flashed — a debug
# build is larger and can overflow where release fits.
[doc("Build every firmware crate/board combination (release)")]
build-firmware:
    #!/usr/bin/env bash
    set -euo pipefail
    for pair in {{ firmware_matrix }}; do
        crate="${pair%%:*}"; board="${pair##*:}"
        echo ">>> build $crate / board-$board"
        cargo build --manifest-path firmware/Cargo.toml --target thumbv7em-none-eabi \
            --release -p "homescope-$crate" --features "board-$board"
    done

# Check homescope-common's whole feature powerset on host and firmware targets.
#
# A per-crate `cargo check -p` resolves features narrowly and will not catch a
# combination no consumer happens to select: `--features crypto,serde` was
# unbuildable for a week that way. The other half of this is `default-features =
# false` on every optional dep — without it `serde` drags in `std` and breaks
# thumbv7em, which only shows up on the ARM pass.
[doc("Check homescope-common's feature powerset on both targets")]
check-features:
    #!/usr/bin/env bash
    set -euo pipefail
    read -ra features <<< "{{ common_features }}"
    for (( mask = 0; mask < (1 << ${#features[@]}); mask++ )); do
        selected=()
        for (( i = 0; i < ${#features[@]}; i++ )); do
            if (( mask & (1 << i) )); then selected+=("${features[i]}"); fi
        done
        combo="$(IFS=,; echo "${selected[*]}")"
        for target in "" "--target thumbv7em-none-eabi"; do
            echo ">>> common [${combo:-no features}] ${target:-host}"
            # shellcheck disable=SC2086
            cargo check -q -p homescope-common --no-default-features \
                ${combo:+--features "$combo"} $target
        done
    done

# Compile the API the way the container image does.
#
# api/.sqlx is keyed by a hash of each query *string*, so editing or even
# reformatting SQL orphans the entry — the dev build keeps working against the
# live database while the image build breaks. Run `just sqlx-prepare` to fix.
[doc("Compile the API offline, the way the container image does")]
check-sqlx:
    SQLX_OFFLINE=true cargo check -p homescope-api --all-targets

# Regenerate api/.sqlx after changing a query (needs a migrated dev DB)
[working-directory('api')]
sqlx-prepare: deps-db
    cargo sqlx prepare -- --all-targets

# All tests, both workspaces.
#
# --all-features matters: `cargo test -p homescope-common` on its own reports 0
# tests, because every interesting module is behind a feature gate. The firmware
# workspace has no tests (no_std binaries).
[doc("Run all tests in both workspaces")]
test: deps-db
    cargo test --workspace --all-features
