{
  description = "MSPM0 low-power sleep work on Embassy (LP-MSPM0L1306) + EnergyTrace tooling";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    flake-parts.inputs.nixpkgs-lib.follows = "nixpkgs";
    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [ "x86_64-linux" "aarch64-linux" ];

      perSystem = { lib, pkgs, system, ... }:
        let
          # Everything here is prebuilt (binary cache / rustup dist) so the
          # shell never compiles anything. In particular: the stable
          # rust-analyzer component, not fenix's from-source nightly one.
          fenixPkgs = inputs.fenix.packages.${system};
          rustToolchain = fenixPkgs.combine [
            (fenixPkgs.stable.withComponents [
              "rustc"
              "cargo"
              "clippy"
              "rustfmt"
              "rust-src"
              "rust-analyzer"
            ])
            fenixPkgs.targets.thumbv6m-none-eabi.stable.rust-std # MSPM0 is Cortex-M0+
          ];
        in
        {
          # TI CCS Theia: reverse-engineering reference for the XDS110 /
          # EnergyTrace libraries only. Multi-GB, x86_64 only, and deliberately
          # NOT in the dev shell: `nix build .#ccs-theia`.
          packages = {
            # EnergyTrace current measurement with the LaunchPad's XDS110:
            # `nix run .#etrace -- live`. Built on demand, not part of the shell.
            etrace = pkgs.rustPlatform.buildRustPackage {
              pname = "etrace";
              version = "0.1.0";
              src = ./tools/etrace;
              cargoLock.lockFile = ./tools/etrace/Cargo.lock;
              nativeBuildInputs = [ pkgs.pkg-config ];
              buildInputs = [ pkgs.libusb1 ];
              # The integration tests read captures that live outside this source tree.
              doCheck = false;
              meta.mainProgram = "etrace";
            };
          } // lib.optionalAttrs (system == "x86_64-linux") {
            ccs-theia = (import inputs.nixpkgs {
              inherit system;
              config.allowUnfreePredicate = pkg:
                builtins.elem (lib.getName pkg) [ "ccs-theia" "ccs-theia-unwrapped" ];
            }).callPackage ./nix/ccs-theia.nix { };
          };

          devShells.default = pkgs.mkShell {
            name = "mspm0sleep";

            packages = [ rustToolchain ] ++ (with pkgs; [
              probe-rs-tools
              libusb1 # etrace talks to the XDS110 directly
              pkg-config
              uhubctl # power-cycle the LaunchPad's hub port
              radare2 # disassembly of TI's XDS110/EnergyTrace libs
            ]);

            # etrace built with cargo links libusb-1.0 dynamically with no rpath.
            LD_LIBRARY_PATH = "${pkgs.libusb1}/lib";
          };
        };
    };
}
