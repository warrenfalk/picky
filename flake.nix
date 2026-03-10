{
  description = "picky: a modular selection/search picker for Niri";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        rustToolchain = pkgs.rust-bin.stable.latest.minimal.override {
          extensions = [ "clippy" "rust-src" "rustfmt" ];
        };
        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };
        package = rustPlatform.buildRustPackage {
          pname = "picky";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = with pkgs; [
            makeWrapper
            pkg-config
            wrapGAppsHook4
          ];
          buildInputs = with pkgs; [
            gtk4
          ];
          postFixup = ''
            wrapProgram "$out/bin/picky" \
              --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.gtk3 pkgs.niri ]}
          '';
        };
      in
      {
        packages.default = package;

        apps.default = flake-utils.lib.mkApp {
          drv = package;
        };

        checks.default = package;

        devShells.default = pkgs.mkShell {
          name = "picky";

          packages = with pkgs; [
            rustToolchain
            rust-analyzer
            pkg-config
            gtk3
            gtk4
            niri
          ];

          buildInputs = with pkgs; [
            gtk4
          ];

          NIX_SHELL_PRESERVE_PROMPT = "1";
          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";

          shellHook = ''
            export PS1="(picky) ''${PS1:-\\u@\\h:\\w \\$ }"
          '';
        };

        formatter = pkgs.nixpkgs-fmt;
      });
}
