{ lib, ... }:
let
  cleanup = lib.mkOption {
    type = lib.types.enum [ "none" "uninstall" "zap" ];
    default = "none";
    description = ''
      What apply does with explicitly installed packages that are not declared. `none` lists them as warnings,
      `uninstall` removes them, `zap` also removes their configuration files.
    '';
  };
in
{
  options.pacman = {
    packages = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "Packages installed with pacman from the distribution's repositories.";
    };
    snapshot = lib.mkOption {
      type = lib.types.nullOr (lib.types.strMatching "[0-9]{4}-[0-9]{2}-[0-9]{2}");
      default = null;
      example = "2026-10-11";
      description = "Arch Linux Archive date the repositories are pinned to. Null follows the live mirrors.";
    };
    onActivation.cleanup = cleanup;
  };

  options.aur = {
    packages = lib.mkOption {
      type = lib.types.attrsOf lib.types.path;
      default = { };
      example = lib.literalExpression "{ paru-bin = inputs.aur-paru-bin; }";
      description = "AUR packages, each built from its package repository, pinned as a flake input.";
    };
    onActivation.cleanup = cleanup;
  };
}
