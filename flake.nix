{
  inputs = {
    # This must be the stable nixpkgs if you're running the app on a
    # stable NixOS install.  Mixing EGL library versions doesn't work.
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
    utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-compat = {
      url = github:edolstra/flake-compat;
      flake = true;
    };
  };

  outputs = { self, nixpkgs, utils, rust-overlay, ... }:
    utils.lib.eachSystem ["x86_64-linux" "aarch64-linux"] (system:
      let
        arch = {
            "x86_64-linux" = "amd64";
            "aarch64-linux" = "arm64";
        };

        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
            inherit system overlays;
            config.allowUnfree = true;
        };

        rustVersion = pkgs.rust-bin.stable.latest.default;

        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustVersion;
          rustc = rustVersion;
        };

        manifest = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package;

        commonBuildInputs = with pkgs; [
          pkg-config
          openssl
        ];

        package = pkgs.rust.packages.stable.rustPlatform.buildRustPackage rec{
          pname = manifest.name;
          version = manifest.version;
          src = pkgs.lib.cleanSource ./.;
          cargoBuildFlags = "";

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          nativeBuildInputs = [
            pkgs.autoPatchelfHook
          ];

          buildInputs = with pkgs; [
            pkgs.rust-bin.stable.latest.default
          ] ++ commonBuildInputs;

          # Certain Rust tools won't work without this
          # This can also be fixed by using oxalica/rust-overlay and specifying the rust-src extension
          # See https://discourse.nixos.org/t/rust-src-not-found-and-other-misadventures-of-developing-rust-on-nixos/11570/3?u=samuela. for more details.
          RUST_SRC_PATH = pkgs.rust.packages.stable.rustPlatform.rustLibSrc;
          PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";
          #LD_LIBRARY_PATH = libPath;
          OPENSSL_LIB_DIR = pkgs.openssl.out + "/lib";

          meta = {
            description = "A Maven-Repository Server";
            homepage = "https://github.com/C0D3-M4513R/maven-repo";
            license = nixpkgs.lib.licenses.unfree; #This repo has no license and should be taken as All-Rights-Reserved.
            maintainers = [];
            mainProgram = "maven-repo";
          };
        };
        docker = pkgs.dockerTools.buildLayeredImage {
            name = manifest.name;
            tag = "${manifest.version}-${arch."${system}"}";

            contents = [ package ];

            config = {
                ExposedPorts = {
                    "8000" = {};
                };
                WorkingDir = "/data";
                Cmd = [ "${nixpkgs.lib.getExe package}" ];
            };

            extraCommands = ''
                mkdir -p data
            '';

            created = "${builtins.substring 0 4 self.lastModifiedDate}-${builtins.substring 4 2 self.lastModifiedDate}-${builtins.substring 6 2 self.lastModifiedDate}T${builtins.substring 8 2 self.lastModifiedDate}:${builtins.substring 10 2 self.lastModifiedDate}:${builtins.substring 12 2 self.lastModifiedDate}Z";
        };
      in
      {
        packages = {
            "${manifest.name}" = package;
            "${manifest.name}-docker" = docker;
        };

        defaultApp = utils.lib.mkApp {
          drv = self.defaultPackage."${system}";
        };

        devShell = with pkgs; mkShell {
          buildInputs = [
            #cargo
            cargo-insta
            docker-compose
            pre-commit
            samply    #for cpu  profiling
            heaptrack #for heap analysis
            sqlite-interactive #for sqlite db diff's
            #rust-analyzer
            #rustPackages.clippy
            #rustc
            #rustfmt
            tokei
        (
            python312.withPackages (ps: with ps; [
                requests
                colorama
                aiohttp
            ])
        )
          ] ++ commonBuildInputs;
          RUST_SRC_PATH = pkgs.rust.packages.stable.rustPlatform.rustLibSrc;
          LD_LIBRARY_PATH = lib.makeLibraryPath commonBuildInputs;
          GIT_EXTERNAL_DIFF = "${difftastic}/bin/difft";
          RUST_BACKTRACE= "1";
          RUST_LIB_BACKTRACE = "1";
        };
      });
}
