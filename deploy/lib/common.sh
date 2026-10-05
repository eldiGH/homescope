# Shared by deploy.sh (both phases) and the `homescope` admin command.
# Sourced, never executed.

HOMESCOPE_USER="homescope"
HOMESCOPE_DIR="/var/lib/homescope"
CONFIG_DIR="$HOMESCOPE_DIR/.config/homescope"
QUADLET_DIR="$HOMESCOPE_DIR/.config/containers/systemd"
STAGING_DIR="$HOMESCOPE_DIR/deploy-src"
HOMESCOPE_ETC="/etc/homescope"
DEFAULT_CONFIG_FILE="$HOMESCOPE_ETC/deploy.toml"
LIB_INSTALL_DIR="/usr/local/lib/homescope"

# Every podman secret homescope uses, by the short name `homescope secret`
# takes. Kept in one place so deploy.sh and the admin command cannot disagree.
declare -A SECRET_NAMES=(
	[kek]="homescope-kek"
	[admin-token]="homescope-admin-token"
	[mqtt-api]="homescope-mqtt-api"
	[mqtt-gateway]="homescope-mqtt-gateway"
	[mqtt-passwd]="homescope-mqtt-passwd"
	[mqtt-acl]="homescope-mqtt-acl"
)

log() {
	echo ">>> $*"
}

warn() {
	echo "WARNING: $*" >&2
}

die() {
	echo "ERROR: $*" >&2
	exit 1
}

# Run a command as the homescope user, from root.
#
# - `cd /`: runuser keeps the caller's cwd, usually a 0700 home dir the
#   homescope user cannot traverse, and rootless podman re-execs into a user
#   namespace that chdir()s back to it — dying with "cannot chdir … Permission
#   denied" before doing anything.
# - XDG_RUNTIME_DIR: runuser resets HOME and USER but keeps the rest of root's
#   environment; podman and `systemctl --user` find the user's runtime state
#   through this variable.
as_homescope() {
	(cd / && runuser -u "$HOMESCOPE_USER" -- \
		env XDG_RUNTIME_DIR="/run/user/$(id -u "$HOMESCOPE_USER")" "$@")
}

homescope_podman() {
	as_homescope podman "$@"
}

secret_exists() {
	homescope_podman secret exists "$1"
}

# Prints a secret's plaintext, byte for byte. For copying into a container's
# stdin, a backup or a comparison — never into a variable that outlives the
# call site. `head -c -1` drops the newline podman's --format appends after
# the data, so `homescope secret show kek > file` reproduces the file exactly.
secret_value() {
	homescope_podman secret inspect --showsecret -f '{{.SecretData}}' "$1" | head -c -1
}

# Creates or replaces a secret from stdin, so the value never appears in argv.
# Extra arguments (--label k=v) go to `podman secret create`.
#
# Through `cat`: `podman secret create -` accepts stdin only when it is a pipe
# and rejects a redirected file ("data must be passed into stdin"), which is
# exactly what `--import-kek FILE` and `homescope secret set x < file` pass.
secret_put() {
	local name="$1"
	shift
	cat | homescope_podman secret create --replace "$@" "$name" - > /dev/null
}

generate_password() {
	openssl rand -hex 24
}

# Loads CFG_* from a deploy.toml. config.py validates and shell-quotes every
# value, so the eval only ever sees plain assignments.
load_config() {
	local file="$1" lib_dir="$2" assignments
	assignments="$(python3 "$lib_dir/config.py" "$file")" || exit 1
	eval "$assignments"
}
