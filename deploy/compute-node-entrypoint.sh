#!/bin/sh
# Render mounts the data disk owned by root. Start as root, give the node
# home to the service user, then drop privileges for good.
set -eu

home=${COVENANT_COMPUTE_NODE_HOME:-/data}
mkdir -p "$home"
chown covenant:covenant "$home"

exec setpriv --reuid=covenant --regid=covenant --init-groups -- covenant-compute-node "$@"
