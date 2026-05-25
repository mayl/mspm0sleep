{
  description = "Template Embassy Project";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    flake-parts.inputs.nixpkgs-lib.follows = "nixpkgs";
    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
    beads.url = "github:gastownhall/beads";
    beads.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [ "x86_64-linux" "aarch64-linux" ];

      perSystem = { config, lib, self', inputs', pkgs, system, ... }:
        let
          # A local nixpkgs that allows specific unfree TI packages.
          pkgsUnfree = import inputs.nixpkgs {
            inherit system;
            config.allowUnfreePredicate = pkg:
              builtins.elem (inputs.nixpkgs.lib.getName pkg) [
                "ccs-theia"
                "ccs-theia-unwrapped"
                "msp-debug-stack-bin"
              ];
          };

          # CCS Theia is x86_64-linux only.
          ccs-theia =
            if system == "x86_64-linux"
            then pkgsUnfree.callPackage ./nix/ccs-theia.nix { }
            else null;

          # Rust toolchain built with fenix: stable components + the
          # Cortex-M cross-compilation targets, plus a standalone
          # rust-analyzer.
          fenixPkgs = inputs.fenix.packages.${system};
          rustTargets = [
            "thumbv6m-none-eabi"
            "thumbv7m-none-eabi"
            "thumbv7em-none-eabi"
          ];
          rustToolchain = fenixPkgs.combine ([
            (fenixPkgs.stable.withComponents [
              "rustc"
              "cargo"
              "clippy"
              "rustfmt"
              "rust-src"
            ])
            fenixPkgs.rust-analyzer
          ] ++ map (t: fenixPkgs.targets.${t}.stable.rust-std) rustTargets);

        in
        {
          packages = {
            bd = inputs.beads.packages.${system}.bd;
            energytrace-util = pkgsUnfree.callPackage ./nix/energytrace-util.nix {
              mspds-bin = pkgsUnfree.mspds-bin;
            };
          } // pkgs.lib.optionalAttrs (ccs-theia != null) {
            ccs-theia = ccs-theia;
          };

          devShells.default = pkgs.mkShell {
            name = "embassy.rs devshell";

            packages = [ rustToolchain ] ++ (with pkgs; [
              probe-rs-tools
              cargo-embassy
              inputs.beads.packages.${system}.bd
              inputs.beads.packages.${system}.fish-completions
              libusb1       # for direct USB access to XDS110 probe
              pkg-config    # needed for libusb1 detection by rust build scripts
            ]) ++ lib.optional (config.packages.energytrace-util != null)
              config.packages.energytrace-util;

            shellHook = ''
              # repro-cli links libusb-1.0 dynamically with no rpath; the plain
              # mkShell (post devenv migration) does not export a library path,
              # so the built binary fails at runtime with
              # "libusb-1.0.so.0: cannot open shared object file". Put libusb on
              # the loader path explicitly.
              export LD_LIBRARY_PATH="${pkgs.libusb1}/lib''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
              echo "use cargo embassy init <project-name> --chip <chip_name> to make a new project"
            '';
          };
        };

      flake = { };
    };
}
