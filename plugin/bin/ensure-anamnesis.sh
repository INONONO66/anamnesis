#!/usr/bin/env sh
# Ensure the `anamnesis` binary is present next to this script, fetching the
# matching prebuilt from the GitHub Release on first use. This is what makes the
# plugin install-and-go for EVERYONE: CC/Codex plugins have no npm-style
# postinstall, so the plugin's own wrappers fetch the binary lazily on first run.
#
# - If a bundled/already-fetched binary exists → echo its path, done (no network).
# - Else download `anamnesis-<platform>` from the release tagged `v<VERSION>` into
#   a temp file and atomically move it into place (a killed/partial download never
#   becomes the live binary). Echoes the path on success.
# - On unsupported platform / no downloader / network failure → exit non-zero so
#   the caller falls back to a PATH `anamnesis` (npm/cargo) or no-ops.
#
# Idempotent and safe to call from multiple hooks: the atomic move means the
# worst case under a race is two downloads, one of which wins the rename.

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BIN="$HERE/anamnesis"

# Fast path: already present (bundled locally, or fetched on a previous run).
if [ -x "$BIN" ]; then
  printf '%s\n' "$BIN"
  exit 0
fi

# Map uname → the release asset name (matches .github/workflows/release.yml).
os=$(uname -s 2>/dev/null || echo unknown)
arch=$(uname -m 2>/dev/null || echo unknown)
case "$os/$arch" in
  Darwin/arm64) asset="anamnesis-darwin-arm64" ;;
  Darwin/x86_64) asset="anamnesis-darwin-x64" ;;
  Linux/x86_64 | Linux/amd64) asset="anamnesis-linux-x64" ;;
  Linux/aarch64 | Linux/arm64) asset="anamnesis-linux-arm64" ;;
  *) exit 1 ;; # unsupported (e.g. Windows, musl) → caller falls back to PATH
esac

ver=$(cat "$HERE/VERSION" 2>/dev/null | tr -d ' \t\r\n')
[ -n "$ver" ] || exit 1
url="https://github.com/INONONO66/anamnesis/releases/download/v${ver}/${asset}"
tmp="$HERE/.anamnesis.download.$$"

if command -v curl >/dev/null 2>&1; then
  curl -fsSL "$url" -o "$tmp" || { rm -f "$tmp"; exit 1; }
elif command -v wget >/dev/null 2>&1; then
  wget -q "$url" -O "$tmp" || { rm -f "$tmp"; exit 1; }
else
  exit 1
fi

# Reject an empty/HTML (404) body before installing it.
if [ ! -s "$tmp" ]; then
  rm -f "$tmp"
  exit 1
fi
chmod +x "$tmp"
mv -f "$tmp" "$BIN" || { rm -f "$tmp"; exit 1; }
printf '%s\n' "$BIN"
