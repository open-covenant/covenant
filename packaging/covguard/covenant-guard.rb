# Homebrew formula for covguard. Lives in the tap repo
# (open-covenant/homebrew-tap) as Formula/covenant-guard.rb.
#
#   brew install open-covenant/tap/covenant-guard
#
# The sha256 values are filled in by the release process for each covguard-v*
# tag (see packaging/covguard/README.md).
class CovenantGuard < Formula
  desc "Run a coding agent unattended: capped spend, sandboxed, signed receipt"
  homepage "https://opencovenant.org"
  version "0.1.0"
  license "Apache-2.0"

  # macOS/arm64 only for now, the OS sandbox is macOS-specific. Intel Macs and
  # Linux build from source until the bubblewrap backend lands.
  depends_on arch: :arm64
  depends_on :macos

  url "https://github.com/open-covenant/covenant/releases/download/covguard-v#{version}/covguard-darwin-arm64.tar.gz"
  sha256 "REPLACE_WITH_DARWIN_ARM64_SHA256"

  def install
    bin.install "covguard"
  end

  test do
    assert_match "covguard #{version}", shell_output("#{bin}/covguard version")
  end
end
