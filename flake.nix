
{
  description = "Nixgram - An easier package manager for Nix";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs = { self, nixpkgs, ... }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
    in
    {
      packages.${system}.default =
        pkgs.rustPlatform.buildRustPackage {
          pname = "nixgram";
          version = "0.1.0";

          src = self;

          cargoLock.lockFile = ./Cargo.lock;

          meta = {
            description = "An interactive package manager for NixOS flakes";
            mainProgram = "nixgram";
            platforms = pkgs.lib.platforms.linux;
          };
        };
    };
}
