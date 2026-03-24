{
  description = "Simple Rust CLI with Nix";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      crane,
      rust-overlay,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "clippy"
            "rustfmt"
            "rust-analyzer"
          ];
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        crateInfo = craneLib.crateNameFromCargoToml { cargoToml = ./crates/loom/Cargo.toml; };

        commonArgs = {
          src = ./.;
          inherit (crateInfo) pname version;
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

      in
      {
        # 📦 Build your CLI
        packages.default = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
          }
        );

        # ▶️ nix run
        apps.default = flake-utils.lib.mkApp {
          drv = self.packages.${system}.default;
        };

        # 🛠️ Dev shell
        devShells.default = pkgs.mkShell {
          shellHook = ''
            echo "==========================================="
            echo "󱄅 ${crateInfo.pname} ${crateInfo.version} - Development Environment"
            echo "==========================================="
            echo "Rust: $(rustc --version | cut -d' ' -f2)"
            echo "Cargo: $(cargo --version | cut -d' ' -f2)"
            echo ""
            echo "Commands:"
            echo "  nix build              - Build with Nix"
            echo "  nix profile install .  - Install loom"
            echo ""
          '';
          nativeBuildInputs = [ rustToolchain ];
        };
      }
    );
}
