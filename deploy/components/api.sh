# The `api` component: homescope-api with its TimescaleDB and, unless
# grafana.enabled = false, its Grafana. Sourced by deploy.sh in both phases.

api_units() {
	echo homescope-db homescope-api
	if $CFG_GRAFANA_ENABLED; then
		echo homescope-grafana
	fi
}

# 0711 on the database's mount point: as a bind mount it keeps these host
# permissions (a named volume would have copied the image's), and postgres
# runs as its own uid inside, so it must be able to traverse in to PGDATA.
api_dirs() {
	echo "$CFG_DATA_DIR/timescaledb 0711"
	if $CFG_GRAFANA_ENABLED; then
		echo "$CFG_DATA_DIR/grafana 0755"
	fi
}

# The KEK wraps every per-device key in devices.key. It must never be minted by
# accident: a restored database is sealed under its *original* KEK, and a fresh
# generation 1 would fail every row. So a first deploy states which case it is.
#
# Rotation is deliberately not automated: add a generation, re-wrap every row,
# promote `current`, drop the old line. A converge script doing that could
# orphan rows.
setup_kek() {
	local secret="${SECRET_NAMES[kek]}"

	# A KEK already here: repeating the first deploy's exact command must be
	# safe, so the same file again is a no-op and --new-kek keeps what exists.
	# Only a *different* KEK is refused — replacing the one in use orphans
	# every device key.
	if secret_exists "$secret"; then
		if [[ -n $IMPORT_KEK ]]; then
			if secret_value "$secret" | cmp -s - "$IMPORT_KEK"; then
				log "KEK already imported from $IMPORT_KEK"
				return 0
			fi
			die "--import-kek given, but this host already has a different KEK. Replacing the KEK in use orphans every device key; if that is really meant, remove it first: sudo homescope secret rm kek"
		fi
		if $NEW_KEK; then
			log "This host already has a KEK; keeping it (--new-kek only mints one where none exists)"
		fi
		return 0
	fi

	if [[ -n $IMPORT_KEK ]]; then
		# The API's parser is the authority; this only catches the wrong file
		# (a dump, an admin token) before it becomes the KEK.
		if ! grep -Eq '^[[:space:]]*current[[:space:]]*=[[:space:]]*[0-9]+[[:space:]]*$' "$IMPORT_KEK" ||
			! grep -Eq '^[[:space:]]*[0-9]+[[:space:]]*=[[:space:]]*[0-9a-fA-F]{64}[[:space:]]*$' "$IMPORT_KEK"; then
			die "$IMPORT_KEK does not look like a KEK file (expected 'current = N' and 'N = <64 hex>' lines)"
		fi
		log "Importing the KEK from $IMPORT_KEK"
		secret_put "$secret" < "$IMPORT_KEK"
	elif $NEW_KEK; then
		log "Generating a new KEK (generation 1)"
		{
			echo "# homescope KEK ring — wraps the per-device keys in devices.key."
			echo "# Generated $(date -Is) by deploy.sh."
			echo "current = 1"
			echo "1 = $(openssl rand -hex 32)"
		} | secret_put "$secret"

		cat >&2 <<-EOF

			!!  A new KEK was generated. Back it up NOW, somewhere other than where
			!!  the database backups go — same drive means one theft or one failure
			!!  takes both. Losing it means re-provisioning every sensor by hand.
			!!
			!!      sudo homescope secret show kek

		EOF
	else
		die "This host has no KEK yet. Say which case this is:
    --new-kek            a fresh installation: generate one
    --import-kek FILE    restoring an installation: import its KEK, so the
                         restored database's device keys still open"
	fi
}

# The bearer token guarding /devices. Unlike the KEK it protects nothing at
# rest, so generating one is always safe; revocation is "set a new one".
setup_admin_token() {
	local secret="${SECRET_NAMES[admin-token]}"

	if secret_exists "$secret"; then
		if [[ -n $IMPORT_ADMIN_TOKEN ]]; then
			if secret_value "$secret" | cmp -s - "$IMPORT_ADMIN_TOKEN"; then
				log "Admin token already imported from $IMPORT_ADMIN_TOKEN"
				return 0
			fi
			die "--import-admin-token given, but this host already has a different one; replace it with: sudo homescope secret set admin-token"
		fi
		return 0
	fi

	if [[ -n $IMPORT_ADMIN_TOKEN ]]; then
		log "Importing the admin API token from $IMPORT_ADMIN_TOKEN"
		secret_put "$secret" < "$IMPORT_ADMIN_TOKEN"
	else
		log "Generating the admin API token"
		openssl rand -hex 32 | secret_put "$secret"
		echo "    Read it out for homescope-provision with: sudo homescope secret show admin-token" >&2
	fi
}

# The dump script, as a root-owned copy for `homescope backup`: the staged
# deploy tree belongs to the homescope user, and root executing a file that
# user can write would let anything running as homescope — a container escape
# included — choose what root runs.
#
# No timer: scheduling and keeping backups is the host's job. Its backup run
# calls `homescope backup --snapshot DIR` right before snapshotting files, so
# the two cannot race (see backup-db.sh). A timer an earlier deploy installed
# is removed.
install_backup_tool() {
	install -m 0755 -o root -g root "$SCRIPT_DIR/backup-db.sh" /usr/local/sbin/homescope-backup-db

	local unit_dir=/etc/systemd/system
	if [[ -e $unit_dir/homescope-backup-db.timer || -e $unit_dir/homescope-backup-db.service ]]; then
		log "Removing the nightly dump timer (backups are scheduled by the host now)"
		systemctl disable --now --quiet homescope-backup-db.timer 2> /dev/null || true
		rm -f "$unit_dir/homescope-backup-db.timer" "$unit_dir/homescope-backup-db.service"
		systemctl daemon-reload
	fi
}

api_root() {
	setup_kek
	setup_admin_token

	if ! $CFG_BROKER_LOCAL; then
		require_secret mqtt-api "the password of MQTT user $CFG_MQTT_API_USER"
	fi

	# 0700, root: the globals dump holds role password hashes.
	install -d -m 0700 -o root -g root "$CFG_BACKUP_DIR"
	install_backup_tool
}

# The database passwords, generated once. Each lives in two files (db.env for
# role creation at the database's first start, the service's env for the
# client), so the files are only valid as a complete set.
setup_db_passwords() {
	local db_env="$CONFIG_DIR/db.env" api_env="$CONFIG_DIR/api.env" grafana_env="$CONFIG_DIR/grafana.env"

	if [[ -f $db_env && -f $api_env && -f $grafana_env ]]; then
		return
	fi
	if [[ -f $db_env || -f $api_env || -f $grafana_env ]]; then
		die "Partial secrets state in $CONFIG_DIR — some env files exist, some are missing. Resolve manually."
	fi

	log "Generating database passwords"

	# 'local' and assignment split on purpose: 'local x="$(cmd)"' would swallow
	# cmd's exit status, and set -e could not catch an openssl failure.
	local api_db_password grafana_db_password
	api_db_password="$(generate_password)"
	grafana_db_password="$(generate_password)"

	cat > "$db_env" <<-EOF
		# Read by the postgres image ONLY on first init of the data directory.
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
		# Admin password is read by grafana only on first start (empty data dir).
		GF_SECURITY_ADMIN_PASSWORD=$(generate_password)
		GRAFANA_DB_PASSWORD=$grafana_db_password
	EOF

	chmod 600 "$db_env" "$api_env" "$grafana_env"
}

api_user() {
	local quadlets="$1"

	setup_db_passwords
	rsync -a --delete "$SCRIPT_DIR/timescaledb/init/" "$CONFIG_DIR/timescaledb-init/"

	cat > "$CONFIG_DIR/mqtt-api.env" <<-EOF
		# Generated by deploy.sh from deploy.toml; changes are overwritten.
		MQTT_HOST=$CFG_MQTT_HOST
		MQTT_PORT=$CFG_MQTT_PORT
		MQTT_USERNAME=$CFG_MQTT_API_USER
	EOF

	cp "$SCRIPT_DIR/quadlets/homescope-db.container" "$SCRIPT_DIR/quadlets/homescope-api.container" "$quadlets/"

	write_dropin "$quadlets" homescope-db <<-EOF
		[Container]
		Volume=$CFG_DATA_DIR/timescaledb:/var/lib/postgresql
		PublishPort=$CFG_DB_PUBLISH:5432
	EOF

	{
		image_lines "$CFG_IMAGE_API"
		echo "PublishPort=$CFG_API_PUBLISH:3000"
		broker_dependency
	} | write_dropin "$quadlets" homescope-api

	if $CFG_GRAFANA_ENABLED; then
		rsync -a --delete "$SCRIPT_DIR/grafana/provisioning/" "$CONFIG_DIR/grafana-provisioning/"
		rsync -a --delete "$SCRIPT_DIR/grafana/dashboards/" "$CONFIG_DIR/grafana-dashboards/"

		{
			echo "# Generated by deploy.sh from deploy.toml; changes are overwritten."
			[[ -z $CFG_GRAFANA_ROOT_URL ]] || echo "GF_SERVER_ROOT_URL=$CFG_GRAFANA_ROOT_URL"
			echo "GF_SECURITY_ALLOW_EMBEDDING=$CFG_GRAFANA_ALLOW_EMBEDDING"
		} > "$CONFIG_DIR/grafana-host.env"

		# Grafana runs as uid 472 and, unlike postgres, chowns nothing itself.
		local grafana_data="$CFG_DATA_DIR/grafana"
		if [[ "$(podman unshare stat -c %u "$grafana_data")" != 472 ]]; then
			podman unshare chown 472:0 "$grafana_data"
		fi

		cp "$SCRIPT_DIR/quadlets/homescope-grafana.container" "$quadlets/"
		write_dropin "$quadlets" homescope-grafana <<-EOF
			[Container]
			Volume=$grafana_data:/var/lib/grafana
			PublishPort=$CFG_GRAFANA_PUBLISH:3000
		EOF
	fi
}
