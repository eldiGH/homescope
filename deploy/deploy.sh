#!/bin/bash
#
# Idempotent deployment script for the homescope stack.
# Safe to rerun at any time — it converges the machine to the state
# described by this repo (deploy/). Typical update flow:
#
#     git pull && sudo ./deploy/deploy.sh
#
# Secrets are generated once and never overwritten on reruns.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

HOMESCOPE_USER="homescope"
HOMESCOPE_DIR="/var/lib/homescope"
CONFIG_DIR="$HOMESCOPE_DIR/.config/homescope"
QUADLET_DIR="$HOMESCOPE_DIR/.config/containers/systemd"

MIN_PODMAN_VERSION="4.4" # quadlet support

log() {
	echo ">>> $*"
}

die() {
	echo "ERROR: $*" >&2
	exit 1
}

generate_password() {
	openssl rand -hex 24
}

homescope_systemctl() {
	systemctl --user -M "$HOMESCOPE_USER@" "$@"
}

preflight_checks() {
	local tool
	for tool in podman rsync openssl udevadm loginctl; do
		if ! command -v "$tool" > /dev/null; then
			die "Missing required tool: $tool"
		fi
	done

	local podman_version
	podman_version="$(podman version --format '{{.Client.Version}}')"
	if [[ "$(printf '%s\n' "$MIN_PODMAN_VERSION" "$podman_version" | sort -V | head -n1)" != "$MIN_PODMAN_VERSION" ]]; then
		die "podman >= $MIN_PODMAN_VERSION required, found $podman_version"
	fi

	if [[ $EUID -ne 0 ]]; then
		die "This script must run as root: sudo $0"
	fi
}

setup_user() {
	if ! id "$HOMESCOPE_USER" &> /dev/null; then
		log "Creating user $HOMESCOPE_USER"
		useradd --create-home --home-dir "$HOMESCOPE_DIR" --shell /bin/bash "$HOMESCOPE_USER"
	fi

	# Rootless podman is unusable without subuid/subgid ranges; useradd
	# normally allocates them, but not on every distro/config.
	if ! grep -q "^$HOMESCOPE_USER:" /etc/subuid; then
		log "Allocating subuid/subgid ranges"
		usermod --add-subuids 200000-265535 --add-subgids 200000-265535 "$HOMESCOPE_USER"
	fi

	# The group owning serial devices is distro-specific (Debian: dialout, Arch: uucp).
	local serial_group
	if getent group dialout > /dev/null; then
		serial_group="dialout"
	elif getent group uucp > /dev/null; then
		serial_group="uucp"
	else
		die "No serial group (dialout/uucp) found"
	fi

	usermod -aG "$serial_group" "$HOMESCOPE_USER"

	# Linger starts the user's systemd manager immediately and keeps it (and
	# thus all services) running without a login session. It must come AFTER
	# the group changes: processes only get supplementary groups present at
	# start, and everything (podman, containers) inherits them from this
	# manager. If a group is ever added while the manager is already running:
	#     systemctl restart user@$(id -u homescope).service
	loginctl enable-linger "$HOMESCOPE_USER"
}

setup_secrets() {
	local db_env="$CONFIG_DIR/db.env"
	local api_env="$CONFIG_DIR/api.env"
	local grafana_env="$CONFIG_DIR/grafana.env"

	if [[ -f "$db_env" && -f "$api_env" && -f "$grafana_env" ]]; then
		log "Secrets already generated, skipping"
		return
	fi

	# The api/grafana DB passwords each live in TWO files (db.env for role
	# creation at first DB init, service env for the client), so the files
	# are only valid as a complete set. Never regenerate just one.
	if [[ -f "$db_env" || -f "$api_env" || -f "$grafana_env" ]]; then
		die "Partial secrets state in $CONFIG_DIR — some env files exist, some are missing. Resolve manually."
	fi

	log "Generating secrets"

	# 'local' and assignment are split on purpose: 'local x="$(cmd)"' would
	# swallow cmd's exit status and set -e could not catch an openssl failure.
	local api_db_password grafana_db_password
	api_db_password="$(generate_password)"
	grafana_db_password="$(generate_password)"

	cat > "$db_env" <<-EOF
		# Read by the postgres image ONLY on first init of the data volume.
		# Editing these later does NOT change any database password.
		POSTGRES_PASSWORD=$(generate_password)
		API_DB_PASSWORD=$api_db_password
		GRAFANA_DB_PASSWORD=$grafana_db_password
	EOF

	cat > "$api_env" <<-EOF
		DB_USER=api
		DB_PASSWORD=$api_db_password
	EOF

	cat > "$grafana_env" <<-EOF
		# Admin password is read by grafana only on first start (empty data volume).
		GF_SECURITY_ADMIN_PASSWORD=$(generate_password)
		GRAFANA_DB_PASSWORD=$grafana_db_password
	EOF

	chmod 600 "$CONFIG_DIR"/*.env
}

setup_configs() {
	log "Syncing container configs"

	mkdir -p "$CONFIG_DIR"
	# 755, not 700: containers that drop privileges (postgres, grafana) access
	# mounts below this dir as non-root mapped uids and need traversal rights.
	# Secrets are protected by the 600 mode on the env files themselves.
	chmod 755 "$CONFIG_DIR"

	cp "$SCRIPT_DIR/mosquitto/mosquitto.conf" "$CONFIG_DIR/mosquitto.conf"

	rsync -a --delete "$SCRIPT_DIR/timescaledb/init/" "$CONFIG_DIR/timescaledb-init/"
	rsync -a --delete "$SCRIPT_DIR/grafana/provisioning/datasources/" "$CONFIG_DIR/grafana-provisioning/"
}

setup_udev_rule() {
	local rule="99-homescope-receiver.rules"

	# cmp guard skips the udev reload when the rule is unchanged;
	# the copy itself would be harmless to repeat.
	if ! cmp -s "$SCRIPT_DIR/udev/$rule" "/etc/udev/rules.d/$rule"; then
		log "Installing udev rule $rule"
		cp "$SCRIPT_DIR/udev/$rule" /etc/udev/rules.d/
		udevadm control --reload-rules
		udevadm trigger --subsystem-match=tty
	fi
}

setup_quadlets() {
	log "Syncing quadlets"

	mkdir -p "$QUADLET_DIR"
	rsync -a --delete "$SCRIPT_DIR/quadlets/" "$QUADLET_DIR/"
}

setup_autoupdate_timer() {
	log "Installing podman-auto-update timer override"

	local dropin_dir="$HOMESCOPE_DIR/.config/systemd/user/podman-auto-update.timer.d"
	mkdir -p "$dropin_dir"
	cp "$SCRIPT_DIR/systemd/podman-auto-update.timer.d/override.conf" "$dropin_dir/"
}

# Single ownership pass instead of per-step chowns: every earlier step runs as
# root and may leave root-owned files inside homescope's home.
fix_ownership() {
	chown -R "$HOMESCOPE_USER:$HOMESCOPE_USER" "$HOMESCOPE_DIR"
}

start_services() {
	log "Reloading systemd and (re)starting services"

	# daemon-reload runs the quadlet generator and picks up the timer
	# drop-in — it must precede enable/restart.
	homescope_systemctl daemon-reload
	homescope_systemctl enable --now podman-auto-update.timer

	# 'restart' below reuses locally cached images and never contacts the
	# registry — without this pull pass a deploy would restart services on
	# stale code. Same oneshot unit the timer fires (incl. auto-rollback);
	# 'start' blocks until it finishes. No-op for not-yet-running containers
	# (first deploy pulls happen in the restarts below instead).
	homescope_systemctl start podman-auto-update.service

	homescope_systemctl restart \
		mosquitto.service \
		timescaledb.service \
		api.service \
		gateway.service \
		grafana.service

	log "Done. Check status with: sudo systemctl --user -M $HOMESCOPE_USER@ status <unit>"
}

main() {
	preflight_checks
	setup_user
	setup_configs
	setup_secrets
	setup_udev_rule
	setup_quadlets
	setup_autoupdate_timer
	fix_ownership
	start_services
}

main "$@"
