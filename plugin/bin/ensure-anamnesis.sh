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

# Drop stale partial downloads from killed runs so they don't make us wait below.
find "$HERE" -maxdepth 1 \( -name '.anamnesis.download.*' -o -name '.anamnesis.sums.*' \) -mmin +5 -delete 2>/dev/null || true

# If another invocation (typically the SessionStart background prefetch) is
# mid-download, wait for it to land instead of racing a second ~24MB fetch. This
# is what lets the MCP server's launcher reuse the prefetch and come up inside the
# ~30s startup window. Bounded, so a stalled peer can never hang us.
i=0
while [ -n "$(ls "$HERE"/.anamnesis.download.* 2>/dev/null)" ] && [ "$i" -lt 25 ]; do
  if [ -x "$BIN" ]; then printf '%s\n' "$BIN"; exit 0; fi
  sleep 1
  i=$((i + 1))
done
if [ -x "$BIN" ]; then printf '%s\n' "$BIN"; exit 0; fi

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
base="https://github.com/INONONO66/anamnesis/releases/download/v${ver}"
url="$base/${asset}"
sums_url="$base/SHA256SUMS.txt"
tmp="$HERE/.anamnesis.download.$$"
sums="$HERE/.anamnesis.sums.$$"

# Fetch $1 (URL) → $2 (dest) with curl or wget. Non-zero if neither downloader
# exists or the transfer fails. One place for the curl/wget choice, so the binary
# and its checksum file are fetched identically.
fetch() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$1" -O "$2"
  else
    return 1
  fi
}

fetch "$url" "$tmp" || { rm -f "$tmp"; exit 1; }

# Reject an empty/HTML (404) body before we spend effort verifying it.
if [ ! -s "$tmp" ]; then
  rm -f "$tmp"
  exit 1
fi

# --- Fail-CLOSED checksum verification (supply-chain integrity, S1b) ----------
# Auto-executing an unverified GitHub download is an RCE-class gap for a
# local-first tool, so the binary is verified BEFORE chmod/mv: fetch the release
# SHA256SUMS.txt, look up the expected hash for THIS platform's asset, hash the
# downloaded file (sha256sum on Linux / shasum -a 256 on macOS), and refuse to
# install on ANY mismatch, missing entry, or absent hasher. This is the only
# fail-CLOSED point; the calling hook (anamnesis-hook.sh) still exits 0
# regardless, so recall/capture as a whole stays fail-open.
if ! fetch "$sums_url" "$sums" || [ ! -s "$sums" ]; then
  rm -f "$tmp" "$sums"
  exit 1
fi

# Expected hash = first field of the SHA256SUMS.txt line naming our asset. `read`
# splits the "<hex>  <file>" line on whitespace (the double space collapses to a
# single delimiter); strip an optional leading '*' binary-mode marker.
expected=""
while read -r sum name; do
  name=${name#\*}
  if [ "$name" = "$asset" ]; then
    expected=$sum
    break
  fi
done < "$sums"
rm -f "$sums"
if [ -z "$expected" ]; then
  rm -f "$tmp"
  exit 1
fi

# Actual hash: sha256sum (Linux) or shasum -a 256 (macOS), whichever exists.
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$tmp" | cut -d' ' -f1)
elif command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$tmp" | cut -d' ' -f1)
else
  rm -f "$tmp"
  exit 1
fi

# Mismatch (tampered/corrupt asset, or a replaced SHA256SUMS.txt) → discard,
# never install, exit non-zero. The hook then falls back to a PATH binary/no-op.
if [ -z "$actual" ] || [ "$actual" != "$expected" ]; then
  rm -f "$tmp"
  exit 1
fi
# --- end verification ---------------------------------------------------------

chmod +x "$tmp"
mv -f "$tmp" "$BIN" || { rm -f "$tmp"; exit 1; }
printf '%s\n' "$BIN"
