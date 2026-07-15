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
gateway: deps-gateway
    cargo run -p homescope-gateway

gateway-watch: deps-gateway
    watchexec -r --exts rs,toml -w gateway -w common -- cargo run -p homescope-gateway

# Run the API (auto-starts MQTT broker + Postgres)
api: deps-api
    cargo run -p homescope-api

api-watch: deps-api
    watchexec -r --exts rs,toml -w api -w common -- cargo run -p homescope-api

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
