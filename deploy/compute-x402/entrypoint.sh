#!/bin/sh
set -eu
task_data=${COVENANT_X402_DATA:-/data}
case "$task_data" in /*) ;; *) echo 'Persistent data path must be absolute' >&2; exit 1;; esac
mkdir -p "$task_data"
chown node:node "$task_data"
chmod 700 "$task_data"
exec setpriv --reuid=node --regid=node --init-groups -- node /opt/covenant/deploy/compute-x402/supervisor.mjs
