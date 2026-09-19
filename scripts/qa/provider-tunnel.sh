#!/usr/bin/env bash
set -euo pipefail

# Capture the printed PID and stop that tunnel with kill. SSH diagnostics are
# retained in .omo/evidence/runtime-complete/g1/provider-tunnel.log.
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
log="$root/.omo/evidence/runtime-complete/g1/provider-tunnel.log"
mkdir -p "$(dirname "$log")"
# Do not attach to a shared master: its PID differs from the printed child PID.
nohup ssh -N -o BatchMode=yes -o ExitOnForwardFailure=yes \
  -o ControlMaster=no -o ControlPath=none \
  -o ServerAliveInterval=30 -o ServerAliveCountMax=3 \
  -L 18089:127.0.0.1:18089 -L 19080:127.0.0.1:19080 inonono > "$log" 2>&1 < /dev/null &
printf '%s\n' "$!"
