{ lib }:
{
  arch = {
    users.root = {
      description = lib.mkForce "";
      shell = "/bin/bash";
    };
    users.nobody = {
      description = lib.mkForce "Kernel Overflow User";
      home = lib.mkForce "/";
      group = lib.mkForce "nobody";
      shell = "/usr/bin/nologin";
    };
    groups.nobody.gid = 65534;
    groups.nogroup.gid = lib.mkForce null;
    groups.tape.gid = lib.mkForce null;
  };
}
