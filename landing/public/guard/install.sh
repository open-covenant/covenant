#!/bin/sh
# covguard installer, one command to a guarded first run.
#
#   curl -fsSL https://opencovenant.org/guard/install.sh | sh
#
# Downloads the latest signed covguard release for this platform, verifies its
# checksum, and installs it onto your PATH. No agent is run; nothing is sent.
set -eu

REPO="${COVGUARD_REPO:-open-covenant/covenant}"

say() { printf '  %s\n' "$*"; }
die() { printf 'covguard install: %s\n' "$*" >&2; exit 1; }

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Darwin) os=darwin ;;
  Linux)  os=linux ;;
  *) die "unsupported OS '$os', build from source: cargo install --path agent-os/crates/covenant-guard" ;;
esac
case "$arch" in
  arm64|aarch64) arch=arm64 ;;
  x86_64|amd64)  arch=x86_64 ;;
  *) die "unsupported arch '$arch'" ;;
esac

# Shipped targets: darwin-arm64 and linux-x86_64. Other platforms build from
# source. The Linux sandbox needs bubblewrap installed (apt install bubblewrap).
asset="covguard-${os}-${arch}.tar.gz"
case "${os}-${arch}" in
  darwin-arm64|linux-x86_64) : ;;
  *) die "no prebuilt binary for ${os}-${arch} yet; build from source: cargo install --path agent-os/crates/covenant-guard" ;;
esac

say "finding the latest covguard release…"
api="https://api.github.com/repos/${REPO}/releases?per_page=100"
# Releases are returned newest-first; take the first asset matching this platform.
url="$(curl -fsSL "$api" | grep -o "https://[^\"]*${asset}" | head -n1 || true)"
[ -n "$url" ] || die "could not find a released ${asset} (set COVGUARD_REPO or build from source)"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
say "downloading $asset"
curl -fsSL "$url" -o "$tmp/$asset"

# The release publishes a cosign-signed checksums.txt (lines: "<sha256>  <asset>").
# Verification is not optional, fail closed if it's missing or doesn't match.
say "verifying checksum"
sums_url="${url%/*}/checksums.txt"
curl -fsSL "$sums_url" -o "$tmp/checksums.txt" || die "no checksums.txt in the release, refusing to install"
expected="$(awk -v a="$asset" '$2 == a {print $1}' "$tmp/checksums.txt")"
[ -n "$expected" ] || die "no checksum for $asset in the release, refusing to install"
if command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
else
  actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
fi
[ "$expected" = "$actual" ] || die "checksum mismatch for $asset, refusing to install"

tar -xzf "$tmp/$asset" -C "$tmp"
bin="$(find "$tmp" -type f -name covguard | head -n1)"
[ -n "$bin" ] || die "covguard binary not found in the archive"
chmod +x "$bin"

# Prefer a system bin dir if writable, else fall back to ~/.local/bin.
dest="/usr/local/bin"
if [ ! -w "$dest" ]; then dest="$HOME/.local/bin"; fi
mkdir -p "$dest"
mv "$bin" "$dest/covguard"

say "installed covguard to $dest/covguard"
case ":$PATH:" in
  *":$dest:"*) : ;;
  *) say "add $dest to your PATH: export PATH=\"$dest:\$PATH\"" ;;
esac
if [ "$os" = linux ] && ! command -v bwrap >/dev/null 2>&1; then
  say "note:  the Linux sandbox needs bubblewrap: apt install bubblewrap (or dnf install bubblewrap)"
fi
printf '\n'
say "next:  covguard doctor"
say "then:  covguard run --budget 10 -- claude -p \"...\" --dangerously-skip-permissions"
