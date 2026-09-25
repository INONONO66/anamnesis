#!/bin/sh
# Daily backup. `ops backup` refuses a live daemon (daemon_live), so the supported lifecycle is:
# stop the daemon (systemd), take the offline dump as the anamnesis user, restart the daemon no matter what,
# then mirror the archive directory to ANAMNESIS_BACKUP_MIRROR (default: the LXC bulk mount; an rsync-over-ssh
# target such as user@host:/path is also accepted). Runs as root (see anamnesis-backup.service). Any failure -> non-zero exit.
set -eu
# The archive path must not exist yet (destination_exists); the result file sits beside it.
stamp=$(date -u +%Y-%m-%dT%H-%M-%SZ)
dest=/var/lib/anamnesis/backups/$stamp
result=/var/lib/anamnesis/backups/$stamp.result.json
# Ingest oneshots carry Requires=anamnesis.service: a queued ingest job re-pulls the daemon and turns
# "systemctl stop anamnesis" into "Job canceled". Quiesce the timers and any running ingest first.
timers=$(systemctl list-units --type=timer --state=active --plain --no-legend 'anamnesis-ingest@*' | awk '{print $1}')
# Armed before the first stop: an early failure must never strand the timers or the daemon.
restore() { systemctl start anamnesis; for t in $timers; do systemctl start "$t"; done; }
trap restore EXIT
for t in $timers; do systemctl stop "$t"; done
systemctl stop 'anamnesis-ingest@*.service'
systemctl stop anamnesis
runuser -u anamnesis -- node /opt/anamnesis/dist/anamnesis-ops.mjs backup "$dest" > "$result"
cat "$result"
grep -q '"state":"complete"' "$result" || { echo "backup did not report state=complete" >&2; exit 1; }
restore
trap - EXIT
mirror=${ANAMNESIS_BACKUP_MIRROR:-/mnt/data/anamnesis/backups/pve-vm}
case "$mirror" in
  *:*) runuser -u anamnesis -- rsync -a --delete-after -e "ssh -i /etc/anamnesis/tunnel_ed25519 -o BatchMode=yes" \
         /var/lib/anamnesis/backups/ "$mirror/" ;;
  *)   install -d -o anamnesis -g anamnesis "$mirror"
       runuser -u anamnesis -- rsync -a --delete-after /var/lib/anamnesis/backups/ "$mirror/" ;;
esac
# Keep the 14 newest local copies (names are UTC timestamps, so lexical order is chronological).
find /var/lib/anamnesis/backups -mindepth 1 -maxdepth 1 -type d | sort | head -n -14 | while IFS= read -r old; do rm -rf "$old" "$old.result.json"; done
