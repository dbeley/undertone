{
  description = "Undertone — Subsonic/Navidrome client for very weak Linux hardware";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "armv7l-linux"
      ];

      forAllSystems =
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          f {
            pkgs = nixpkgs.legacyPackages.${system};
          }
        );

      # `wrapper = false` builds just the binary: no runtime PATH wrapping and
      # no release-tuning. That is what the ARM cross-check needs, and it keeps
      # the build out of mpv's and ffmpeg's dependency trees.
      mkUndertone =
        {
          pkgs,
          wrapper ? true,
        }:
        pkgs.rustPlatform.buildRustPackage {
          pname = "undertone";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          env =
            if wrapper then
              {
                CARGO_PROFILE_RELEASE_LTO = "fat";
                CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "1";
                CARGO_PROFILE_RELEASE_PANIC = "abort";
                CARGO_PROFILE_RELEASE_STRIP = "symbols";
              }
            else
              { };

          nativeBuildInputs = if wrapper then [ pkgs.makeWrapper ] else [ ];

          # mpv and chafa are runtime dependencies, not build inputs: the
          # binary execs them. Wrapping puts both on PATH so a fresh NixOS
          # install works without the user installing anything by hand.
          postInstall = ''
            wrapProgram $out/bin/undertone \
              --prefix PATH : ${
                pkgs.lib.makeBinPath [
                  pkgs.mpv
                  pkgs.chafa
                ]
              }
          '';

          # Tests run in CI via `cargo test`, where fixtures and tools exist.
          doCheck = false;

          meta = {
            description = "Subsonic/Navidrome client for very weak Linux hardware";
            homepage = "https://github.com/dbeley/undertone";
            license = pkgs.lib.licenses.gpl3Plus;
            mainProgram = "undertone";
          };
        };
    in
    {
      packages =
        nixpkgs.lib.recursiveUpdate
          (forAllSystems (
            { pkgs }:
            {
              undertone = mkUndertone { inherit pkgs; };
              default = mkUndertone { inherit pkgs; };
            }
          ))
          {
            # Every target this app is for is ARM, so an x86_64 build proves
            # little. This cross-builds for aarch64 from an x86_64 machine: build
            # scripts run natively, only the crate code is compiled for the
            # target, so it needs no emulation. CI builds it.
            x86_64-linux.cross-aarch64 = mkUndertone {
              wrapper = false;
              pkgs = import nixpkgs {
                system = "x86_64-linux";
                crossSystem = "aarch64-unknown-linux-gnu";
              };
            };
          };

      devShells = forAllSystems (
        { pkgs }:
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              rustfmt
              clippy
              chafa
              mpv
              just
              pkg-config
              # scripts/degradation.py drives the binary through a pty in CI
              python3
            ];
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          };
        }
      );

      overlays.default = final: _prev: {
        undertone = self.packages.${final.system}.undertone;
      };

      nixosModules.default =
        {
          config,
          lib,
          pkgs,
          ...
        }:
        let
          cfg = config.programs.undertone;
        in
        {
          options.programs.undertone = {
            enable = lib.mkEnableOption "Undertone, a Subsonic/Navidrome terminal client";

            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.system}.undertone;
              description = "The undertone package to use.";
            };

            settings = lib.mkOption {
              type = lib.types.attrs;
              default = { };
              example = {
                server = "https://music.example.org";
                username = "david";
                scrobble = true;
              };
              description = ''
                Rendered to /etc/undertone/config.toml, which undertone reads as a
                fallback when no user config exists. Do not put a password here.
              '';
            };
          };

          config = lib.mkIf cfg.enable {
            environment.systemPackages = [ cfg.package ];
            environment.etc."undertone/config.toml" = lib.mkIf (cfg.settings != { }) {
              text = lib.generators.toTOML { } cfg.settings;
            };
          };
        };

      formatter = forAllSystems ({ pkgs }: pkgs.nixfmt);
    };
}
