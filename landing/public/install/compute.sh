#!/bin/sh
# Installs the Covenant Compute command-line tools on macOS or Linux.
#
#   curl -fsSL https://opencovenant.org/install/compute.sh | sh
#
# Downloads the release archive for this machine from GitHub, checks it against
# the checksum pinned below, and copies the four commands to
# $COVENANT_COMPUTE_INSTALL_DIR (default: ~/.local/bin). Nothing else on the
# system is changed. Guide: https://docs.opencovenant.org/compute
set -eu

# >>> pinned release
version=0.3.0
sha256_darwin_arm64=2c3d456d03aae6ce8074e8b26b296b53695a86261d5c0c2cee7d4a09b81ec768
sha256_darwin_x86_64=1b60802d7b2f02eb9c4dc0d1286f4c17b839270cf67148702b70418a1e24244c
sha256_linux_x86_64=0419d348e8a28790750f610937d63aa0e39ca5cceedb22f583f9dd47ba661592
sha256_linux_arm64=082f271879243f047bacc2f83c8c52431973a75bf9dcc78dfd52acb6f2cda206
# <<< pinned release

commands="covenant-compute covenant-compute-mcp covenant-compute-node covenant-compute-stake"

fail() {
  printf 'covenant-compute install: %s\n' "$*" >&2
  exit 1
}

case "$(uname -s)" in
  Darwin) os=darwin ;;
  Linux) os=linux ;;
  *) fail "no build for $(uname -s); see https://docs.opencovenant.org/compute to build from source" ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) arch=arm64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) fail "no build for $(uname -m) processors; see https://docs.opencovenant.org/compute to build from source" ;;
esac
# A shell running under Rosetta reports x86_64 on Apple silicon; install the native build.
if [ "$os" = darwin ] && [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
  arch=arm64
fi

asset="covenant-compute-$os-$arch"
case "$asset" in
  covenant-compute-darwin-arm64) expected=$sha256_darwin_arm64 ;;
  covenant-compute-darwin-x86_64) expected=$sha256_darwin_x86_64 ;;
  covenant-compute-linux-x86_64) expected=$sha256_linux_x86_64 ;;
  covenant-compute-linux-arm64) expected=$sha256_linux_arm64 ;;
esac
[ -n "$expected" ] || fail "no release is pinned for $asset in this copy of the script"
url="https://github.com/open-covenant/covenant/releases/download/compute-v$version/$asset.tar.gz"

stage=
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"; [ -z "$stage" ] || rm -rf "$stage"' EXIT
trap 'exit 1' INT TERM

printf 'Downloading Covenant Compute %s for %s %s\n' "$version" "$os" "$arch"
if command -v curl >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -fsSL "$url" -o "$tmp/$asset.tar.gz" || fail "download failed: $url"
elif command -v wget >/dev/null 2>&1; then
  wget -q "$url" -O "$tmp/$asset.tar.gz" || fail "download failed: $url"
else
  fail "needs curl or wget to download $url"
fi

if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$asset.tar.gz" | cut -d ' ' -f 1)"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$tmp/$asset.tar.gz" | cut -d ' ' -f 1)"
else
  fail "needs sha256sum or shasum to check the download"
fi
[ "$actual" = "$expected" ] || fail "$asset.tar.gz does not match its pinned checksum (expected $expected, got $actual); nothing was installed"
printf 'Checksum verified\n'

tar -xzf "$tmp/$asset.tar.gz" -C "$tmp"
dir="${COVENANT_COMPUTE_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$dir" || fail "cannot create $dir; set COVENANT_COMPUTE_INSTALL_DIR to a directory you can write to"
# Stage next to the destination, then rename: a rename needs no free space, so a
# full disk or an interrupted copy leaves the previous commands untouched.
stage="$(mktemp -d "$dir/.covenant-compute.XXXXXX")" || fail "cannot write to $dir"
for cmd in $commands; do
  install -m 0755 "$tmp/$asset/$cmd" "$stage/$cmd" || fail "cannot write to $dir"
done
for cmd in $commands; do
  mv -f "$stage/$cmd" "$dir/$cmd" || fail "cannot replace $dir/$cmd"
done

printf '\nInstalled Covenant Compute %s in %s:\n' "$version" "$dir"
for cmd in $commands; do
  printf '  %s\n' "$cmd"
done

# What the shell still needs: the install directory on PATH, and the network to talk to.
need_path=yes
case ":$PATH:" in
  *":$dir:"*) need_path= ;;
esac
need_url=yes
[ -z "${COVENANT_COMPUTE_COORDINATOR_URL:-}" ] || need_url=
if [ -n "$need_path" ] && [ -n "$need_url" ]; then
  printf '\nAdd these lines to your shell profile, and run them in this terminal too:\n'
elif [ -n "$need_path$need_url" ]; then
  printf '\nAdd this line to your shell profile, and run it in this terminal too:\n'
fi
[ -z "$need_path" ] || printf "  export PATH=\"%s:\$PATH\"\n" "$dir"
[ -z "$need_url" ] || printf '  export COVENANT_COMPUTE_COORDINATOR_URL=https://compute-api.opencovenant.org\n'
if [ -n "$need_path$need_url" ]; then
  printf '\nThen print your buyer key, which identifies your balance:\n'
else
  printf '\nPrint your buyer key, which identifies your balance:\n'
fi
printf '  covenant-compute whoami\n'
printf '\nGuide: https://docs.opencovenant.org/compute\n'
