{
  inputs = {
    naersk.url = "github:nix-community/naersk/master";
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      utils,
      naersk,
    }:
    utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        naersk-lib = pkgs.callPackage naersk { };
        tsk = naersk-lib.buildPackage ./.;
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
            ];
            RUST_SRC_PATH = rustPlatform.rustLibSrc;
          };
      }
    )
    // {
      nixosModules.default = import ./module.nix { inherit self; };
      nixosModules.tsk-serv = self.nixosModules.default;
    };
}
