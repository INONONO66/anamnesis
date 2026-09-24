#!/bin/sh
# Prints status.workers and exits 1 when either lane reports an error or the daemon is unreachable.
set -eu
# Outside the systemd unit the daemon env is not inherited; load it so the client finds the runtime root.
if [ -z "${ANAMNESIS_RUNTIME_ROOT:-}" ] && [ -r /etc/anamnesis/anamnesis.env ]; then set -a; # shellcheck source=/dev/null
  . /etc/anamnesis/anamnesis.env; set +a; fi
out=$(runuser -u anamnesis -- node /opt/anamnesis/dist/anamnesis-ops.mjs status) || { echo "daemon unreachable" >&2; exit 1; }
printf '%s\n' "$out" | node -e '
let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const j=JSON.parse(s);const w=j.workers??j.result?.workers;console.log(JSON.stringify(w));
const bad=(w?.embedding?.last_error)||(w?.extraction?.state==="unconfigured")||(w?.extraction?.last_error);process.exit(bad?1:0);});'
