#!/bin/sh
# Daily backup: ops backup (RPC; daemon stops Neo4j, dumps, restarts) into a dated dir,
# then rsync the archive dir to inonono. Exit non-zero on any failure so the timer unit shows failed.
set -eu
dest=/var/lib/anamnesis/backups/$(date -u +%Y-%m-%dT%H-%M-%SZ)
mkdir -p "$dest"
node /opt/anamnesis/dist/anamnesis-ops.mjs backup "$dest" | tee "$dest/backup-result.json"
grep -q '"error"' "$dest/backup-result.json" && { echo "backup reported error" >&2; exit 1; }
rsync -a --delete-after -e "ssh -i /etc/anamnesis/tunnel_ed25519 -o BatchMode=yes" \
  /var/lib/anamnesis/backups/ inonono@100.113.163.45:/mnt/data/anamnesis/backups/pve-vm/
# Keep 14 local copies.
ls -1d /var/lib/anamnesis/backups/*/ | head -n -14 | xargs -r rm -rf
