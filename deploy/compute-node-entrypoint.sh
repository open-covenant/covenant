#!/bin/sh
# Render mounts the data disk owned by root. Start as root, give the node
# home to the service user, then drop privileges for good.
set -eu

home=${COVENANT_COMPUTE_NODE_HOME:-/data}
mkdir -p "$home"
chown covenant:covenant "$home"

# A node moving to a fresh disk keeps its identity, and with it the stake and
# reputation tied to that key, when COVENANT_COMPUTE_NODE_IDENTITY_B64 holds
# the base64 of its 32-byte identity.json. An identity already on the disk
# always wins, and the variable never reaches the node's environment.
if [ -n "${COVENANT_COMPUTE_NODE_IDENTITY_B64:-}" ] && [ ! -e "$home/identity.json" ]; then
    (
        umask 077
        printf '%s' "$COVENANT_COMPUTE_NODE_IDENTITY_B64" | base64 -d > "$home/identity.json"
    )
    chown covenant:covenant "$home/identity.json"
fi
unset COVENANT_COMPUTE_NODE_IDENTITY_B64

exec setpriv --reuid=covenant --regid=covenant --init-groups -- covenant-compute-node "$@"
