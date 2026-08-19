#!/bin/bash
#
# Idempotent deployment script for the homescope stack.
# Safe to rerun at any time — it converges the machine to the state
# described by this repo (deploy/). Typical update flow:
#
#     git pull && sudo ./deploy/deploy.sh
#
# Runs in two phases: root does only what needs root (user creation, udev
# rule, staging the deploy tree into $STAGING_DIR), then drops to
# $HOMESCOPE_USER for everything else, so all deploy-managed files get the
# right owner at creation time. In particular,
# nothing here may ever chown into ~/.local/share/containers — podman's
# storage holds files owned by subuids (the containers' own users), and a
# recursive chown corrupts every image and volume in it.
#
# Secrets are generated once and never overwritten on reruns.
#
# Two secrets are exceptions to "secrets live in $CONFIG_DIR" — the KEK and the
# admin API token. Both are podman secrets, so they reach the API as tmpfs files
# under /run/secrets rather than as environment variables. See setup_kek and
# setup_admin_token.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

HOMESCOPE_USER="homescope"
HOMESCOPE_DIR="/var/lib/homescope"
CONFIG_DIR="$HOMESCOPE_DIR/.config/homescope"
QUADLET_DIR="$HOMESCOPE_DIR/.config/containers/systemd"
STAGING_DIR="$HOMESCOPE_DIR/deploy-src"

# Names of the podman secrets (see setup_kek / setup_admin_token).
KEK_SECRET="homescope-kek"
ADMIN_TOKEN_SECRET="homescope-admin-token"

MIN_PODMAN_VERSION="4.5" # 4.4 for quadlets, 4.5 for `podman secret exists`

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

# 32 bytes as 64 hex chars — the width homescope-api's KEK parser requires.
generate_kek() {
	openssl rand -hex 32
}

# 32 bytes as 64 hex chars. Deliberately not a UUID: v4 has ample entropy, but
# UUID is a format for uniqueness rather than unpredictability and not every
# generator on the path is cryptographic. Same effort, one fewer question.
generate_admin_token() {
	openssl rand -hex 32
}

### Root phase ################################################################

preflight_checks() {
	local tool
	for tool in podman rsync openssl udevadm loginctl runuser; do
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

# The homescope user usually cannot read the git checkout (home dirs are 700
# on current Raspberry Pi OS, and path resolution needs traversal rights on
# every ancestor), so root — which can read anything — stages a copy that
# homescope owns. The copy is kept after the deploy: --delete keeps it
# converged, it doubles as a record of what the last deploy shipped, and a
# cleanup-on-exit would be one more thing to go wrong mid-failure.
stage_deploy_tree() {
	log "Staging deploy tree to $STAGING_DIR"

	rsync -a --delete --chown "$HOMESCOPE_USER:$HOMESCOPE_USER" \
		"$SCRIPT_DIR/" "$STAGING_DIR/"
}

# Shell functions don't survive into a child process on their own; export -f
# carries them through the environment, so the runuser'd bash below runs the
# exact functions defined in this file — no flags, no second script.
drop_to_homescope() {
	export STAGING_DIR HOMESCOPE_USER HOMESCOPE_DIR CONFIG_DIR QUADLET_DIR \
		KEK_SECRET ADMIN_TOKEN_SECRET
	export -f log die generate_password generate_kek generate_admin_token \
		setup_secrets setup_kek setup_admin_token \
		setup_configs setup_quadlets setup_autoupdate_timer start_services \
		homescope_phase

	exec runuser -u "$HOMESCOPE_USER" -- bash -c 'set -euo pipefail; homescope_phase'
}

### Homescope phase ###########################################################

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

# The KEK wraps every per-device AEAD key stored in devices.key. It is the one
# secret that must never live in the database or in a database backup: with it,
# a leaked dump yields the whole fleet's keys; without it, the dump is inert.
#
# A podman secret rather than an env file, for two reasons. It is delivered as
# a tmpfs file under /run/secrets, so it never enters the API's environment —
# an env var would be readable from /proc/<pid>/environ by anything running as
# this user, inherited by every child process, and echoed by `podman inspect`.
# And `podman secret create -` reads stdin, so the generated key never touches
# a filesystem outside podman's own storage.
#
# Rotation is deliberately NOT automated here. It means: add a generation to
# the ring, re-wrap every devices.key row, promote it with `current`, then drop
# the old line. Doing that from a converge script would risk orphaning rows.
setup_kek() {
	if podman secret exists "$KEK_SECRET"; then
		log "KEK secret already exists, skipping"
		return
	fi

	log "Generating KEK (generation 1)"

	# Format must match homescope-api's parser: `current = N` plus one
	# `N = <64 hex>` line per generation. Generation numbers are foreign keys
	# into devices.kek_ver and are never renumbered or reused.
	{
		echo "# homescope KEK ring — wraps the per-device keys in devices.key."
		echo "# Generated $(date -Is) by deploy.sh."
		echo "current = 1"
		echo "1 = $(generate_kek)"
	} | podman secret create "$KEK_SECRET" -

	cat >&2 <<-EOF

		!!  A new KEK was generated. Back it up NOW, somewhere other than
		!!  wherever the database backups go — same drive means one theft or
		!!  one failure takes both, which is the exact scenario it defends
		!!  against. Losing it means re-provisioning every sensor by hand.
		!!
		!!      sudo podman secret inspect --showsecret \\
		!!          -f '{{.SecretData}}' $KEK_SECRET
		!!
		!!  (as $HOMESCOPE_USER — see the wrapping in backup-db.sh)

	EOF
}

# The bearer token guarding the device-management endpoints — the ones that
# mint a key, and the ones that rotate a deployed sensor's key out from under
# it. `device_addr` is broadcast in the clear in every BLE advertisement, so
# an unauthenticated rotate endpoint is a one-request denial of service against
# the whole fleet by anyone who can reach the port.
#
# A podman secret for the same reasons as the KEK: tmpfs under /run/secrets
# instead of the environment, and `podman secret create -` reads stdin so the
# token never touches a filesystem outside podman's storage.
#
# Unlike the KEK this one is NOT worth backing up — it derives nothing and
# protects nothing at rest. Revocation is exactly "generate a new one":
#
#     podman secret rm homescope-admin-token && sudo ./deploy/deploy.sh
setup_admin_token() {
	if podman secret exists "$ADMIN_TOKEN_SECRET"; then
		log "Admin token secret already exists, skipping"
		return
	fi

	log "Generating admin API token"

	# Piped, never assigned: the token stays out of shell variables and out of
	# any process's argv. homescope-api reads the first line and trims it, so
	# openssl's trailing newline is fine — but nothing else may be in here.
	generate_admin_token | podman secret create "$ADMIN_TOKEN_SECRET" -

	cat >&2 <<-EOF

		!!  A new admin API token was generated. Read it out and store it on
		!!  the workstation that runs homescope-provision:
		!!
		!!      sudo -u $HOMESCOPE_USER XDG_RUNTIME_DIR=/run/user/\$(id -u $HOMESCOPE_USER) \\
		!!          podman secret inspect --showsecret -f '{{.SecretData}}' $ADMIN_TOKEN_SECRET
		!!
		!!  The API is published on loopback only (see api.container), so reach
		!!  it through an SSH tunnel rather than over the LAN — nothing
		!!  terminates TLS in front of it yet and the token is a bearer token.

	EOF
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
	rsync -a --delete "$SCRIPT_DIR/grafana/provisioning/" "$CONFIG_DIR/grafana-provisioning/"
	rsync -a --delete "$SCRIPT_DIR/grafana/dashboards/" "$CONFIG_DIR/grafana-dashboards/"
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

start_services() {
	log "Reloading systemd and (re)starting services"

	# daemon-reload runs the quadlet generator and picks up the timer
	# drop-in — it must precede enable/restart.
	systemctl --user daemon-reload
	systemctl --user enable --now podman-auto-update.timer

	# 'restart' below reuses locally cached images and never contacts the
	# registry — without this pull pass a deploy would restart services on
	# stale code. Same oneshot unit the timer fires (incl. auto-rollback);
	# 'start' blocks until it finishes. No-op for not-yet-running containers
	# (first deploy pulls happen in the restarts below instead).
	systemctl --user start podman-auto-update.service

	systemctl --user restart \
		mosquitto.service \
		timescaledb.service \
		api.service \
		gateway.service \
		grafana.service

	log "Done. Check status with: sudo systemctl --user -M $HOMESCOPE_USER@ status <unit>"
}

homescope_phase() {
	# runuser resets HOME/USER to the target user but keeps the rest of the
	# caller's environment; systemctl --user finds the user's systemd manager
	# through XDG_RUNTIME_DIR, which still points at root's — fix it.
	XDG_RUNTIME_DIR="/run/user/$(id -u)"
	export XDG_RUNTIME_DIR

	if [[ ! -S "$XDG_RUNTIME_DIR/bus" ]]; then
		die "No user manager for $HOMESCOPE_USER at $XDG_RUNTIME_DIR — linger not active yet?"
	fi

	# Everything below reads from the root-staged copy, not the checkout.
	SCRIPT_DIR="$STAGING_DIR"

	setup_configs
	setup_secrets
	setup_kek
	setup_admin_token
	setup_quadlets
	setup_autoupdate_timer
	start_services
}

main() {
	preflight_checks
	setup_user
	setup_udev_rule
	stage_deploy_tree
	drop_to_homescope
}

main "$@"
