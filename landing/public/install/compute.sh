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
version=0.4.1
sha256_darwin_arm64=bcea9e9bd493cef0f7568197a6b06ed8d14d36bf2502d98b838338c043a6ee39
sha256_darwin_x86_64=54340753b65f22c04b4b50e16d1a21e30d230d8a6138c847bd085285c7594928
sha256_linux_x86_64=a8a91cb60f2251ac070b0bacc104010b121698b65ca9efab6424ef62d79718bb
sha256_linux_arm64=5aa9d0673a56094322043a495e39563e8b009b4c643e9f878feaf4ed9a9e0b94
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
