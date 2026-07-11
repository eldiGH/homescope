[private]
default:
    @just --list

[private]
deps-gateway:
    podman compose -f compose.dev.yml up -d --wait mqtt-broker

[private]
deps-api:
    podman compose -f compose.dev.yml up -d --wait mqtt-broker db

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
    zellij --layout deploy/dev-layout.kdl

# Stop the dev stack (keeps volumes)
down:
    podman compose -f compose.dev.yml down

