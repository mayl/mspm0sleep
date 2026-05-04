{
  description = "Template Embassy Project";

  inputs = {
    devenv-root = {
      url = "file+file:///dev/null";
      flake = false;
    };
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    devenv.url = "github:cachix/devenv";
    devenv.inputs.nixpkgs.follows = "nixpkgs";
    mk-shell-bin.url = "github:rrbutani/nix-mk-shell-bin";
    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    beads.url = "github:gastownhall/beads";
    beads.inputs.nixpkgs.follows = "nixpkgs";
  };

  nixConfig = {
    extra-trusted-public-keys = "devenv.cachix.org-1:w1cLUi8dv3hnoSPGAuibQv+f9TZLr6cv/Hm9XgU50cw=";
    extra-substituters = "https://devenv.cachix.org";
  };

  outputs = inputs@{ flake-parts, devenv-root, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        inputs.devenv.flakeModule
      ];
      systems = [ "x86_64-linux" "aarch64-linux" ];

      perSystem = { config, lib, self', inputs', pkgs, system, ... }:
        let
          # A local nixpkgs that allows specific unfree TI packages.
          # Kept separate from the main `pkgs` so devenv continues
          # to see a stock nixpkgs.
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

        in
        {
          packages = {
            bd = inputs.beads.packages.${system}.bd;
            energytrace-util = pkgsUnfree.callPackage ./nix/energytrace-util.nix { };
          } // pkgs.lib.optionalAttrs (ccs-theia != null) {
            ccs-theia = ccs-theia;
          };

        devenv.shells.default = {
          devenv.root =
            let
              devenvRootFileContent = builtins.readFile devenv-root.outPath;
            in
            pkgs.lib.mkIf (devenvRootFileContent != "") devenvRootFileContent;

          name = "embassy.rs devshell";

          imports = [ ];

          packages = with pkgs; [
            probe-rs-tools
            cargo-embassy
            mspds-bin
            inputs.beads.packages.${system}.bd
            inputs.beads.packages.${system}.fish-completions
          ] ++ lib.optional (energytrace-util != null) energytrace-util;

          enterShell = ''
            echo "use cargo embassy init <project-name> --chip <chip_name> to make a new project"
          '';

          languages.rust = {
            enable = true;
            channel = "stable";
            components = [
              "rustc"
              "cargo"
              "clippy"
              "rustfmt"
              "rust-analyzer"
              "rust-src"
            ];
            targets = [
              "thumbv6m-none-eabi"
              "thumbv7m-none-eabi"
              "thumbv7em-none-eabi"
            ];
          };
        };

      };
      flake = { };
    };
}
