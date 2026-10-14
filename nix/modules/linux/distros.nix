{ lib }:
{
  arch = {
    users.root.shell = "/bin/bash";
    users.nobody = {
      group = lib.mkForce "nobody";
      shell = "/usr/bin/nologin";
    };
    groups.nobody.gid = 65534;
    groups.nogroup.gid = lib.mkForce null;
    groups.tape.gid = lib.mkForce null;
  };
}
