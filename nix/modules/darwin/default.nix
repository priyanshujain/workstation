{ config, lib, ... }:

let
  users = config.home-manager.users or { };
  logs = home: name: {
    StandardOutPath = "${home}/Library/Logs/wsctl-${name}.log";
    StandardErrorPath = "${home}/Library/Logs/wsctl-${name}.log";
  };

  agentsFor = user: hm:
    let
      cfg = hm.services.wsctl;
      wsctl = lib.getExe cfg.package;
      home = hm.home.homeDirectory;
      jobs = lib.mapAttrs' (name: job: lib.nameValuePair "wsctl-${name}" {
        serviceConfig = {
          ProgramArguments = [ wsctl ] ++ job.command;
          StartInterval = job.interval;
          ProcessType = "Background";
          LowPriorityIO = true;
        } // logs home name;
      }) cfg.jobs;
      display = lib.optionalAttrs cfg.display.enable {
        wsctl-display.serviceConfig = {
          ProgramArguments = [ wsctl "-v" "display" "apply" ];
          RunAtLoad = true;
          WatchPaths = [ "/Library/Preferences/com.apple.windowserver.displays.plist" ];
          StartInterval = 5;
          ProcessType = "Background";
        } // logs home "display";
      };
    in
    jobs // display;
in
{
  options.services.wsctl.agentsFromHome = lib.mkEnableOption
    "launchd user agents for each home-manager user's wsctl jobs and display pin, run without a shell wrapper so Login Items names them wsctl";

  config = lib.mkIf config.services.wsctl.agentsFromHome {
    launchd.user.agents = lib.mkMerge (lib.mapAttrsToList agentsFor users);
  };
}
