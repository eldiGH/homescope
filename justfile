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

# Run the API (auto-starts MQTT broker + Postgres)
api: deps-api
    cargo run -p homescope-api

# Stop the dev stack (keeps volumes)
down:
    podman compose -f compose.dev.yml down
