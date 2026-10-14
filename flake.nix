{
  description = "wsctl: one declarative manager for a developer's whole machine";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    system-manager = {
      url = "github:numtide/system-manager/release-26.05";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nix-darwin = {
      url = "github:nix-darwin/nix-darwin/nix-darwin-26.05";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, system-manager, nix-darwin }:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: rec {
        wsctl = pkgs.callPackage ./nix/package.nix {
          src = self;
          switcher =
            if pkgs.stdenv.isDarwin
            then nix-darwin.packages.${pkgs.stdenv.hostPlatform.system}.darwin-rebuild
            else system-manager.packages.${pkgs.stdenv.hostPlatform.system}.default;
        };
        default = wsctl;
      });

      systemModules.default = ./nix/modules/linux;

      systemConfigs.arch = system-manager.lib.makeSystemConfig {
        modules = [
          self.systemModules.default
          {
            nixpkgs.hostPlatform = "x86_64-linux";
            distro = "arch";
          }
        ];
      };
    };
}
