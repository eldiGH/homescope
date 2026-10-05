#!/bin/bash
#
# Dumps the homescope TimescaleDB (pg_dump custom format) plus the cluster
# globals (roles) from the running container. The dump is a consistent MVCC
# snapshot — the stack keeps running while it is taken.
#
#     sudo homescope backup                     on demand, e.g. before a migration
#     sudo homescope backup --snapshot DIR      for the host's own backup job
#
# Scheduling and keeping backups is the host's job, not homescope's: the host
# already backs up its files on its own schedule, and a second, clock-coupled
# schedule here could only race it. So homescope offers the one thing a file
# backup cannot do itself — a consistent snapshot of a live database — and the
# host's backup job calls it right before snapshotting its files:
#
#     homescope backup --snapshot /srv/.backup-staging/homescope
#
# --snapshot writes stable names (homescope.dump, globals.sql) into DIR,
# replacing what is there: the host's backup keeps the history. It skips
# compression: a gzip stream changes from its first differing byte on and
# defeats restic's deduplication, while an uncompressed dump of an
# append-mostly hypertable dedups almost completely — restic compresses itself.
# A non-zero exit means no new snapshot; what that means for the rest of the
# backup run is the host's decision.
#
# On demand, without --snapshot, each run writes a new timestamped, compressed
# pair into BACKUP_DIR and deletes nothing. BACKUP_DIR comes from the
# environment (`homescope backup` passes deploy.toml's backup.dir), defaulting
# to /var/lib/homescope/backups.
#
# Restore — destroys the current homescope DB, so every step is manual on
# purpose:
#
#   1. Stop writers:
#        sudo homescope stop api
#   2. Drop and recreate the database:
#        sudo homescope podman exec homescope-db psql -U postgres -c 'DROP DATABASE homescope WITH (FORCE)'
#        sudo homescope podman exec homescope-db psql -U postgres -c 'CREATE DATABASE homescope OWNER api'
#   3. Prepare timescaledb (extension version must match the one the dump
#      was taken with — keep the same container image). IF NOT EXISTS because
#      the image installs the extension into template1, so step 2's database
#      already has it:
#        sudo homescope podman exec homescope-db psql -U postgres -d homescope -v ON_ERROR_STOP=1 \
#            -c 'CREATE EXTENSION IF NOT EXISTS timescaledb' -c 'SELECT timescaledb_pre_restore()'
#   4. Restore the dump:
#        sudo homescope podman exec -i homescope-db pg_restore -U postgres -d homescope < <file>.dump
#   5. Finish timescaledb bookkeeping:
#        sudo homescope podman exec homescope-db psql -U postgres -d homescope -c 'SELECT timescaledb_post_restore()'
#   6. Restart writers — a newer API applies its pending migrations on start:
#        sudo homescope start api
#
# The globals file is only needed when restoring onto a fresh cluster whose
# roles were not created by deploy.sh; a cluster it initialised already has
# them (with new passwords — do not restore the old ones over them).
#
# NOT IN THESE BACKUPS, ON PURPOSE: the KEK. devices.key holds every sensor's
# AEAD key wrapped under it, so a dump plus the KEK is the whole fleet, and a
# dump alone is inert — which is the entire point, and only holds while the two
# are stored apart. Back the KEK up separately, off this machine:
#
#     sudo homescope secret show kek
#
# A restore onto a fresh machine needs both: deploy.sh --import-kek FILE. Without the KEK the rows survive
# but no device key can be opened, and every sensor must be re-provisioned by
# hand.

set -euo pipefail

HOMESCOPE_USER="homescope"
CONTAINER="homescope-db"
DATABASE="homescope"
BACKUP_DIR="${BACKUP_DIR:-/var/lib/homescope/backups}"
SNAPSHOT_DIR=""

case "${1:-}" in
	--snapshot)
		if [[ -z ${2:-} || $2 != /* ]]; then
			echo "usage: $0 [--snapshot ABSOLUTE_DIR]" >&2
			exit 2
		fi
		SNAPSHOT_DIR="$2"
		;;
	"") ;;
	*)
		echo "usage: $0 [--snapshot ABSOLUTE_DIR]" >&2
		exit 2
		;;
esac

log() {
	echo ">>> $*"
}

die() {
	echo "ERROR: $*" >&2
	exit 1
}

# Rootless podman lives under the homescope user. runuser rather than sudo:
# this also runs from a root systemd service, and runuser is always there.
# XDG_RUNTIME_DIR is how podman finds the user's runtime state.
homescope_podman() {
	runuser -u "$HOMESCOPE_USER" -- \
		env XDG_RUNTIME_DIR="/run/user/$(id -u "$HOMESCOPE_USER")" podman "$@"
}

[[ $EUID -eq 0 ]] || die "This script must run as root: sudo $0"

# Run from a directory $HOMESCOPE_USER can reach. sudo keeps the caller's cwd,
# and rootless podman re-execs inside a user namespace where the child chdir()s
# back to it — from a 0700 home dir that fails with "cannot chdir to <dir>:
# Permission denied" before podman does any work. Every path here is absolute.
cd /

homescope_podman exec "$CONTAINER" pg_isready -U postgres > /dev/null \
	|| die "Container $CONTAINER is not running or postgres is not ready"

if [[ -n $SNAPSHOT_DIR ]]; then
	out_dir="$SNAPSHOT_DIR"
	dump="$out_dir/$DATABASE.dump"
	globals="$out_dir/globals.sql"
	compression=(-Z0)
else
	out_dir="$BACKUP_DIR"
	stamp="$(date +%Y%m%d-%H%M%S)"
	dump="$out_dir/$DATABASE-$stamp.dump"
	globals="$out_dir/globals-$stamp.sql"
	compression=()
fi

# Created 0700, root-owned, when missing: the globals dump holds role password
# hashes. An existing directory is left as it is — with --snapshot it is the
# host's, and its permissions are the host's decision.
[[ -d $out_dir ]] || install -d -m 0700 -o root -g root "$out_dir"

# Dumps land in .part files and are only renamed after validation, so an
# interrupted or failed run can never leave a plausible-looking backup.
trap 'rm -f "$dump.part" "$globals.part"' EXIT

log "Dumping database $DATABASE"
homescope_podman exec "$CONTAINER" pg_dump -U postgres -Fc "${compression[@]}" "$DATABASE" > "$dump.part"

# pg_restore --list reads only the archive's table of contents and exits; the
# `cat` drains the rest of stdin. Without it podman fails writing the remainder
# into a closed pipe and reports an error although the archive was fine — a
# failure that appears only once dumps outgrow a pipe buffer.
log "Validating archive"
homescope_podman exec -i "$CONTAINER" sh -c 'pg_restore --list > /dev/null && cat > /dev/null' < "$dump.part"
mv "$dump.part" "$dump"

log "Dumping cluster globals (roles)"
homescope_podman exec "$CONTAINER" pg_dumpall -U postgres --globals-only > "$globals.part"
mv "$globals.part" "$globals"

chmod 600 "$dump" "$globals"

log "Done: $dump ($(du -h "$dump" | cut -f1))"
log "      $globals"
