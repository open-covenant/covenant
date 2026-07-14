{
  # HEAD-only flake: builds the working tree, not a tagged release.
  # No derivation hash pinning, no binary cache, no signatures. Release
  # binding (pinned rev, cache signing, CI install checks) is operator-owned.
  description = "Covenant - local control plane for governed autonomous agents (HEAD-only flake)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      covenantFor = pkgs:
        pkgs.rustPlatform.buildRustPackage {
          pname = "covenant";
          version = "0.1.0-head";

          # HEAD-only: the flake's own tree is the source; no fetched archive.
          src = ./agent-os;
          cargoLock.lockFile = ./agent-os/Cargo.lock;

          # Same two binaries as Formula/covenant.rb and the source installer.
          cargoBuildFlags = [ "-p" "covenantd" "-p" "covenant" ];

          # The covenant CLI reaches openssl-sys through the solana-sdk
          # dependency chain; without these the build only succeeds where a
          # system OpenSSL leaks into the sandbox.
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.openssl ];

          # Workspace tests run in repo validators; the package build stays
          # focused on producing the daemon and CLI binaries.
          doCheck = false;

          meta = {
            description = "Local control plane for governed autonomous agents";
            homepage = "https://opencovenant.org";
            license = pkgs.lib.licenses.asl20;
            mainProgram = "covenantd";
          };
        };
    in
    {
      packages = forAllSystems (pkgs: rec {
        covenant = covenantFor pkgs;
        default = covenant;
      });

      # Linux counterpart of the formula's launchd service block.
      nixosModules.covenant = { config, lib, pkgs, ... }:
        let
          cfg = config.services.covenant;
        in
        {
          options.services.covenant = {
            enable = lib.mkEnableOption "Covenant local control plane daemon";
            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.stdenv.hostPlatform.system}.covenant;
              defaultText = lib.literalExpression "covenant.packages.\${system}.covenant";
              description = "Covenant package providing covenantd and covenant.";
            };
          };

          config = lib.mkIf cfg.enable {
            systemd.services.covenantd = {
              description = "Covenant local control plane daemon";
              wantedBy = [ "multi-user.target" ];
              after = [ "network.target" ];
              # The daemon reads its data directory from COVENANT_HOME; the
              # module pins it to the systemd-managed state directory.
              environment.COVENANT_HOME = "/var/lib/covenant";
              serviceConfig = {
                ExecStart = "${cfg.package}/bin/covenantd";
                Restart = "always";
                StateDirectory = "covenant";
              };
            };
          };
        };
    };
}
