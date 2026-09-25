# Deployment runbook

The data authority is the Neo4j database plus `~/.anamnesis/objects/`, nothing else ([00-overview](00-overview.md) invariant 1). Durability comes from `backup`/`restore` over that authority. The episode journal is a write-ahead spool precursor and a disaster-recovery convenience — it lets the originals layer be re-ingested if the graph is lost before a backup exists — not a second store of record ([10-decision-log](10-decision-log.md) D0).

## First deploy

1. Generate the git-ignored `.env` with a per-install random Neo4j password, then review the bind address and memory limits. The generator never overwrites an existing `.env` and creates it with mode 0600.
   ```sh
   bun scripts/gen-password.ts
   ```
2. Start Neo4j and wait for it to become healthy:
   ```sh
   bun run db:up
   docker compose ps
   ```
3. Apply the idempotent schema migration:
   ```sh
   set -a; . ./.env; set +a
   bun run migrate
   ```
4. Optionally configure ingestion to use `journaledRemember` so writes are spooled before graph ingest. Put its journal directory on durable storage outside the Neo4j volumes. A future stateless ingestion service should depend on `neo4j` with `condition: service_healthy`.

## Optional TCP listener

By default the daemon accepts RPC only on the Unix socket `<runtime root>/anamnesis.sock` (mode 0600), where filesystem permissions are the access control. Setting `ANAMNESIS_LISTEN` adds a TCP listener with identical framing and request handling; the socket keeps working unchanged. TCP has no filesystem check, so every TCP connection must prove possession of a bearer token before its first request.

| Variable | Meaning |
|---|---|
| `ANAMNESIS_LISTEN` | `host:port` to bind, e.g. `127.0.0.1:4400` or `[::1]:4400`. Port `0` binds an ephemeral port. The bound address is reported in the `listening` log event as `"tcp":{"host":...,"port":...}`. Unset = socket only. |
| `ANAMNESIS_LISTEN_TOKEN_FILE` | Required with `ANAMNESIS_LISTEN`. Path to a regular file (not a symlink) owned by the daemon user with mode exactly `0600`, holding one bearer token of at most 1024 bytes; surrounding whitespace is ignored. This is distinct from the installation token in `<runtime root>/token`, which `hello` still requires on every transport. |

A missing variable, unreadable or non-`0600` file, or empty token is a startup error. The daemon exits before claiming the runtime root, and the error names the variable, never the token. A TCP bind failure (for example `EADDRINUSE`) after the socket is up also exits instead of serving socket-only.

```sh
(umask 077; openssl rand -base64 32 > /etc/anamnesis/listen-token)
export ANAMNESIS_LISTEN=127.0.0.1:4400
export ANAMNESIS_LISTEN_TOKEN_FILE=/etc/anamnesis/listen-token
node dist/anamnesis-daemon.mjs
```

Wire contract on TCP: the first frame is `{"auth":{"bearer":"<token>"}}` with the same four-byte big-endian length prefix as requests. The daemon compares it in constant time and sends nothing on success; the client then continues with `hello`. A wrong, missing, or malformed first frame is answered with one error response (`id: null`, `error.data.code: "unauthorized"`) and the connection is closed. The daemon logs `{"event":"unauthorized","connection":N}` without the supplied value. The bundled client does this for you: `RpcClient.connect({ host, port, token: bearer }, installationToken)`; passing a socket path instead keeps the existing behaviour.

The listener is plain TCP without TLS. Bind to loopback or a private interface and put it behind a TLS terminator or SSH tunnel when peers are remote.

### Remote `ops` client

The `ops` CLI (`dist/anamnesis-ops.mjs`, or `app/anamnesis/ops.ts`) talks to the local socket by default. Set `ANAMNESIS_RPC_TCP=host:port` to target a daemon's TCP listener instead; every command that needs a connection (`status`, `verify`, `recall`, the `ingest-*` adapters, `backup`, `down`) then goes over TCP.

| Variable | Meaning |
|---|---|
| `ANAMNESIS_RPC_TCP` | `host:port` of the remote listener. Unset = local socket, unchanged. |
| `ANAMNESIS_RPC_TCP_TOKEN_FILE` | Listener bearer (the daemon's `ANAMNESIS_LISTEN_TOKEN_FILE` contents). Regular file, owned by the caller, mode `0600`. |
| `ANAMNESIS_RUNTIME_TOKEN_FILE` | Installation token (the daemon's `<runtime root>/token`) for `hello`. Same file rules. |

`verify` over TCP skips the three filesystem checks (`root_private`, `token_private`, `socket_private`) because they describe the daemon host; its `scope` field says so.

A missing or non-`0600` file is an error naming the variable; token values are never printed. `ANAMNESIS_RUNTIME_ROOT` still has to point at a writable directory for the client's own checkpoints, but nothing under it is read for authentication in this mode.

## Production layout: PVE guest + inonono sources

Reference layout used for the first production install (issue #213). Unit files and scripts live under `deploy/`; copy them, do not symlink into the checkout. Production is the Debian 13 LXC at `10.10.10.20`; it reaches the tailnet through the PVE subnet router, not a guest tailscaled instance.

```
 inonono (tailnet)                              PVE guest "anamnesis" (routed)
 ┌────────────────────────────┐  ssh -N -L      ┌──────────────────────────────┐
 │ token-hub    127.0.0.1:19080├──────────────┐ │ anamnesis-tunnel.service      │
 │ llama-server 127.0.0.1:18081├────────────┐ │ │  127.0.0.1:19080 / :18081     │
 │                            │            └─┴─┤ anamnesis.service (foreground)│
 │ anamnesis-ingest@*.timer   │  RPC/TCP bearer│  LISTEN=10.10.10.20:4400      │
 │  ops ingest-* over tailnet ├───────────────►│ Neo4j 5.26 (docker compose)   │
 │ /mnt/data/anamnesis/backups│◄── rsync ──────┤ anamnesis-backup.timer        │
 └────────────────────────────┘                └──────────────────────────────┘
```

### PVE LXC creation and routing

Create the guest through `POST /api2/json/nodes/<node>/lxc`, supplying the allocated VMID, Debian template, storage, bridge/IP configuration and SSH public key. For the API-token provisioning path used here, **privilege separation must be off (`privsep=0`)** on the provisioning token; its owning user still needs the relevant PVE permissions. Keep the token in a private credential file, never inline in recorded commands, shell tracing, or the provisioning log. Wait for the returned PVE task to succeed before starting the guest.

Set `features=nesting=1`. Do not request `keyctl`: PVE will not let an API token set that feature, even with privilege separation off. Nesting alone sufficed for this guest's Docker/Neo4j deployment.

The guest's default gateway is not the tailnet subnet router. Add the return route and persist it in the guest's `eth0` stanza in `/etc/network/interfaces`:

```text
    post-up ip route replace 100.64.0.0/10 via <tsgw internal IP>
```

On this install `<tsgw internal IP>` is `10.10.10.2`; apply `ip route replace 100.64.0.0/10 via 10.10.10.2` immediately as well. On inonono run `sudo tailscale set --accept-routes` so it can reach the advertised guest subnet. Verify both directions, not just an SSH handshake. Administrative access is `ssh -J admin@100.64.240.3 root@10.10.10.20`.

### Guest

1. Debian guest with docker and node 22 (for the built bundles); the production LXC uses Debian 13 and the routing above. Create user `anamnesis` (member of `docker`), `/opt/anamnesis` (checkout + `bun run build:runtime` output in `dist/`), `/var/lib/anamnesis/{runtime,objects,backups}` owned by that user, `/etc/anamnesis` mode `0700`.
2. Neo4j: follow [First deploy](#first-deploy) inside `/opt/anamnesis` (`gen-password`, `db:up`, `migrate`). Keep the compose bind on `127.0.0.1`. The compose file labels the container `anamnesis.qa.owner=${ANAMNESIS_QA_OWNER}` (default `production`, from `.env`); the daemon env must carry the same `ANAMNESIS_QA_OWNER` and `ANAMNESIS_NEO4J_CONTAINER=anamnesis-neo4j-1`, otherwise backup/restore fail with `backup_adapter_unavailable` or `owned_container_required`.
   inonono only needs node 22 and the `dist/` bundles (no bun); copy `dist/` from a `bun run build:runtime` output.
3. Secrets, all `0600`, owned by `anamnesis`, never committed or logged:
   - `/etc/anamnesis/anamnesis.env` from `deploy/vm/anamnesis.env.example` (Neo4j password from `.env`, `ANAMNESIS_LISTEN=10.10.10.20:4400`).
   - `/etc/anamnesis/listen-token` (`openssl rand -base64 32`).
   - `/etc/anamnesis/token-hub-haiku.json` — copy of the token-hub client key (`{"bearer":"..."}`) from inonono `~/.config/anamnesis/token-hub-haiku.json`.
   - `/etc/anamnesis/tunnel_ed25519` — key pair for the tunnel; add the public key to inonono `~/.ssh/authorized_keys` restricted with `restrict,port-forwarding,permitopen="127.0.0.1:19080",permitopen="127.0.0.1:18081"`. The unit runs locally as `anamnesis`, connects as `inonono@100.113.163.45`, and forwards only loopback listeners using `ssh -N -T`, `BatchMode=yes` and `StrictHostKeyChecking=yes`. Provision the verified host key for the `anamnesis` account before starting it. `restrict` disables PTY/agent/X11 forwarding, but does not itself prohibit remote commands: the shipped backup script also uses this key for rsync. Do not add a forced `/bin/false` command to a shared backup key; a dedicated backup key can instead be restricted to the exact rsync receiver command and destination.
4. Units: `install -m 644 deploy/vm/*.service deploy/vm/*.timer /etc/systemd/system/`, then `systemctl enable --now anamnesis-tunnel anamnesis anamnesis-backup.timer`. `anamnesis.service` waits for the Neo4j container to report healthy and runs `ops foreground`, so systemd is the supervisor; do not also run `ops up` (the `managed` supervisor) on the same root.
5. Pacing: the example env sets `ANAMNESIS_EXTRACTION_MAX_IN_FLIGHT=2`, `ANAMNESIS_LLM_MIN_INTERVAL_MS=4000`, `ANAMNESIS_LLM_JITTER_FRACTION=0.5` (about 0.25 haiku requests/s with jitter) so anamnesis stays a small share of the shared token-hub load. `ops status` reports the active values under `workers.extraction.pacing`.
6. Health: `deploy/vm/healthcheck.sh` prints `status.workers` and exits 1 when a lane reports `last_error` or extraction is `unconfigured`. Wire it into whatever probes the host.

### Sources on inonono

1. Client files: `~/.config/anamnesis/ingest.env` from `deploy/inonono/ingest.env.example`, with `ANAMNESIS_RPC_TCP=10.10.10.20:4400`, plus `~/.config/anamnesis/listen-token` and `~/.config/anamnesis/pve-runtime-token` copied from the guest (`0600`).
2. `install -m 755 deploy/inonono/anamnesis-ingest-source ~/.local/bin/`; `install -m 644 deploy/inonono/anamnesis-ingest@.* ~/.config/systemd/user/`; `systemctl --user daemon-reload`; `loginctl enable-linger $USER`.
3. Enable live-source timers: `systemctl --user enable --now anamnesis-ingest@{codex,claude}.timer`, then trigger `systemctl --user start anamnesis-ingest@{codex,claude}.service` and check `Result=success`, `ExecMainStatus=0`, and `source_checkpoint` / `source_complete` output. The adapters read sealed snapshots, not live tails. `loginctl show-user "$USER" -p Linger` must report `yes`.
4. Use a new `ANAMNESIS_INGEST_STATE` when moving to a new daemon incarnation; do not delete old pending/checkpoint files. This deployment preserves the old state and uses `~/.local/state/anamnesis-ingest/pve-51ca02ca/<source>/`. Re-running against the same installation is idempotent by revision key.

The Claude adapter needs a recognized `projects/` component **inside** its export root. Passing `~/.claude/projects` directly produced `source_no_export_files`; passing all of `~/.claude` encountered an unrelated `CLAUDE.md` symlink and was correctly rejected. The deployed `~/.config/systemd/user/anamnesis-ingest@claude.service.d/source-root.conf` stages only projects, retaining their directory structure:

```ini
[Service]
ExecStart=
ExecStartPre=/usr/bin/mkdir -p ${ANAMNESIS_INGEST_STATE}/claude-export/projects
ExecStartPre=/usr/bin/rsync -a --delete %h/.claude/projects/ ${ANAMNESIS_INGEST_STATE}/claude-export/projects/
ExecStart=/usr/bin/node %h/.local/lib/anamnesis/dist/anamnesis-ops.mjs ingest-claude-raw ${ANAMNESIS_INGEST_STATE}/claude-export ${ANAMNESIS_INGEST_STATE}/claude/checkpoint.json
```

Run `systemctl --user daemon-reload` after installing the drop-in. Keep the original source quiescent while taking the snapshot; the adapter still rejects symlinks inside the staged tree. The initial live-source runs completed at checkpoint `next=27` (Codex) and `next=6` (Claude).

### Vault backlog

Only `vault-codex` and `vault-claude-code` are enabled for this rollout. Each `vault-*` service converts once with `dist/vault-to-snapshots.mjs`, orders sessions newest-first (records ascending within each session), and ingests one shard per run. The timer repeats every 30 minutes with up to 5 minutes of randomized delay; extraction remains throttled by the daemon's verified `2 / 4000 ms / 0.5` pacing, not a client sleep loop.

```sh
systemctl --user enable --now anamnesis-ingest@{vault-codex,vault-claude-code}.timer
systemctl --user start --no-block anamnesis-ingest@{vault-codex,vault-claude-code}.service
```

The production services have per-instance logging drop-ins, for example:

```ini
[Service]
StandardOutput=append:%h/.local/state/anamnesis-ingest/pve-51ca02ca/logs/vault-codex.log
StandardError=inherit
```

Create the log directory first and use `vault-claude-code.log` for the other instance. `{"event":"backlog_drained"}` marks completion; starting the units is not evidence of full backlog coverage. Inspect `systemctl --user show <unit> -p Result -p ExecMainStatus` and the logs. On inonono the useful journal is also available through `sudo journalctl _SYSTEMD_USER_UNIT=<unit>` when `journalctl --user` is empty.

Slack, Notion, Discord, OpenCode, GJC, Pi, and workstation-side omo/agentlog original-export coverage are deferred in [#224](https://github.com/INONONO66/anamnesis/issues/224). Slack/Discord/OpenCode/GJC/Pi vault envelopes with `raw` fields do exist; that is not proof that upstream originals are complete. Do not discard those records or blindly enable every source. The old `vault-opencode` and `vault-slack` timers are disabled pending that review.

Lost replies: a run that ends in `outcome_unknown` (the daemon or its TCP path went away under a `remember`, e.g. during a backup restart) leaves `<checkpoint>.pending.json` behind and the next timer run asks the daemon `ingest.status` for that identity. `committed` advances the checkpoint; `spooled`/`blocked` exit 1 as `source_pending_<state>` until the spool drains (`quarantined` needs the usual spool recovery first); `unknown` from the same `data_incarnation` whose own reply reports `storage: available` (the daemon observes storage in the same call, so an earlier `ops status` cannot go stale) means the daemon never admitted the delivery, so the run logs `{"event":"pending_retired","reason":"daemon_unknown"}`, retires the pending file and resends that line from the checkpoint. No operator step is needed for any of these. Only two outcomes still need a hand: `source_pending_unknown` repeating while `storage` is `unavailable` (restore the graph/DB connection; the resend happens on the next run), and `incarnation_mismatch` (the checkpoint belongs to another daemon installation: point the unit at the right daemon or start that source from a new checkpoint directory). Moving a pending file aside by hand skips that daemon check and is a blind resend; leave the file to the next run.

### First-production verification and troubleshooting

The secret-free first-ingest evidence at revision `2525ea4` recorded **32 episodes, 80 active Facts, and 32 vectors**, with workers idle and vectors equal to episodes. These are the acceptance-sample counts before the continuing backlog, not a live total. The local evidence file is not committed; summarize its numbers in the deployment PR.

| Symptom | Meaning / action |
|---|---|
| Tunnel appears to connect but forwards do not work | Check the missing tailnet return route (`100.64.0.0/10 via 10.10.10.2`) and inonono route acceptance; an SSH/listener state alone is insufficient. |
| Unauthenticated hub `/v1/models` returns HTTP 401 | The forwarded hub is reachable; authentication is required. This is not proof that the configured client credential works. |
| Claude `source_no_export_files` or unrelated `source_symlink` | Use the staged `projects/` export root above, not the projects directory itself or the entire Claude configuration tree. |
| `incarnation_mismatch` after moving the daemon | Preserve old checkpoints and select a fresh state directory for the new installation. |
| Healthcheck immediately after backup says socket `ENOENT` | The daemon may still be starting. Confirm its `listening` event, then run the healthcheck; persistent absence is a failure. |
| Backup `authority_snapshot_unavailable: invalidation hash evidence is incomplete` | The 2026-09-25 production backup hit this blocker before creating a dump. Preserve the graph and error evidence; repair the authority/invalidation evidence path before declaring backup acceptance. Do not bypass validation or present an older dump as this run's backup. |

### Running on inonono instead of a PVE guest

When the PVE guest is unavailable the same artifacts run directly on inonono, where token-hub, llama-server and the backup disk already live. Differences from the guest layout, all expressed through configuration:

- Compose project `anamnesis-prod` (`COMPOSE_PROJECT_NAME` in `/opt/anamnesis/.env`, Bolt on `127.0.0.1:7688`) so the legacy `anamnesis-neo4j-1` container is never touched; `ANAMNESIS_NEO4J_CONTAINER=anamnesis-prod-neo4j-1`.
- No `anamnesis-tunnel.service`: `ANAMNESIS_LLM_BASE_URL` / `ANAMNESIS_EMBEDDING_BASE_URL` point at the loopback ports directly; drop the tunnel lines from `anamnesis.service`.
- Objects on the data disk: `ANAMNESIS_OBJECTS_ROOT=/mnt/data/anamnesis/prod/objects` (add it to `ReadWritePaths`).
- `backup.sh` rsyncs to the local user over ssh with a `restrict,command="rsync --server ..."` key so the archive lands under `/mnt/data/anamnesis/backups/pve-vm/` unchanged.
- The ingest wrapper's `ANAMNESIS_RPC_TCP` is the host's own tailnet address.

## Backup

`anamnesis-backup.timer` runs `deploy/vm/backup.sh` daily as root: `systemctl stop anamnesis`, offline `ops backup <dir>` as the `anamnesis` user into a not-yet-existing `<stamp>/` directory with the JSON result beside it as `<stamp>.result.json` (`ops backup` refuses a live daemon with `daemon_live` and an existing destination with `destination_exists`; the daemon is restarted even if the dump fails), then `rsync` of `/var/lib/anamnesis/backups/` to inonono `/mnt/data/anamnesis/backups/pve-vm/`, keeping 14 local copies. `systemctl list-timers anamnesis-backup.timer` and `journalctl -u anamnesis-backup` show the last run; a non-zero exit marks the unit failed. Restore follows [Restore a dump](#restore-a-dump) with the daemon stopped (`systemctl stop anamnesis`).

## Redeploy

There is no application container yet. When a stateless ingestion service is added, rebuild and recreate only that service:

```sh
docker compose up -d --build --no-deps ingestion
```

Do not run `docker compose down -v`. Named volumes survive normal `up`, `restart`, and `down` operations, so app-only redeploys leave graph data untouched.

## Back up before an upgrade

Neo4j Community Edition does not provide online backup. The backup command stops the service container, uses the pinned Neo4j image with the same volumes to perform an offline dump, and restarts the service even if dumping fails:

```sh
bun run backup -- --container anamnesis-neo4j-1
# Or set NEO4J_CONTAINER and run: bun run backup
```

The dump is written to `backups/neo4j-<ISO timestamp>.dump`. Confirm that the file is non-empty and copy it to durable backup storage before changing the image version.

## Restore a dump

Restoring replaces the graph database. Stop the container, stream the selected dump into an offline utility container that shares its volumes, then restart and migrate:

```sh
container=anamnesis-neo4j-1
dump="$PWD/backups/neo4j-2026-09-02T12:00:00.000Z.dump"
docker stop "$container"
if docker run --rm -i --volumes-from "$container" neo4j:5.26-community \
  neo4j-admin database load neo4j --from-stdin --overwrite-destination=true < "$dump"
then
  docker start "$container"
  set -a; . ./.env; set +a
  bun run migrate
else
  echo "Restore failed; $container remains stopped" >&2
fi
```

If loading fails, leave the graph stopped, preserve the failed volume state for diagnosis, and retry from a verified dump.

## Rebuild after graph loss (no usable backup)

1. Start Neo4j with empty named volumes and run `bun run migrate`.
2. Point an `EpisodeJournal` at the durable journal directory.
3. Create and initialize an `Engine`, then call `await journal.replay(engine)`.
4. Run normal digest processing to regenerate downstream facts, entities, links, and communities.

Replay uses the normal `Engine.remember` path. Existing origin, element ID, and payload hash constraints make repeated replay safe and idempotent. Prefer `restore` from a verified dump when one exists — replay is the fallback for the window before the first backup, and it cannot recover Hit-ledger state, only originals.
