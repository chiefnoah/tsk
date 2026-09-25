{
  inputs = {
    naersk.url = "github:nix-community/naersk/master";
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    pre-commit-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      pre-commit-hooks,
      utils,
      naersk,
    }:
    let
      homeManagerModules = rec {
        tsk = import ./nix/home-manager/tsk.nix { inherit self; };
        default = tsk;
      };
    in
    utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        naersk-lib = pkgs.callPackage naersk { };
        tsk = naersk-lib.buildPackage ./.;
        pre-commit-check = pre-commit-hooks.lib.${system}.run {
          src = ./.;
          hooks = {
            clippy.enable = true;
            nixfmt.enable = true;
            rustfmt.enable = true;
          };
        };
      in
      {
        packages.default = tsk;
        defaultPackage = tsk;
        devShell =
          with pkgs;
          mkShell {
            buildInputs = [
              libiconv
              cargo
              rustc
              rustfmt
              rust-analyzer
              rustPackages.clippy
              plan9port
              pandoc
              codeberg-cli
            ]
            ++ pre-commit-check.enabledPackages;
            inherit (pre-commit-check) shellHook;
            RUST_SRC_PATH = rustPlatform.rustLibSrc;
          };
      }
    )
    // {
      inherit homeManagerModules;
      nixosModules.default = import ./module.nix { inherit self; };
      nixosModules.tsk-serv = self.nixosModules.default;
    };
}
