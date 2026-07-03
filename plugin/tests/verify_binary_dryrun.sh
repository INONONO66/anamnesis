#!/usr/bin/env sh
# Dry-run verification harness for the fail-CLOSED checksum gate in
# bin/ensure-anamnesis.sh (supply-chain fix S1b), plus the S2 pin check.
#
# It runs the REAL, UNMODIFIED bin/ensure-anamnesis.sh inside a throwaway
# sandbox, with a mock `curl` on PATH that serves a local fake binary + a local
# SHA256SUMS.txt fixture instead of hitting github.com — NO network, NO real
# download. Because the installer installs next to itself ($HERE/anamnesis), the
# harness runs a COPY inside the sandbox, so every install lands in the sandbox
# and the real plugin/bin/anamnesis is never a write target.
#
# Assertions:
#   (a) TAMPERED: served bytes != the hash published in SHA256SUMS.txt
#         → installer exits non-zero AND creates no binary.
#   (b) MATCH:    served bytes == the published hash
#         → installer exits 0 AND installs the verified binary.
#   (c) the real plugin/bin/anamnesis is byte-for-byte unchanged by the test.
#   (d) .codex-mcp.json is pinned: no "@latest", contains the VERSION value.
#
# Exit 0 iff every assertion holds. POSIX sh; requires sha256sum or shasum.

set -u

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PLUGIN_DIR=$(CDPATH= cd -- "$HERE/.." && pwd)
SCRIPT="$PLUGIN_DIR/bin/ensure-anamnesis.sh"
VERSION_FILE="$PLUGIN_DIR/bin/VERSION"
REAL_BIN="$PLUGIN_DIR/bin/anamnesis"
CODEX_MCP="$PLUGIN_DIR/.codex-mcp.json"

VERSION=$(tr -d ' \t\r\n' < "$VERSION_FILE")

fails=0
pass() { printf '  PASS  %s\n' "$1"; }
fno()  { printf '  FAIL  %s\n' "$1"; fails=$((fails + 1)); }
# check "<description>" <test-command...>   (e.g. check "..." test "$RC" -ne 0)
check() { d=$1; shift; if "$@"; then pass "$d"; else fno "$d"; fi; }

# Hash a file the same way the installer does: sha256sum (Linux) or shasum (macOS).
hash_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    printf 'NO_HASHER\n'
  fi
}

if [ "$(hash_of "$VERSION_FILE")" = NO_HASHER ]; then
  printf 'FATAL: neither sha256sum nor shasum found; cannot run.\n' >&2
  exit 2
fi

# Snapshot the real binary's state so we can prove the harness never wrote it.
real_before="absent"
[ -e "$REAL_BIN" ] && real_before=$(hash_of "$REAL_BIN")

RC=0
INSTALLED=no
BINROOT=
GOOD_HASH=

# run_scenario <good|tamper> — build a sandbox, run the installer copy against a
# mocked download, and set RC / INSTALLED / BINROOT / GOOD_HASH.
run_scenario() {
  mode=$1
  sandbox=$(mktemp -d "${TMPDIR:-/tmp}/anamnesis-verify.XXXXXX") || {
    printf 'FATAL: mktemp failed\n' >&2; exit 2; }
  serve="$sandbox/serve"
  mockbin="$sandbox/mockbin"
  BINROOT="$sandbox/bin"
  mkdir -p "$serve" "$mockbin" "$BINROOT"

  # Installer copy + its VERSION so $HERE (and the install target) is the sandbox.
  cp "$SCRIPT" "$BINROOT/ensure-anamnesis.sh"
  cp "$VERSION_FILE" "$BINROOT/VERSION"

  # The legitimate binary; its hash is what SHA256SUMS.txt publishes.
  printf 'GOOD-ANAMNESIS-BINARY-%s\n' "$VERSION" > "$serve/good"
  GOOD_HASH=$(hash_of "$serve/good")

  # Publish that hash for every platform asset name (the installer resolves and
  # looks up its own; the extra lines are harmless) in standard sha256sum format.
  : > "$serve/SHA256SUMS.txt"
  for a in anamnesis-darwin-arm64 anamnesis-darwin-x64 anamnesis-linux-x64 anamnesis-linux-arm64; do
    printf '%s  %s\n' "$GOOD_HASH" "$a" >> "$serve/SHA256SUMS.txt"
  done

  # What the mock curl actually serves as the binary body.
  if [ "$mode" = good ]; then
    cp "$serve/good" "$serve/binary"                                  # matches published hash
  else
    printf 'TAMPERED-EVIL-PAYLOAD-%s\n' "$VERSION" > "$serve/binary"  # != published hash
  fi

  # Mock curl (no network): serve $serve/binary for any anamnesis-* asset and
  # $serve/SHA256SUMS.txt for the sums file; anything else 404s (exit 22). The
  # installer prefers curl, so providing only a mock curl is sufficient.
  cat > "$mockbin/curl" <<'MOCK'
#!/usr/bin/env sh
url=""; out=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) shift; out="$1" ;;
    http://*|https://*) url="$1" ;;
  esac
  shift
done
base=${url##*/}
case "$base" in
  SHA256SUMS.txt) src="$MOCK_SERVE_DIR/SHA256SUMS.txt" ;;
  anamnesis-*)    src="$MOCK_SERVE_DIR/binary" ;;
  *)              exit 22 ;;
esac
[ -f "$src" ] || exit 22
cp "$src" "$out"
MOCK
  chmod +x "$mockbin/curl"

  # Run the installer copy: mock curl first on PATH, MOCK_SERVE_DIR feeds it.
  MOCK_SERVE_DIR="$serve" PATH="$mockbin:$PATH" sh "$BINROOT/ensure-anamnesis.sh" >/dev/null 2>&1
  RC=$?
  INSTALLED=no
  [ -e "$BINROOT/anamnesis" ] && INSTALLED=yes
}

printf '== scenario (a): TAMPERED binary (sha256 mismatch) ==\n'
run_scenario tamper
check "installer exits non-zero on mismatch (rc=$RC)"      test "$RC" -ne 0
check "no binary installed on mismatch (installed=$INSTALLED)" test "$INSTALLED" = no

printf '== scenario (b): MATCHING checksum ==\n'
run_scenario good
check "installer exits 0 on match (rc=$RC)"                test "$RC" -eq 0
check "verified binary installed on match (installed=$INSTALLED)" test "$INSTALLED" = yes
if [ "$INSTALLED" = yes ]; then
  check "installed bytes equal the verified binary" test "$(hash_of "$BINROOT/anamnesis")" = "$GOOD_HASH"
fi

printf '== guard: real plugin/bin/anamnesis untouched ==\n'
real_after="absent"
[ -e "$REAL_BIN" ] && real_after=$(hash_of "$REAL_BIN")
check "real binary unchanged (state: $real_before)" test "$real_after" = "$real_before"

printf '== scenario (d): .codex-mcp.json pin (S2) ==\n'
if grep -q '@latest' "$CODEX_MCP"; then
  fno ".codex-mcp.json still contains @latest"
else
  pass ".codex-mcp.json no longer contains @latest"
fi
if grep -q "anamnesis-mcp@${VERSION}" "$CODEX_MCP"; then
  pass ".codex-mcp.json pins anamnesis-mcp@${VERSION}"
else
  fno ".codex-mcp.json does not pin anamnesis-mcp@${VERSION}"
fi

printf '\n== summary ==\n'
if [ "$fails" -eq 0 ]; then
  printf 'ALL ASSERTIONS PASSED\n'
  exit 0
fi
printf '%d ASSERTION(S) FAILED\n' "$fails"
exit 1
