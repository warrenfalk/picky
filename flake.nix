{
  description = "picky: a modular selection/search picker for Niri";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        runtimeLibs = with pkgs; [
          wayland
          libxkbcommon
          xorg.libX11
          xorg.libXcursor
          xorg.libXi
          xorg.libXrandr
          libGL
          vulkan-loader
        ];
        graphicsLoaderLibs = with pkgs; [
          libxkbcommon
          libglvnd
          vulkan-loader
        ];
        graphicsLoaderPath = pkgs.lib.makeLibraryPath graphicsLoaderLibs;
        graphicsRuntimeHook = ''
          wayland_dir=""
          for icd_json in /run/opengl-driver/share/vulkan/icd.d/*.json; do
            [ -r "$icd_json" ] || continue
            vulkan_icd=$(sed -n 's/.*"library_path": "\(.*\)".*/\1/p' "$icd_json")
            [ -r "$vulkan_icd" ] || continue
            wayland_lib=$(LD_LIBRARY_PATH= ldd "$vulkan_icd" 2>/dev/null | awk '/libwayland-client.so.0/ { print $3; exit }')
            if [ -n "$wayland_lib" ]; then
              wayland_dir=$(dirname "$wayland_lib")
              break
            fi
          done

          export LD_LIBRARY_PATH="${graphicsLoaderPath}''${wayland_dir:+:''${wayland_dir}}"
        '';
        graphicsRuntimeScript = pkgs.writeShellScript "picky-graphics-runtime" ''
          ${graphicsRuntimeHook}
        '';
        runtimeBinPath = pkgs.lib.makeBinPath [
          pkgs.firefox
          pkgs.gtk3
          pkgs.niri
        ];
        rustToolchain = pkgs.rust-bin.stable.latest.minimal.override {
          extensions = [
            "clippy"
            "rust-src"
            "rustfmt"
          ];
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
          buildInputs =
            with pkgs;
            [
              gtk4
            ]
            ++ runtimeLibs;
          postFixup = ''
            for bin in picky wgpu_probe; do
              wrapped="$out/bin/.''${bin}-wrapped"
              [ -x "$wrapped" ] || continue

              printf '%s\n' \
                '#!${pkgs.runtimeShell}' \
                'set -eu' \
                '. ${graphicsRuntimeScript}' \
                "export PATH=\"${runtimeBinPath}:\$PATH\"" \
                "exec \"$wrapped\" \"\$@\"" \
                > "$out/bin/''${bin}"

              chmod +x "$out/bin/''${bin}"
            done
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
            cage
            firefox
            grim
            mesa-demos
            nixfmt-rfc-style
            rustToolchain
            rust-analyzer
            pkg-config
            gtk3
            gtk4
            niri
            slurp
            vulkan-tools
            wayland-utils
            weston
            wl-clipboard
            wtype
            ydotool
          ];

          buildInputs = with pkgs; [
            gtk4
          ];

          NIX_SHELL_PRESERVE_PROMPT = "1";
          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";

          shellHook = ''
            ${graphicsRuntimeHook}
            export PS1="(picky) ''${PS1:-\\u@\\h:\\w \\$ }"
          '';
        };

        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
