#!/bin/sh
# Render mounts the data disk owned by root and hands keys over as JSON in
# the environment. Start as root, give the disk to the service user, write
# each key to the file its path variable names, then drop privileges for good.
set -eu

home=${COVENANT_COMPUTE_COORDINATOR_HOME:-/data}
mkdir -p "$home"
chown covenant:covenant "$home"

# The public pilot store is opt-in and separate from private coordinator state.
# Fixed paths keep an environment typo from changing ownership elsewhere.
if [ -n "${COVENANT_POOL_OPERATOR_DIR:-}" ]; then
    if [ "$COVENANT_POOL_OPERATOR_DIR" != /data/pool-operator ] \
        || [ -L /data ] || [ -L /data/pool-operator ]; then
        echo 'invalid public pool-operator directory' >&2
        exit 1
    fi
    mkdir -p /data/pool-operator
    chown covenant:covenant /data/pool-operator
    chmod 700 /data/pool-operator
fi

if [ -n "${COVENANT_COMPUTE_WORK_DIR:-}" ]; then
    if [ "$COVENANT_COMPUTE_WORK_DIR" != /data/community-work-public ] \
        || [ -L /data ] || [ -L /data/community-work-public ]; then
        echo 'invalid public community-work directory' >&2
        exit 1
    fi
    mkdir -p /data/community-work-public
    chown covenant:covenant /data/community-work-public
    chmod 700 /data/community-work-public
fi

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
    chmod 600 "$path"
}

write_key COVENANT_X402_FUNDING_KEYPAIR COVENANT_X402_FUNDING_KEYPAIR_JSON
write_key COVENANT_COMPUTE_LEASE_KEYPAIR COVENANT_COMPUTE_LEASE_KEYPAIR_JSON
write_key COVENANT_COMPUTE_LEASE_COORDINATOR_KEYPAIR COVENANT_COMPUTE_LEASE_COORDINATOR_KEYPAIR_JSON
write_key COVENANT_COMPUTE_STAKE_SLASH_KEYPAIR COVENANT_COMPUTE_STAKE_SLASH_KEYPAIR_JSON

if [ -n "${COVENANT_CANARY_MODE:-}" ]; then
    # Opt-in supervisor runs the two processes under different Unix identities.
    # A configured `disabled` mode stops admissions but still recovers old jobs.
    if [ "$home" != /data/mainnet ]; then
        echo 'canary requires existing coordinator home /data/mainnet; journal was not moved' >&2
        exit 1
    fi
    chown root:root /data
    chmod 755 /data
    chmod 700 "$home"
    exec python3 /opt/covenant/compute-canary-supervisor.py "$@"
fi
exec setpriv --reuid=covenant --regid=covenant --init-groups -- covenant-compute-coordinator "$@"
