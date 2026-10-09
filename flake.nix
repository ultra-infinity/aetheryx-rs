{
  description = "aetheryx: a Nix evaluator built on pliron";

  inputs = {
    crane.url = "github:ipetkov/crane";
    flake-parts.url = "github:hercules-ci/flake-parts";
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  };

  outputs =
    inputs@{ self, flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      # `aetheryx` for other package sets, built with this flake's pinned
      # nixpkgs:
      #   nixpkgs.overlays = [ aetheryx.overlays.default ];
      flake.overlays.default = final: _prev: {
        aetheryx = self.packages.${final.stdenv.hostPlatform.system}.aetheryx;
      };

      perSystem =
        {
          pkgs,
          lib,
          self',
          ...
        }:
        let
          craneLib =
            assert lib.assertMsg (lib.versionAtLeast pkgs.rustc.version "1.95")
              "aetheryx needs rustc >= 1.95, but this nixpkgs has ${pkgs.rustc.version}";
            inputs.crane.mkLib pkgs;

          # The Rust sources, and the samples the tests compare against Nix.
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              (craneLib.fileset.commonCargoSources ./.)
              ./samples
            ];
          };

          commonArgs = {
            inherit src;
            strictDeps = true;
            # The workspace manifest has no [package] to take these from.
            pname = "aetheryx";
            version = "0.1.0";
          };

          # The dependencies, built once and shared by everything below.
          withDeps = commonArgs // {
            cargoArtifacts = craneLib.buildDepsOnly commonArgs;
          };
        in
        {
          # `nix build`, `nix run . -- eval file.nix`, `nix profile install`:
          packages = {
            aetheryx = craneLib.buildPackage (
              withDeps
              // {
                # The tests run in `checks.tests`.
                doCheck = false;
                meta = {
                  description = "A Nix evaluator built on pliron";
                  mainProgram = "aetheryx";
                };
              }
            );
            default = self'.packages.aetheryx;
          };

          # `nix flake check`: the project's checks.
          checks = {
            inherit (self'.packages) aetheryx;
            tests = craneLib.cargoTest withDeps;
            clippy = craneLib.cargoClippy (
              withDeps // { cargoClippyExtraArgs = "--workspace --all-targets -- --deny warnings"; }
            );
            fmt = craneLib.cargoFmt commonArgs;
            doc = craneLib.cargoDoc (
              withDeps
              // {
                cargoDocExtraArgs = "--workspace --no-deps";
                RUSTDOCFLAGS = "-D warnings";
              }
            );
          };

          # `nix develop`: the toolchain of the checks, plus editor support.
          devShells.default = craneLib.devShell {
            checks = self'.checks;
            packages = with pkgs; [
              rust-analyzer
              rustup
            ];
          };
        };
    };
}
