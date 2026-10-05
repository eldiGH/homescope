#!/bin/bash
#
# Converges this host to /etc/homescope/deploy.toml. Safe to rerun at any time:
#
#     git pull && sudo ./deploy/deploy.sh
#
# The config file says what the host *is*: its components (broker, api,
# gateway), its site, where data lives, which MQTT broker to use. Flags only
# request one-off actions and never change what the host is:
#
#     sudo ./deploy/deploy.sh init <all-in-one|server|gateway>
#         start a new host's config from deploy/examples/
#     sudo ./deploy/deploy.sh --check
#         validate the config and show the plan; changes nothing
#     sudo ./deploy/deploy.sh --new-kek
#         first deploy of a fresh installation
#     sudo ./deploy/deploy.sh --import-kek FILE [--import-admin-token FILE]
#         first deploy of a restored installation
#     --config PATH   another config file than /etc/homescope/deploy.toml
#
# Two phases. Root does only what needs root — the service user, the udev
# rule, data directories, secrets, system units, staging the deploy tree —
# then drops to the homescope user for the rest, so everything created there
# has the right owner from the start. Nothing here may ever chown into
# ~/.local/share/containers: podman's storage holds files owned by subuids
# (the containers' own users), and a recursive chown corrupts every image in it.
#
# Secrets never pass through the config file: they are podman secrets, mounted
# into the containers as files under /run/secrets. Day-to-day operation goes
# through the `homescope` admin command this script installs
# (`sudo homescope help`). Design: docs/design/deployment-topology.md.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib/common.sh"
for component_file in "$SCRIPT_DIR"/components/*.sh; do
	source "$component_file"
done

# 5.0: quadlet drop-in directories (secret --replace is 4.7, labels 4.3).
MIN_PODMAN_VERSION="5.0"

CONFIG_FILE="$DEFAULT_CONFIG_FILE"
NEW_KEK=false
IMPORT_KEK=""
IMPORT_ADMIN_TOKEN=""
CHECK_ONLY=false
INIT_ROLE=""
MISSING_SECRETS=()

usage() {
	sed -n '3,22p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
	exit "${1:-0}"
}

parse_args() {
	while (($#)); do
		case "$1" in
			init)
				[[ $# -ge 2 ]] || die "init needs a role: all-in-one, server or gateway"
				INIT_ROLE="$2"
				shift
				;;
			--check) CHECK_ONLY=true ;;
			--new-kek) NEW_KEK=true ;;
			--import-kek)
				[[ $# -ge 2 ]] || die "--import-kek needs a file"
				IMPORT_KEK="$2"
				shift
				;;
			--import-admin-token)
				[[ $# -ge 2 ]] || die "--import-admin-token needs a file"
				IMPORT_ADMIN_TOKEN="$2"
				shift
				;;
			--config)
				[[ $# -ge 2 ]] || die "--config needs a path"
				CONFIG_FILE="$2"
				shift
				;;
			-h | --help) usage ;;
			*) die "unknown argument: $1 (see --help)" ;;
		esac
		shift
	done

	if $NEW_KEK && [[ -n $IMPORT_KEK ]]; then
		die "--new-kek and --import-kek exclude each other"
	fi
	local file
	for file in "$IMPORT_KEK" "$IMPORT_ADMIN_TOKEN"; do
		[[ -z $file || -r $file ]] || die "cannot read $file"
	done
}

do_init() {
	local example="$SCRIPT_DIR/examples/$INIT_ROLE.toml"
	[[ -f $example ]] || die "no such role: $INIT_ROLE (expected all-in-one, server or gateway)"
	[[ ! -e $CONFIG_FILE ]] || die "$CONFIG_FILE already exists; edit it instead"

	install -d -m 0755 "$(dirname "$CONFIG_FILE")"
	install -m 0644 "$example" "$CONFIG_FILE"
	log "Wrote $CONFIG_FILE from the $INIT_ROLE example."
	echo "    Edit it, check it with: sudo $0 --check"
	echo "    then deploy with:       sudo $0 --new-kek   (or --import-kek FILE when restoring)"
}

### Checks ####################################################################

preflight() {
	local tool
	for tool in podman rsync openssl udevadm loginctl runuser python3 systemd-analyze findmnt mountpoint awk sha256sum; do
		command -v "$tool" > /dev/null || die "Missing required tool: $tool"
	done

	python3 -c 'import sys; sys.exit(sys.version_info < (3, 11))' ||
		die "python3 >= 3.11 required (tomllib), found $(python3 --version)"

	local podman_version
	podman_version="$(podman version --format '{{.Client.Version}}')"
	if [[ "$(printf '%s\n' "$MIN_PODMAN_VERSION" "$podman_version" | sort -V | head -n1)" != "$MIN_PODMAN_VERSION" ]]; then
		die "podman >= $MIN_PODMAN_VERSION required (quadlet drop-ins), found $podman_version"
	fi
}

check_config_values() {
	if $CFG_HAS_API; then
		systemd-analyze calendar "$CFG_BACKUP_ON_CALENDAR" > /dev/null 2>&1 ||
			die "$CONFIG_FILE: backup.on_calendar is not a systemd calendar expression: $CFG_BACKUP_ON_CALENDAR"
	fi
}

# A data directory under a mount point that fstab lists but that is not mounted
# — a missing USB disk on a host that boots anyway thanks to `nofail` — would
# otherwise be created on the filesystem underneath, and the database would
# start empty on the SD card.
check_data_mounts() {
	local dir target best
	local dirs=("$CFG_DATA_DIR")
	$CFG_HAS_API && dirs+=("$CFG_BACKUP_DIR")

	for dir in "${dirs[@]}"; do
		best=""
		while read -r target; do
			[[ $target == / ]] && continue
			if [[ $dir == "$target" || $dir == "$target"/* ]] && ((${#target} > ${#best})); then
				best="$target"
			fi
		done < <(findmnt --fstab -n -l -o TARGET)

		if [[ -n $best ]] && ! mountpoint -q "$best"; then
			die "$dir is under $best, which fstab lists but is not mounted — refusing to create data on the filesystem underneath. Mount it first."
		fi
	done
}

### Root phase ################################################################

wait_for_user_manager() {
	local uid bus _
	uid="$(id -u "$HOMESCOPE_USER")"
	bus="/run/user/$uid/bus"
	for _ in $(seq 1 30); do
		[[ -S $bus ]] && return
		sleep 1
	done
	die "the user manager of $HOMESCOPE_USER did not come up ($bus missing)"
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

	# Linger starts the user's systemd manager now and keeps it, with every
	# container, running without a login session.
	loginctl enable-linger "$HOMESCOPE_USER"
	wait_for_user_manager
}

setup_data_dirs() {
	# Created when missing, never touched when present: the database and
	# Grafana chown their directories to their own (sub)uids, and re-owning
	# them would lock the containers out of their data.
	if [[ ! -d $CFG_DATA_DIR ]]; then
		log "Creating $CFG_DATA_DIR"
		install -d -m 0755 -o "$HOMESCOPE_USER" -g "$HOMESCOPE_USER" "$CFG_DATA_DIR"
	fi

	local component dir mode
	for component in $CFG_COMPONENTS; do
		while read -r dir mode; do
			[[ -n $dir && ! -d $dir ]] || continue
			install -d -m "$mode" -o "$HOMESCOPE_USER" -g "$HOMESCOPE_USER" "$dir"
		done < <("${component}_dirs")
	done
}

# The containers run in the homescope *user* manager, which does not load
# fstab mount units — a RequiresMountsFor= inside a quadlet would fail as
# "unit not found" at boot, or work by accident. On the system unit of that
# manager it is well defined: without the data disk the whole homescope stack
# does not start (verified by booting a VM with the disk missing).
#
# ⚠️ It does NOT show in `systemctl --failed`: a job that fails for a missing
# dependency leaves the unit *inactive* with result 'dependency', not failed.
# Monitoring has to ask `systemctl is-active user@<uid>.service` directly.
setup_mount_dependency() {
	local uid dir file content
	uid="$(id -u "$HOMESCOPE_USER")"
	dir="/etc/systemd/system/user@$uid.service.d"
	file="$dir/homescope-data.conf"
	content="$(printf '%s\n' \
		"# Written by homescope's deploy.sh: the homescope user's services keep their data under data_dir." \
		"[Unit]" \
		"RequiresMountsFor=$CFG_DATA_DIR")"

	if [[ "$(cat "$file" 2> /dev/null)" != "$content" ]]; then
		log "Tying the homescope user manager to the mount holding $CFG_DATA_DIR"
		install -d -m 0755 "$dir"
		printf '%s\n' "$content" > "$file"
		systemctl daemon-reload
	fi
}

install_admin_tools() {
	install -m 0755 -o root -g root "$SCRIPT_DIR/homescope" /usr/local/bin/homescope
	install -d -m 0755 "$LIB_INSTALL_DIR"
	install -m 0644 -o root -g root "$SCRIPT_DIR/lib/common.sh" "$SCRIPT_DIR/lib/config.py" "$LIB_INSTALL_DIR/"
	if [[ $CONFIG_FILE != "$DEFAULT_CONFIG_FILE" ]]; then
		warn "the homescope command reads $DEFAULT_CONFIG_FILE; this deploy used $CONFIG_FILE"
	fi
}

# Secrets an operator must provide (an external broker's passwords). Collected
# across components, then reported together, with the commands to set them.
# A password this deploy generated for a local broker does not count: it would
# never log in to someone else's broker.
require_secret() {
	local short="$1" description="$2" name generated
	name="${SECRET_NAMES[$short]}"
	if secret_exists "$name"; then
		generated="$(homescope_podman secret inspect \
			-f '{{index .Spec.Labels "homescope.generated"}}' "$name")"
		[[ $generated == true ]] || return 0
	fi
	MISSING_SECRETS+=("$short|$description")
}

report_missing_secrets() {
	((${#MISSING_SECRETS[@]})) || return 0

	local entry
	{
		echo "ERROR: secrets this host needs are not set yet:"
		for entry in "${MISSING_SECRETS[@]}"; do
			echo "    sudo homescope secret set ${entry%%|*}     # ${entry#*|}"
		done
		echo "Then run the deploy again."
	} >&2
	exit 1
}

# The homescope user usually cannot read the git checkout (home dirs are 0700,
# and path resolution needs traversal rights on every ancestor), so root
# stages a copy it owns. Kept afterwards: --delete keeps it converged, and it
# records what the last deploy shipped.
stage_deploy_tree() {
	log "Staging deploy tree to $STAGING_DIR"
	rsync -a --delete --chown "$HOMESCOPE_USER:$HOMESCOPE_USER" "$SCRIPT_DIR/" "$STAGING_DIR/"
}

run_user_phase() {
	# shellcheck disable=SC2163 # exports every CFG_* variable by name
	export "${!CFG_@}" STAGING_DIR
	# cd: runuser keeps the cwd, and rootless podman dies chdir()ing back into
	# a directory the homescope user cannot enter.
	cd "$STAGING_DIR"
	# shellcheck disable=SC2016 # expanded by the homescope user's shell
	runuser -u "$HOMESCOPE_USER" -- env XDG_RUNTIME_DIR="/run/user/$(id -u "$HOMESCOPE_USER")" \
		bash -c '
			set -euo pipefail
			source "$STAGING_DIR/lib/common.sh"
			source "$STAGING_DIR/lib/user-phase.sh"
			for f in "$STAGING_DIR"/components/*.sh; do source "$f"; done
			user_phase'
}

### --check ###################################################################

print_plan() {
	echo "Config: $CONFIG_FILE"
	python3 "$SCRIPT_DIR/lib/config.py" --show "$CONFIG_FILE" | sed 's/^/    /'

	# findmnt -T needs an existing path; before the first deploy, data_dir is not.
	local probe="$CFG_DATA_DIR"
	while [[ ! -e $probe ]]; do
		probe="$(dirname "$probe")"
	done
	echo "Data:   $CFG_DATA_DIR on $(findmnt -n -o SOURCE,FSTYPE -T "$probe" | tr -s ' ')"

	local units="" component
	for component in $CFG_COMPONENTS; do
		units+="$("${component}_units" | tr '\n' ' ')"
	done
	echo "Units:  $units"

	echo "Secrets:"
	if ! id "$HOMESCOPE_USER" &> /dev/null; then
		echo "    (user $HOMESCOPE_USER does not exist yet — none set)"
		return
	fi
	local short
	for short in kek admin-token mqtt-api mqtt-gateway mqtt-passwd mqtt-acl; do
		if secret_exists "${SECRET_NAMES[$short]}" 2> /dev/null; then
			echo "    $short: set"
		else
			echo "    $short: not set"
		fi
	done
}

### Main ######################################################################

main() {
	parse_args "$@"
	[[ $EUID -eq 0 ]] || die "This script must run as root: sudo $0 $*"

	if [[ -n $INIT_ROLE ]]; then
		do_init
		return
	fi

	preflight
	load_config "$CONFIG_FILE" "$SCRIPT_DIR/lib"
	check_config_values
	check_data_mounts

	if $CHECK_ONLY; then
		print_plan
		return
	fi

	setup_user
	setup_data_dirs
	setup_mount_dependency
	install_admin_tools

	local component
	for component in $CFG_COMPONENTS; do
		"${component}_root"
	done
	report_missing_secrets

	stage_deploy_tree
	run_user_phase
}

main "$@"
