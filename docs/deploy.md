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
