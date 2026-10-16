{
  description = "watt-start — the Project Watt Cubed launcher";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    # The package manager's library crates, which Cargo finds at ../pwc-package-manager. The dev
    # shell builds against the sibling checkout; the package against this pinned revision
    # (`nix flake update pwc-package-manager` moves it).
    pwc-package-manager = {
      url = "github:gusahlg/pwc-package-manager";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, flake-utils, pwc-package-manager }:
    flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" ] (system:
      let
        pkgs = import nixpkgs { inherit system; };

        # winit and wgpu dlopen these: Wayland (or X11) for the window, the Vulkan loader for
        # rendering. The GPU driver itself comes from the system (/run/opengl-driver on NixOS).
        runtimeLibs = with pkgs; [
          wayland
          libxkbcommon
          vulkan-loader
          libx11
          libxcursor
          libxrandr
          libxi
        ];
        libraryPath = pkgs.lib.makeLibraryPath runtimeLibs;

        # Cargo needs the package manager next to the launcher, as in a checkout.
        combinedSrc = pkgs.runCommand "source" { } ''
          mkdir -p $out
          cp -r ${self} $out/watt-start
          cp -r ${pwc-package-manager} $out/pwc-package-manager
        '';
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "watt-start";
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
          src = combinedSrc;
          sourceRoot = "source/watt-start";
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [ "-p" "watt-start" ];
          nativeBuildInputs = [ pkgs.makeWrapper ];
          # The tests render headless with wgpu and need a GPU; they run in the dev shell.
          doCheck = false;
          postInstall = ''
            install -Dm644 packaging/watt-start.desktop $out/share/applications/watt-start.desktop
          '';
          postFixup = ''
            patchelf --add-rpath "${libraryPath}" "$out/bin/watt-start"
            wrapProgram "$out/bin/watt-start" --prefix PATH : "${pkgs.lib.makeBinPath [ pkgs.git ]}"
          '';
          meta = {
            description = "Instances, mods, one-click updates and builds for Project Watt Cubed";
            license = pkgs.lib.licenses.agpl3Plus;
            mainProgram = "watt-start";
          };
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [ rustc cargo rustfmt clippy git ];
          LD_LIBRARY_PATH = "${libraryPath}:/run/opengl-driver/lib";
        };
      });
}
