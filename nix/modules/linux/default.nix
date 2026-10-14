{ config, lib, ... }:
let
  distros = import ./distros.nix { inherit lib; };
in
{
  imports = [ ./packages.nix ];

  options.distro = lib.mkOption {
    type = lib.types.enum (builtins.attrNames distros);
    description = "The Linux distribution underneath. Selects the users and groups kept as the distribution made them.";
  };

  config = {
    system-manager.allowAnyDistro = true;
    users = distros.${config.distro};
  };
}
