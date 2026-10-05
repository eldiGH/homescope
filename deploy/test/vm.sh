#!/bin/bash
#
# End-to-end test of deploy/ on a throwaway Debian 13 VM (QEMU + KVM):
#
#     just test-deploy-vm [--keep] [--fast] [--dump FILE]
#
# Linting and the config tests check the scripts; only a real host checks
# that the deploy converges one. Every bug the deploy has had so far that a
# rewrite in another language would *not* have prevented — podman's pipe-only
# secret stdin, a pipe-buffer failure, a login check reading the replaced
# container's logs — was found this way. This script makes that repeatable.
#
# One fresh VM per run; the scenarios run in order on it, each starting from
# the state the previous one left, the way a real host evolves:
#
#   init + --check, a first deploy without a terminal (the banner), the
#   password prompts, the password flags and the MQTT login check, the KEK
#   guards, backup/snapshot/restore, data on a separate `nofail` disk, a boot
#   without that disk, the all-in-one shape with its local broker and ACL,
#   deselecting components, and an idempotent rerun.
#
# homescope-api and homescope-gateway are built from this working tree (x86,
# native in the VM): the run tests what you are about to push, not what CI
# published. An external broker is played by a rootful host-network Mosquitto
# with per-user ACLs, as on srv01.
#
# Needs KVM (/dev/kvm), qemu-system-x86_64, qemu-img, podman, python3, ssh,
# curl and sha512sum. The Debian cloud image is cached (checksum-verified) in
# ${XDG_CACHE_HOME:-~/.cache}/homescope/vm, with the last run's full log as
# last-run.log; everything else lives in a temporary directory removed at the
# end.
#
#   --keep        leave the VM running afterwards and print how to reach it
#   --fast        skip the two reboots (booting without the data disk)
#   --dump FILE   also restore this pg_dump archive (a production backup, say)

# Command strings handed to vm() are expanded by the VM's shell, not this one:
# their single-quoted $(…) and ~ are meant literally here.
# shellcheck disable=SC2016,SC2088

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/homescope/vm"
IMAGE_URL="https://cloud.debian.org/images/cloud/trixie/latest"
IMAGE="debian-13-genericcloud-amd64.qcow2"
TAG="vmtest"
SITE="vmtest"
D="/home/tester/homescope/deploy" # the deploy tree inside the VM

KEEP=false
FAST=false
EXTRA_DUMP=""
RUN=""
SEED_PID=""
SSH_PORT=""
STARTED="$(date +%s)"
PASSED=0
CURRENT="setup"
OUT=""
RC=0

### Output ####################################################################

say() {
	echo "$*"
	[[ -z $RUN ]] || echo "$*" >> "$RUN/test.log"
}

fail() {
	{
		echo
		echo "✗ $CURRENT: $*"
		if [[ -n $OUT ]]; then
			echo "---- last output ----"
			tail -40 <<< "$OUT"
			echo "---------------------"
		fi
	} >&2
	exit 1
}

### Host side #################################################################

usage() {
	# The header, up to the note on how vm() strings are expanded.
	sed -n '3,/^# Command strings handed/p' "${BASH_SOURCE[0]}" | sed '$d; s/^# \{0,1\}//'
	exit "${1:-0}"
}

parse_args() {
	while (($#)); do
		case "$1" in
			--keep) KEEP=true ;;
			--fast) FAST=true ;;
			--dump)
				[[ $# -ge 2 && -r $2 ]] || fail "--dump needs a readable file"
				EXTRA_DUMP="$(realpath "$2")"
				shift
				;;
			-h | --help) usage ;;
			*) fail "unknown argument: $1" ;;
		esac
		shift
	done
}

preflight() {
	local tool
	for tool in qemu-system-x86_64 qemu-img podman python3 ssh ssh-keygen curl sha512sum tar; do
		command -v "$tool" > /dev/null || fail "missing tool: $tool"
	done
	[[ -r /dev/kvm && -w /dev/kvm ]] || fail "no access to /dev/kvm (are you in the kvm group?)"
}

fetch_image() {
	mkdir -p "$CACHE_DIR"
	curl -fsSL -o "$CACHE_DIR/SHA512SUMS" "$IMAGE_URL/SHA512SUMS" ||
		fail "cannot fetch $IMAGE_URL/SHA512SUMS"
	local sums
	sums="$(grep " $IMAGE\$" "$CACHE_DIR/SHA512SUMS")" || fail "$IMAGE not listed in SHA512SUMS"
	if ! (cd "$CACHE_DIR" && sha512sum -c --quiet - <<< "$sums" > /dev/null 2>&1); then
		say "▸ downloading $IMAGE"
		curl -fsSL -o "$CACHE_DIR/$IMAGE" "$IMAGE_URL/$IMAGE"
		(cd "$CACHE_DIR" && sha512sum -c --quiet - <<< "$sums") || fail "$IMAGE checksum mismatch"
	fi
}

build_images() {
	say "▸ building homescope-api and homescope-gateway from the working tree"
	local crate
	for crate in api gateway; do
		podman build -q -f "$REPO/$crate/Containerfile" -t "localhost/homescope-$crate:$TAG" "$REPO" \
			>> "$RUN/test.log" 2>&1 || fail "building $crate (see $RUN/test.log)"
	done
	podman save -o "$RUN/images.tar" "localhost/homescope-api:$TAG" "localhost/homescope-gateway:$TAG" \
		>> "$RUN/test.log" 2>&1
}

free_port() {
	python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'
}

vm() {
	ssh -i "$RUN/id_ed25519" -p "$SSH_PORT" -o IdentitiesOnly=yes -o IdentityAgent=none \
		-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
		-o LogLevel=ERROR -o ConnectTimeout=5 -o ServerAliveInterval=15 \
		tester@127.0.0.1 "$@"
}

wait_for_ssh() {
	local deadline=$(($(date +%s) + 300))
	until vm true 2> /dev/null; do
		(($(date +%s) < deadline)) || fail "the VM did not come up on ssh within 5 minutes"
		sleep 3
	done
}

start_vm() {
	say "▸ booting a fresh Debian 13 VM"
	ssh-keygen -q -t ed25519 -N '' -C homescope-vm-test -f "$RUN/id_ed25519"
	mkdir -p "$RUN/seed"
	cat > "$RUN/seed/user-data" <<-EOF
		#cloud-config
		hostname: homescope-test
		users:
		  - name: tester
		    groups: [sudo]
		    sudo: "ALL=(ALL) NOPASSWD:ALL"
		    shell: /bin/bash
		    ssh_authorized_keys:
		      - $(cat "$RUN/id_ed25519.pub")
		package_update: true
		packages: [podman, rsync, openssl, python3, mosquitto-clients, curl]
	EOF
	printf 'instance-id: homescope-test-%s\nlocal-hostname: homescope-test\n' "$STARTED" > "$RUN/seed/meta-data"

	local seed_port
	seed_port="$(free_port)"
	python3 -m http.server "$seed_port" --bind 127.0.0.1 --directory "$RUN/seed" > /dev/null 2>&1 &
	SEED_PID=$!

	qemu-img create -q -f qcow2 -b "$CACHE_DIR/$IMAGE" -F qcow2 "$RUN/root.qcow2" 16G
	qemu-img create -q -f qcow2 "$RUN/srv.qcow2" 2G
	SSH_PORT="$(free_port)"
	qemu-system-x86_64 -enable-kvm -cpu host -smp 4 -m 4096 -name homescope-test \
		-drive "file=$RUN/root.qcow2,if=virtio" -drive "file=$RUN/srv.qcow2,if=virtio" \
		-netdev "user,id=n0,hostfwd=tcp:127.0.0.1:$SSH_PORT-:22" -device virtio-net-pci,netdev=n0 \
		-smbios "type=1,serial=ds=nocloud;s=http://10.0.2.2:$seed_port/" \
		-display none -serial "file:$RUN/console.log" -daemonize -pidfile "$RUN/qemu.pid"

	wait_for_ssh
	vm 'cloud-init status --wait > /dev/null' || fail "cloud-init failed in the VM (see $RUN/console.log)"
	# Once is enough: on a reboot, without its seed, cloud-init would take the
	# VM for a new instance and regenerate the host keys, among other things.
	vm 'sudo touch /etc/cloud/cloud-init.disabled'
	kill "$SEED_PID" 2> /dev/null || true
	SEED_PID=""
}

cleanup() {
	local status=$?
	[[ -n $SEED_PID ]] && kill "$SEED_PID" 2> /dev/null
	[[ -n $RUN && -f $RUN/test.log ]] && cp "$RUN/test.log" "$CACHE_DIR/last-run.log"

	if [[ -n $RUN ]] && $KEEP && [[ -f $RUN/qemu.pid ]]; then
		echo
		echo "VM kept running. Reach it with:"
		echo "    ssh -i $RUN/id_ed25519 -p $SSH_PORT -o IdentitiesOnly=yes -o IdentityAgent=none \\"
		echo "        -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null tester@127.0.0.1"
		echo "Stop and remove it with:"
		echo "    kill \$(cat $RUN/qemu.pid) && rm -rf $RUN"
	elif [[ -n $RUN ]]; then
		if [[ -f $RUN/qemu.pid ]]; then
			kill "$(cat "$RUN/qemu.pid")" 2> /dev/null || true
			sleep 1
		fi
		rm -rf "$RUN"
	fi
	if ((status != 0)); then
		echo "Full log: $CACHE_DIR/last-run.log" >&2
	fi
}

### In the VM #################################################################

# Runs a command in the VM; its combined output lands in $OUT (colours and
# carriage returns stripped) and its exit status in $RC. Never fails by itself.
run() {
	local started elapsed
	started="$(date +%s)"
	set +e
	OUT="$(vm "$@" 2>&1)"
	RC=$?
	set -e
	elapsed=$(($(date +%s) - started))
	printf '\n$ %s\n%s\n[exit %s, %s s]\n' "$*" "$OUT" "$RC" "$elapsed" >> "$RUN/test.log"
	OUT="$(sed 's/\x1b\[[0-9;]*m//g; s/\r//g' <<< "$OUT")"
	if ((elapsed > 60)); then
		say "  (slow: ${elapsed} s for: ${*:0:90})"
	fi
}

expect_rc() {
	[[ $RC == "$1" ]] || fail "expected exit status $1, got $RC"
}

expect() {
	grep -qE -- "$1" <<< "$OUT" || fail "expected output matching /$1/"
}

expect_not() {
	! grep -qE -- "$1" <<< "$OUT" || fail "unexpected output matching /$1/"
}

put_config() {
	vm 'sudo tee /etc/homescope/deploy.toml > /dev/null' <<< "$1"
}

server_config() {
	local data_dir="$1"
	cat <<-EOF
		components = ["api", "gateway"]
		site = "$SITE"
		data_dir = "$data_dir"

		[mqtt]
		host = "host.containers.internal"
		port = 1883
		api_user = "homescope-api"
		gateway_user = "homescope-$SITE"

		[grafana]
		publish = "127.0.0.1:4000"
		root_url = "https://grafana.$SITE.example/"
		allow_embedding = true

		[backup]
		dir = "$data_dir/backups"

		[images]
		api = "localhost/homescope-api:$TAG"
		gateway = "localhost/homescope-gateway:$TAG"
	EOF
}

all_in_one_config() {
	cat <<-EOF
		components = ["broker", "api", "gateway"]
		site = "$SITE"
		data_dir = "/srv/homescope"

		[grafana]
		publish = "127.0.0.1:4000"

		[images]
		api = "localhost/homescope-api:$TAG"
		gateway = "localhost/homescope-gateway:$TAG"
	EOF
}

api_only_config() {
	cat <<-EOF
		components = ["api"]
		data_dir = "/srv/homescope"

		[mqtt]
		host = "host.containers.internal"
		api_user = "homescope-api"

		[grafana]
		publish = "127.0.0.1:4000"

		[images]
		api = "localhost/homescope-api:$TAG"
	EOF
}

# The deploy tree, an external broker standing in for asgard's — a rootful
# quadlet on the host network, no anonymous access, per-user ACL, so it comes
# back after a reboot like srv01's — and the passwords its owner would hand
# over.
setup_vm() {
	say "▸ preparing the VM: deploy tree, external broker"
	tar -C "$REPO" -czf - deploy | vm 'rm -rf homescope && mkdir homescope && tar -C homescope -xzf -'

	API_PW="$(openssl rand -hex 12)"
	GW_PW="$(openssl rand -hex 12)"
	vm "printf '%s\n' '$API_PW' > ~/api.pw && printf '%s\n' '$GW_PW' > ~/gw.pw && printf 'wrong\n' > ~/wrong.pw"

	run "set -e
		sudo mkdir -p /opt/broker/config /opt/broker/data
		printf 'listener 1883\nallow_anonymous false\npassword_file /mosquitto/config/passwd\nacl_file /mosquitto/config/acl\npersistence true\npersistence_location /mosquitto/data/\n' |
			sudo tee /opt/broker/config/mosquitto.conf > /dev/null
		printf 'user homescope-api\ntopic read homescope/+/sensors/+/envelope\n\nuser homescope-$SITE\ntopic write homescope/$SITE/#\n' |
			sudo tee /opt/broker/config/acl > /dev/null
		sudo podman run --rm -v /opt/broker/config:/mosquitto/config docker.io/library/eclipse-mosquitto:2.0.22 sh -c \
			'mosquitto_passwd -c -b /mosquitto/config/passwd homescope-api $API_PW &&
			 mosquitto_passwd -b /mosquitto/config/passwd homescope-$SITE $GW_PW &&
			 chown -R 1883:1883 /mosquitto/config && chmod 600 /mosquitto/config/passwd /mosquitto/config/acl'
		printf '[Container]\nContainerName=external-broker\nImage=docker.io/library/eclipse-mosquitto:2.0.22\nNetwork=host\nVolume=/opt/broker/config:/mosquitto/config\nVolume=/opt/broker/data:/mosquitto/data\n[Service]\nRestart=always\n[Install]\nWantedBy=multi-user.target\n' |
			sudo tee /etc/containers/systemd/external-broker.container > /dev/null
		sudo systemctl daemon-reload
		sudo systemctl start external-broker.service"
	expect_rc 0

	# One test envelope through the local broker, as MQTT v5 so an ACL refusal
	# is reported. Topic and envelope name the same device, as a gateway's do.
	vm "cat > ~/publish.sh" <<-'EOF'
		#!/bin/bash
		# publish.sh TOPIC USER PASSWORD
		payload='{"deviceAddr":"C0FFEE000001","rssi":-70,"receivedAt":"2026-10-05T10:00:00Z","packet":"AQID"}'
		sudo homescope podman exec homescope-mqtt mosquitto_pub -V 5 -q 1 -u "$2" -P "$3" -t "$1" -m "$payload"
	EOF
	vm 'chmod +x ~/publish.sh'
}

wait_for_api() {
	local deadline=$(($(date +%s) + 180))
	until vm 'sudo homescope status 2> /dev/null | grep -q "homescope-api.service.*running"'; do
		(($(date +%s) < deadline)) || fail "homescope-api did not come up within 3 minutes"
		sleep 3
	done
}

reboot_vm() {
	vm 'sudo systemctl reboot' 2> /dev/null || true
	sleep 10
	wait_for_ssh
}

### Scenarios #################################################################

scenario() {
	CURRENT="$1"
	local started
	started="$(date +%s)"
	say "▶ $1"
	"$2"
	PASSED=$((PASSED + 1))
	say "  ✓ $(($(date +%s) - started)) s"
}

s_init() {
	run "sudo $D/deploy.sh --check"
	expect_rc 1
	expect "create it with: deploy.sh init"
	run "sudo $D/deploy.sh init server"
	expect_rc 0
	expect "Wrote /etc/homescope/deploy.toml from the server example"
	run "sudo $D/deploy.sh --check"
	expect_rc 0
	expect "Units: +homescope-db homescope-api homescope-grafana homescope-gateway"
	put_config "$(server_config /var/lib/homescope/data)"
}

s_first_deploy_without_terminal() {
	run "sudo $D/deploy.sh --new-kek"
	expect_rc 0
	expect "Generating a new KEK"
	expect "ACTION NEEDED"
	expect "secret set mqtt-api +# MQTT user homescope-api"
	expect "secret set mqtt-gateway +# MQTT user homescope-$SITE"
	expect "^ +sudo \S+/deploy\.sh$"
	expect_not "Staging deploy tree"

	# The homescope user exists now; give it the images under test.
	vm 'sudo homescope podman load' < "$RUN/images.tar" >> "$RUN/test.log" 2>&1
}

s_password_prompts() {
	run "python3 $D/test/pty-drive.py \
		--answer 'Password for homescope-api=$API_PW' \
		--answer 'Password for homescope-$SITE=$GW_PW' \
		-- sudo $D/deploy.sh"
	expect_rc 0
	expect "Set mqtt-api"
	expect "Set mqtt-gateway"
	expect "homescope-api logged in to host.containers.internal:1883 as homescope-api"
	expect "homescope-gateway did not start"

	run 'sudo ss -ltnH'
	expect "127\.0\.0\.1:4000 "
	expect "127\.0\.0\.1:4001 "
	expect "127\.0\.0\.1:5432 "
	expect_not "(0\.0\.0\.0|\*|\[::\]):(4000|4001|5432) "
}

s_password_flags() {
	run "sudo $D/deploy.sh --mqtt-api-password ~/wrong.pw"
	expect_rc 0
	expect "Setting mqtt-api from"
	expect "cannot log in to host.containers.internal:1883 as homescope-api"

	run "sudo $D/deploy.sh --mqtt-api-password ~/api.pw"
	expect_rc 0
	expect "homescope-api logged in to host.containers.internal:1883"

	run "sudo $D/deploy.sh --mqtt-api-password ~/api.pw"
	expect_rc 0
	expect "mqtt-api already set from"
}

s_kek_guards() {
	run "sudo $D/deploy.sh --new-kek"
	expect_rc 0
	expect "already has a KEK; keeping it"

	run 'sudo homescope secret show kek > ~/kek.backup && sudo homescope secret show kek | cmp - ~/kek.backup'
	expect_rc 0
	run "sudo $D/deploy.sh --import-kek ~/kek.backup"
	expect_rc 0
	expect "KEK already imported from"

	run "printf 'current = 1\n1 = %s\n' \$(openssl rand -hex 32) > ~/other.kek && sudo $D/deploy.sh --import-kek ~/other.kek"
	expect_rc 1
	expect "already has a different KEK"

	run "sudo homescope secret rm kek < /dev/null"
	expect_rc 1
	run 'sudo homescope secret show kek | cmp - ~/kek.backup'
	expect_rc 0
}

s_backup_and_restore() {
	local migrations
	migrations="$(find "$REPO/api/migrations" -name '*.sql' | wc -l)"

	run "sudo homescope psql -v ON_ERROR_STOP=1 \
		-c \"INSERT INTO devices (device_addr, name) VALUES (x'C0FFEE000001'::bigint, 'vm test')\" \
		-c \"INSERT INTO readings (time, device_addr, seq, temp_degc, rh_percent, battery_mv, rssi)
			SELECT now() - make_interval(mins => g), x'C0FFEE000001'::bigint, g, 21.5, 45, 3000, -60
			FROM generate_series(1, 5000) g\""
	expect_rc 0

	run 'sudo homescope backup'
	expect_rc 0
	local dump
	dump="$(grep -oE 'Done: \S+\.dump' <<< "$OUT" | cut -d' ' -f2)"
	[[ -n $dump ]] || fail "no dump path in the backup output"

	run "sudo homescope psql -c 'DELETE FROM readings'"
	expect_rc 0
	run 'sudo homescope restore /etc/hostname'
	expect_rc 1
	expect "is not a pg_dump archive"
	run "echo nope | sudo homescope restore $dump"
	expect_rc 1
	expect "nothing changed"
	run "sudo homescope restore $dump --yes"
	expect_rc 0
	expect "migrations applied: $migrations"
	expect "readings: 5000"
	expect "devices: 1"
	wait_for_api

	run 'sudo install -d -m 0700 /srv/.backup-staging && sudo homescope backup --snapshot /srv/.backup-staging/homescope && sudo stat -c "%a %n" /srv/.backup-staging/homescope /srv/.backup-staging/homescope/homescope.dump /srv/.backup-staging/homescope/globals.sql'
	expect_rc 0
	expect "^700 /srv/.backup-staging/homescope$"
	expect "^600 /srv/.backup-staging/homescope/homescope.dump$"
	expect "^600 /srv/.backup-staging/homescope/globals.sql$"
	run 'sudo homescope backup --snapshot relative/dir'
	expect_rc 2
	vm 'sudo rm -rf /srv/.backup-staging'

	if [[ -n $EXTRA_DUMP ]]; then
		say "  restoring $EXTRA_DUMP"
		vm 'cat > ~/extra.dump' < "$EXTRA_DUMP"
		run 'sudo homescope restore ~/extra.dump --yes'
		expect_rc 0
		expect "migrations applied: $migrations"
		grep -E '^ +(migrations|readings|devices|API):' <<< "$OUT" | while read -r line; do say "    $line"; done
		wait_for_api
	fi
}

s_data_disk() {
	run "set -e
		sudo mkfs.ext4 -q -L srv /dev/vdb
		echo 'LABEL=srv /srv ext4 defaults,noatime,nofail,x-systemd.device-timeout=10s 0 2' | sudo tee -a /etc/fstab > /dev/null
		sudo systemctl daemon-reload
		sudo mount /srv"
	expect_rc 0
	put_config "$(server_config /srv/homescope)"
	run "sudo $D/deploy.sh"
	expect_rc 0
	expect "Creating /srv/homescope"
	expect "homescope-api logged in"
	run 'sudo test -d /srv/homescope/timescaledb/18/docker'
	expect_rc 0

	run 'sudo systemctl stop srv.mount; systemctl is-active user@$(id -u homescope).service'
	expect "^inactive$"
	run 'sudo homescope status'
	expect_rc 1
	expect "user manager .* is inactive"
	run "sudo $D/deploy.sh --check"
	expect_rc 1
	expect "fstab lists but is not mounted"

	run 'sudo systemctl start user@$(id -u homescope).service && systemctl is-active srv.mount'
	expect "^active$"
	wait_for_api
}

s_boot_without_disk() {
	vm 'sudo sed -i "s/^LABEL=srv /LABEL=srv-missing /" /etc/fstab'
	reboot_vm
	run 'if mountpoint -q /srv; then echo mounted=yes; else echo mounted=no; fi; systemctl is-active user@$(id -u homescope).service'
	expect "mounted=no"
	expect "^inactive$"

	vm 'sudo sed -i "s/^LABEL=srv-missing /LABEL=srv /" /etc/fstab'
	reboot_vm
	wait_for_api
}

s_all_in_one() {
	put_config "$(all_in_one_config)"
	run "sudo $D/deploy.sh"
	expect_rc 0
	expect "Writing the broker ACL"
	expect "Writing the broker password file"
	expect "homescope-api logged in to homescope-mqtt:1883 as homescope-api"

	run "~/publish.sh homescope/$SITE/sensors/C0FFEE000001/envelope homescope-$SITE \"\$(sudo homescope secret show mqtt-gateway)\""
	expect_rc 0
	sleep 2
	run 'sudo homescope logs api --no-pager -n 20'
	expect "site=$SITE device_addr=C0FFEE000001"

	run "~/publish.sh homescope/elsewhere/sensors/C0FFEE000001/envelope homescope-$SITE \"\$(sudo homescope secret show mqtt-gateway)\""
	expect "Not authorized"
	run "~/publish.sh homescope/$SITE/sensors/C0FFEE000001/envelope homescope-$SITE wrong"
	expect "[Nn]ot authori[sz]ed"
}

s_deselect() {
	put_config "$(api_only_config)"
	run "sudo $D/deploy.sh"
	expect_rc 0
	expect "Stopping homescope-mqtt \(no longer part of this host\)"
	expect "Stopping homescope-gateway \(no longer part of this host\)"
	expect "homescope-api logged in to host.containers.internal:1883"
	run 'sudo homescope status'
	expect_not "homescope-(mqtt|gateway)"
}

s_idempotent_rerun() {
	run "sudo $D/deploy.sh"
	expect_rc 0
	expect_not "Generating|Writing the broker|Setting mqtt|Creating|Importing"
	expect "homescope-api logged in"
}

### Main ######################################################################

main() {
	parse_args "$@"
	preflight
	mkdir -p "$CACHE_DIR"
	RUN="$(mktemp -d "${TMPDIR:-/tmp}/homescope-vm.XXXXXX")"
	trap cleanup EXIT
	: > "$RUN/test.log"

	fetch_image
	build_images
	start_vm
	setup_vm

	scenario "init and --check" s_init
	scenario "first deploy without a terminal: the banner" s_first_deploy_without_terminal
	scenario "password prompts, then the login check" s_password_prompts
	scenario "password flags: wrong, right, unchanged" s_password_flags
	scenario "KEK guards" s_kek_guards
	scenario "backup, snapshot and restore" s_backup_and_restore
	scenario "data on a separate nofail disk" s_data_disk
	if $FAST; then
		say "▷ booting without the data disk — skipped (--fast)"
	else
		scenario "booting without the data disk" s_boot_without_disk
	fi
	scenario "all-in-one: local broker, ACL" s_all_in_one
	scenario "deselecting components" s_deselect
	scenario "idempotent rerun" s_idempotent_rerun

	local elapsed=$(($(date +%s) - STARTED))
	say ""
	say "All $PASSED scenarios passed in $((elapsed / 60)) min $((elapsed % 60)) s."
}

main "$@"
