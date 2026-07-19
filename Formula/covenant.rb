class Covenant < Formula
  desc "Local control plane for governed autonomous agents"
  homepage "https://opencovenant.org"
  license "Apache-2.0"
  head "https://github.com/open-covenant/covenant.git", branch: "main"

  depends_on "rust" => :build

  def install
    cd "agent-os" do
      # --bin scopes each install: the covenantd crate also builds a
      # preempt_fixture test helper that must not land in the prefix.
      system "cargo", "install", *std_cargo_args(path: "crates/covenantd"), "--bin", "covenantd"
      system "cargo", "install", *std_cargo_args(path: "crates/covenant"), "--bin", "covenant"
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
