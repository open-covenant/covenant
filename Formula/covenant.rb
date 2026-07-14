class Covenant < Formula
  desc "Local control plane for governed autonomous agents"
  homepage "https://opencovenant.org"
  head "https://github.com/open-covenant/covenant.git", branch: "main"

  license "Apache-2.0"

  depends_on "rust" => :build

  def install
    cd "agent-os" do
      system "cargo", "build", "-p", "covenantd", "-p", "covenant", "--locked", "--release"
      bin.install "target/release/covenantd"
      bin.install "target/release/covenant"
    end
  end

  def caveats
    <<~EOS
      Covenant reads its data directory from $COVENANT_HOME (default: ~/.covenant).
      The launchd service overrides this to the Homebrew var/covenant directory.

      HEAD formula: built from the main branch via `brew install --HEAD`.
      Not bound to a tagged release; no bottles, no signatures, not a tap.
    EOS
  end

  service do
    run [opt_bin/"covenantd"]
    keep_alive true
    log_path var/"log/covenantd.log"
    error_log_path var/"log/covenantd.log"
    environment_variables COVENANT_HOME: var/"covenant"
  end

  test do
    assert_match "covenantd 0.1.0", shell_output("#{bin}/covenantd --version")
    assert_match "usage", shell_output("#{bin}/covenant 2>&1", 2)
  end
end
