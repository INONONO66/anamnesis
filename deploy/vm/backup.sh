#!/bin/sh
# Daily backup. `ops backup` refuses a live daemon (daemon_live), so the supported lifecycle is:
# stop the daemon (systemd), take the offline dump as the anamnesis user, restart the daemon no matter what,
# then rsync the archive directory to inonono. Runs as root (see anamnesis-backup.service). Any failure -> non-zero exit.
set -eu
dest=/var/lib/anamnesis/backups/$(date -u +%Y-%m-%dT%H-%M-%SZ)
install -d -o anamnesis -g anamnesis -m 0700 "$dest"
systemctl stop anamnesis
trap 'systemctl start anamnesis' EXIT
runuser -u anamnesis -- node /opt/anamnesis/dist/anamnesis-ops.mjs backup "$dest" > "$dest/backup-result.json"
cat "$dest/backup-result.json"
grep -q '"state":"complete"' "$dest/backup-result.json" || { echo "backup did not report state=complete" >&2; exit 1; }
systemctl start anamnesis
trap - EXIT
runuser -u anamnesis -- rsync -a --delete-after -e "ssh -i /etc/anamnesis/tunnel_ed25519 -o BatchMode=yes" \
  /var/lib/anamnesis/backups/ inonono@100.113.163.45:/mnt/data/anamnesis/backups/pve-vm/
# Keep the 14 newest local copies (names are UTC timestamps, so lexical order is chronological).
find /var/lib/anamnesis/backups -mindepth 1 -maxdepth 1 -type d | sort | head -n -14 | while IFS= read -r old; do rm -rf "$old"; done
