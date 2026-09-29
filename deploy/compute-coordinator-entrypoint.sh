#!/bin/sh
# Render mounts the data disk owned by root and hands keys over as JSON in
# the environment. Start as root, give the disk to the service user, write
# each key to the file its path variable names, then drop privileges for good.
set -eu

home=${COVENANT_COMPUTE_COORDINATOR_HOME:-/data}
mkdir -p "$home"
chown covenant:covenant "$home"

# write_key PATH_VAR JSON_VAR: writes $JSON_VAR to the file $PATH_VAR names,
# readable by the service user only, and keeps the key out of the
# coordinator's own environment.
write_key() {
    eval "path=\${$1:-}"
    eval "json=\${$2:-}"
    unset "$2"
    if [ -z "$path" ] || [ -z "$json" ]; then
        return 0
    fi
    mkdir -p "$(dirname "$path")"
    (umask 077 && printf '%s' "$json" > "$path")
    chown covenant:covenant "$path"
}

write_key COVENANT_X402_FUNDING_KEYPAIR COVENANT_X402_FUNDING_KEYPAIR_JSON
write_key COVENANT_COMPUTE_LEASE_KEYPAIR COVENANT_COMPUTE_LEASE_KEYPAIR_JSON
write_key COVENANT_COMPUTE_LEASE_COORDINATOR_KEYPAIR COVENANT_COMPUTE_LEASE_COORDINATOR_KEYPAIR_JSON
write_key COVENANT_COMPUTE_STAKE_SLASH_KEYPAIR COVENANT_COMPUTE_STAKE_SLASH_KEYPAIR_JSON

exec setpriv --reuid=covenant --regid=covenant --init-groups -- covenant-compute-coordinator "$@"
