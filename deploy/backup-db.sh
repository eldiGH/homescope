#!/bin/bash
#
# On-demand backup of the homescope TimescaleDB (e.g. before a migration).
# Dumps the database (pg_dump custom format) plus cluster globals (roles)
# from the running container into $BACKUP_DIR. Each run creates a new
# timestamped pair; nothing is ever deleted. The dump is a consistent
# MVCC snapshot — the stack can keep running while it is taken.
#
#     sudo ./deploy/backup-db.sh
#
# Restore — destroys the current homescope DB, so every step is manual on
# purpose. Run the podman commands as the homescope user (same wrapping as
# homescope_podman below):
#
#   1. Stop writers:
#        systemctl --user -M homescope@ stop api.service
#   2. Drop and recreate the database:
#        podman exec homescope-db psql -U postgres -c 'DROP DATABASE homescope WITH (FORCE)'
#        podman exec homescope-db psql -U postgres -c 'CREATE DATABASE homescope OWNER api'
#   3. Prepare timescaledb (extension version must match the one the dump
#      was taken with — keep the same container image):
#        podman exec homescope-db psql -U postgres -d homescope \
#            -c 'CREATE EXTENSION timescaledb' -c 'SELECT timescaledb_pre_restore()'
#   4. Restore the dump:
#        podman exec -i homescope-db pg_restore -U postgres -d homescope < <file>.dump
#   5. Finish timescaledb bookkeeping:
#        podman exec homescope-db psql -U postgres -d homescope -c 'SELECT timescaledb_post_restore()'
#   6. Restart writers:
#        systemctl --user -M homescope@ start api.service
#
# The globals-<stamp>.sql file is only needed when restoring onto a fresh
# cluster (roles/passwords); on the existing cluster the roles already exist.

set -euo pipefail

HOMESCOPE_USER="homescope"
CONTAINER="homescope-db"
DATABASE="homescope"
BACKUP_DIR="/var/lib/homescope/backups"

log() {
	echo ">>> $*"
}

die() {
	echo "ERROR: $*" >&2
	exit 1
}

# Rootless podman lives under the homescope user; plain sudo -u does not set
# XDG_RUNTIME_DIR, without which podman cannot find its runtime state.
homescope_podman() {
	sudo -u "$HOMESCOPE_USER" XDG_RUNTIME_DIR="/run/user/$(id -u "$HOMESCOPE_USER")" podman "$@"
}

[[ $EUID -eq 0 ]] || die "This script must run as root: sudo $0"

homescope_podman exec "$CONTAINER" pg_isready -U postgres > /dev/null \
	|| die "Container $CONTAINER is not running or postgres is not ready"

stamp="$(date +%Y%m%d-%H%M%S)"
dump="$BACKUP_DIR/$DATABASE-$stamp.dump"
globals="$BACKUP_DIR/globals-$stamp.sql"

# 700 + root-owned: the globals dump contains role password hashes.
mkdir -p "$BACKUP_DIR"
chmod 700 "$BACKUP_DIR"

# Dumps land in .part files and are only renamed after validation, so an
# interrupted or failed run can never leave a plausible-looking backup.
trap 'rm -f "$dump.part" "$globals.part"' EXIT

log "Dumping database $DATABASE"
homescope_podman exec "$CONTAINER" pg_dump -U postgres -Fc "$DATABASE" > "$dump.part"

log "Validating archive"
homescope_podman exec -i "$CONTAINER" pg_restore --list > /dev/null < "$dump.part"
mv "$dump.part" "$dump"

log "Dumping cluster globals (roles)"
homescope_podman exec "$CONTAINER" pg_dumpall -U postgres --globals-only > "$globals.part"
mv "$globals.part" "$globals"

chmod 600 "$dump" "$globals"

log "Done: $dump ($(du -h "$dump" | cut -f1))"
log "      $globals"
