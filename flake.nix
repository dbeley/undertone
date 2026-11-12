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

      mkUndertone =
        pkgs:
        pkgs.rustPlatform.buildRustPackage {
          pname = "undertone";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          env = {
            CARGO_PROFILE_RELEASE_LTO = "fat";
            CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "1";
            CARGO_PROFILE_RELEASE_PANIC = "abort";
            CARGO_PROFILE_RELEASE_STRIP = "symbols";
          };

          nativeBuildInputs = [ pkgs.makeWrapper ];

          # mpv and chafa are runtime dependencies, not build inputs: the
          # binary execs them. Wrapping puts both on PATH so a fresh NixOS
          # install works without the user installing anything by hand.
          postInstall = ''
            wrapProgram $out/bin/undertone \
              --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.mpv pkgs.chafa ]}
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
      packages = forAllSystems (
        { pkgs }:
        {
          undertone = mkUndertone pkgs;
          default = mkUndertone pkgs;
        }
      );

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
            ];
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          };
        }
      );

      overlays.default = final: _prev: {
        undertone = self.packages.${final.system}.undertone;
      };

      nixosModules.default =
        { config, lib, pkgs, ... }:
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
