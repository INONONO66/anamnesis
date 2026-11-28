#!/usr/bin/env bash
set -euo pipefail
umask 077

if [[ $# != 2 || $2 != /* ]]; then
  echo "usage: bash scripts/eval/runtime.sh {up|load|down} <absolute-eval-directory>" >&2
  exit 2
fi
cd "$(dirname "$0")/../.."
action=$1
data=$2
name=anamnesis-eval-20261128
owner=eval-20261128
image=neo4j:5.26-community
root=/tmp/anamnesis-eval-root
password_file=$data/neo4j.password
node=${NODE_BIN:-node}
ops=dist/anamnesis-ops.mjs

# Do not inherit any production routing, credentials or provider settings.
for variable in ${!ANAMNESIS_@}; do unset "$variable"; done
export ANAMNESIS_RUNTIME_ROOT=$root
export ANAMNESIS_EXTRACTION_LANE=off
export ANAMNESIS_NEO4J_URI=bolt://127.0.0.1:17687

if docker container inspect "$name" >/dev/null 2>&1; then
  [[ $(docker inspect -f '{{index .Config.Labels "anamnesis.qa.owner"}}' "$name") == "$owner" ]] ||
    { echo "refusing to use container without the eval ownership label" >&2; exit 1; }
fi

case "$action" in
  up)
    mkdir -p "$data"
    if [[ ! -e $password_file ]]; then
      (set -C; openssl rand -hex 32 > "$password_file")
    fi
    [[ -f $password_file && ! -L $password_file ]]
    chmod 600 "$password_file"
    export ANAMNESIS_NEO4J_PASSWORD
    ANAMNESIS_NEO4J_PASSWORD=$(<"$password_file")
    if ! docker container inspect "$name" >/dev/null 2>&1; then
      docker volume create --label "anamnesis.qa.owner=$owner" "$name-data" >/dev/null
      NEO4J_AUTH="neo4j/$ANAMNESIS_NEO4J_PASSWORD" docker create --name "$name" \
        --label "anamnesis.qa.owner=$owner" -p 127.0.0.1:17687:7687 \
        -v "$name-data:/data" -e NEO4J_AUTH \
        -e NEO4J_server_memory_heap_initial__size=1G \
        -e NEO4J_server_memory_heap_max__size=1G \
        -e NEO4J_server_memory_pagecache_size=512M "$image" >/dev/null
    fi
    docker start "$name" >/dev/null
    ready=0
    for ((i=0; i<90; i++)); do
      if NEO4J_PASSWORD="$ANAMNESIS_NEO4J_PASSWORD" docker exec -e NEO4J_PASSWORD \
        "$name" cypher-shell -u neo4j 'RETURN 1;' >/dev/null 2>&1; then ready=1; break; fi
      sleep 1
    done
    [[ $ready == 1 ]] || { echo "eval Neo4j did not become ready" >&2; exit 1; }
    exec "$node" "$ops" foreground
    ;;
  load)
    for metadata in "$data"/slice-clean/*/_rooms.jsonl; do
      source=${metadata%/_rooms.jsonl}
      # Admission is serial; parallel batches can queue behind the socket's 30s idle deadline.
      "$node" "$ops" load "$data/slice-clean" "${source##*/}" "$data/load-state" --concurrency 1
    done
    ;;
  down)
    "$node" "$ops" down
    docker stop "$name"
    ;;
  *) echo "unknown command: $action" >&2; exit 2 ;;
esac
