#!/usr/bin/env bash
# Pins a published Covenant Compute release into the Homebrew formula and the
# install script. The checksums come from the release's checksums.txt, after
# cosign confirms the release workflow signed it for that tag.
#
#   deploy/compute-release/pin.sh <version> <homebrew-tap checkout> <install script>
#
# The install script is landing/public/install/compute.sh on main, served as
# https://opencovenant.org/install/compute.sh. Its block between the "pinned
# release" markers is replaced whole.
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: pin.sh <version> <homebrew-tap checkout> <install script>" >&2
  exit 2
fi
version="$1"
tap="$2"
script="$3"

semver='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$'
if ! [[ $version =~ $semver ]]; then
  echo "pin.sh: version must look like 0.2.0, got '$version'" >&2
  exit 2
fi
[ -d "$tap/Formula" ] || { echo "pin.sh: $tap/Formula not found" >&2; exit 1; }
markers="$(grep -n -E '^# (>>>|<<<) pinned release$' "$script" | cut -d: -f2- | tr '\n' '|')"
if [ "$markers" != "# >>> pinned release|# <<< pinned release|" ]; then
  echo "pin.sh: $script needs one '# >>> pinned release' line followed by one '# <<< pinned release' line" >&2
  exit 1
fi
command -v cosign >/dev/null || { echo "pin.sh: needs cosign to check the release signature" >&2; exit 1; }

tag="compute-v$version"
base="https://github.com/open-covenant/covenant/releases/download/$tag"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
for f in checksums.txt checksums.txt.sig checksums.txt.pem; do
  curl --proto '=https' --tlsv1.2 -fsSL "$base/$f" -o "$work/$f" ||
    { echo "pin.sh: cannot download $base/$f; is $tag published?" >&2; exit 1; }
done
cosign verify-blob \
  --signature "$work/checksums.txt.sig" \
  --certificate "$work/checksums.txt.pem" \
  --certificate-identity "https://github.com/open-covenant/covenant/.github/workflows/compute-release.yml@refs/tags/$tag" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  "$work/checksums.txt" >/dev/null

sha() {
  local value
  value="$(awk -v f="covenant-compute-$1.tar.gz" '$2 == f { print $1 }' "$work/checksums.txt")"
  if [ "${#value}" -ne 64 ] || [[ $value == *[!0-9a-f]* ]]; then
    echo "pin.sh: checksums.txt has no single checksum for covenant-compute-$1.tar.gz" >&2
    exit 1
  fi
  printf '%s' "$value"
}
darwin_arm64="$(sha darwin-arm64)"
darwin_x86_64="$(sha darwin-x86_64)"
linux_x86_64="$(sha linux-x86_64)"
linux_arm64="$(sha linux-arm64)"

cat > "$tap/Formula/covenant-compute.rb" <<EOF
class CovenantCompute < Formula
  desc "Rent GPUs by the second, hire coding agents and run nodes on Covenant Compute"
  homepage "https://docs.opencovenant.org/compute"
  version "$version"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "$base/covenant-compute-darwin-arm64.tar.gz"
      sha256 "$darwin_arm64"
    end
    on_intel do
      url "$base/covenant-compute-darwin-x86_64.tar.gz"
      sha256 "$darwin_x86_64"
    end
  end

  on_linux do
    on_arm do
      url "$base/covenant-compute-linux-arm64.tar.gz"
      sha256 "$linux_arm64"
    end
    on_intel do
      url "$base/covenant-compute-linux-x86_64.tar.gz"
      sha256 "$linux_x86_64"
    end
  end

  def install
    bin.install "covenant-compute", "covenant-compute-mcp", "covenant-compute-node", "covenant-compute-stake"
  end

  test do
    %w[covenant-compute covenant-compute-mcp covenant-compute-node covenant-compute-stake].each do |cmd|
      assert_equal "#{cmd} #{version}", shell_output("#{bin}/#{cmd} --version").strip
    end
  end
end
EOF

cat > "$work/block" <<EOF
version=$version
sha256_darwin_arm64=$darwin_arm64
sha256_darwin_x86_64=$darwin_x86_64
sha256_linux_x86_64=$linux_x86_64
sha256_linux_arm64=$linux_arm64
EOF
awk -v block="$work/block" '
  /^# >>> pinned release$/ { print; while ((getline line < block) > 0) print line; skip = 1; next }
  /^# <<< pinned release$/ { skip = 0 }
  !skip { print }
' "$script" > "$work/script"
cat "$work/script" > "$script"

echo "pinned $tag into $tap/Formula/covenant-compute.rb and $script"
