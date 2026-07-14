#!/usr/bin/env bash
set -euo pipefail

BIN="${1:?usage: hook-battery.sh <anamnesis-binary>}"
workdir="$(mktemp -d)"
cleanup() {
  local status=$?
  sleep 2 || true
  rm -rf "$workdir"
  exit "$status"
}
trap cleanup EXIT

export ANAMNESIS_DB="$workdir/battery.db"
export ANAMNESIS_SOCKET="$workdir/battery.sock"
export ANAMNESIS_EMBED_MODEL="${ANAMNESIS_EMBED_MODEL:-multilingual-e5-small}"
export ANAMNESIS_HOOK_TIMEOUT_MS="${ANAMNESIS_HOOK_TIMEOUT_MS:-60000}"
export ANAMNESIS_DAEMON_GRACE_SECS=1

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

ensure_daemon() {
  "$BIN" stats >/dev/null
}

remember_scoped() {
  local scope="$1"
  local content="$2"
  ensure_daemon
  python3 - "$scope" "$content" <<'PY'
import json
import os
import socket
import sys

scope, content = sys.argv[1], sys.argv[2]
req = {"op": "remember", "content": content, "scope": scope}
with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.connect(os.environ["ANAMNESIS_SOCKET"])
    sock.sendall((json.dumps(req, ensure_ascii=False) + "\n").encode())
    data = b""
    while not data.endswith(b"\n"):
        chunk = sock.recv(65536)
        if not chunk:
            break
        data += chunk
resp = json.loads(data.decode())
if resp.get("status") != "ok":
    raise SystemExit(resp)
PY
}

hook_user_prompt() {
  local prompt="$1"
  local cwd="$2"
  python3 - "$prompt" "$cwd" <<'PY' | "$BIN" hook user-prompt
import json
import sys

prompt, cwd = sys.argv[1], sys.argv[2]
print(json.dumps({
    "hook_event_name": "UserPromptSubmit",
    "prompt": prompt,
    "cwd": cwd,
}, ensure_ascii=False))
PY
}

hook_session_start() {
  local cwd="$1"
  python3 - "$cwd" <<'PY' | "$BIN" hook session-start
import json
import sys

cwd = sys.argv[1]
print(json.dumps({
    "hook_event_name": "SessionStart",
    "cwd": cwd,
}, ensure_ascii=False))
PY
}

remember_scoped "project/proj-a" \
  "Project proj-a deploy policy: canary rollout requires a health gate before full release."
remember_scoped "project/proj-a" \
  "Project proj-a migration rule: take a database backup before running schema changes."
remember_scoped "project/proj-b" \
  "Project proj-b mobile note: SwiftUI widget schedule uses a TimelineProvider refresh budget."

out="$(hook_user_prompt "다 별론데?" "/tmp/proj-a")"
[[ -z "$out" ]] || fail "content-free prompt injected: $out"

out="$(hook_user_prompt "What is the canary health gate rollout policy?" "/tmp/proj-a")"
[[ "$out" == *"canary rollout"* ]] || fail "topical prompt did not inject canary context: $out"

out="$(hook_user_prompt "What is the SwiftUI widget schedule budget?" "/tmp/proj-a")"
[[ "$out" != *"SwiftUI widget"* ]] || fail "cross-project prompt leaked proj-b context: $out"

out="$(hook_session_start "/tmp/no-such-project")"
[[ -z "$out" ]] || fail "unknown project SessionStart injected: $out"

echo OK
