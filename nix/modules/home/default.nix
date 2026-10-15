self:
{ config, lib, pkgs, ... }:

let
  cfg = config.services.wsctl;
  program = job: [ (lib.getExe cfg.package) ] ++ job.command;
in
{
  options.services.wsctl = {
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.wsctl;
      defaultText = lib.literalExpression "workstation.packages.\${system}.wsctl";
    };

    agents = lib.mkOption {
      default = [ ];
      description = "Commands that start coding agents, and the environment variable each one sets in its children.";
      type = lib.types.listOf (lib.types.submodule {
        options = {
          command = lib.mkOption {
            type = lib.types.str;
            example = "claude";
          };
          marker = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            example = "CLAUDE_CODE_SESSION_ID";
          };
        };
      });
    };

    jobs = lib.mkOption {
      default = { };
      description = "wsctl subcommands run on a timer as the user; each run exits when done.";
      type = lib.types.attrsOf (lib.types.submodule {
        options = {
          command = lib.mkOption {
            type = lib.types.listOf lib.types.str;
            example = [ "disk" "refresh" ];
          };
          interval = lib.mkOption {
            type = lib.types.ints.positive;
            description = "Seconds between runs.";
          };
        };
      });
    };
  };

  config = lib.mkMerge [
    { home.packages = [ cfg.package ]; }

    (lib.mkIf (cfg.agents != [ ]) {
      xdg.configFile."wsctl/agents.json".text = builtins.toJSON { inherit (cfg) agents; };
    })

    (lib.mkIf pkgs.stdenv.isDarwin {
      launchd.agents = lib.mapAttrs' (name: job: lib.nameValuePair "wsctl-${name}" {
        enable = true;
        config = {
          ProgramArguments = program job;
          StartInterval = job.interval;
          RunAtLoad = false;
          ProcessType = "Background";
          LowPriorityIO = true;
          StandardOutPath = "${config.home.homeDirectory}/Library/Logs/wsctl-${name}.log";
          StandardErrorPath = "${config.home.homeDirectory}/Library/Logs/wsctl-${name}.log";
        };
      }) cfg.jobs;
    })

    (lib.mkIf pkgs.stdenv.isLinux {
      systemd.user.services = lib.mapAttrs' (name: job: lib.nameValuePair "wsctl-${name}" {
        Unit.Description = "wsctl ${lib.concatStringsSep " " job.command}";
        Service = {
          Type = "oneshot";
          ExecStart = lib.escapeShellArgs (program job);
          Nice = 19;
          IOSchedulingClass = "idle";
        };
      }) cfg.jobs;

      systemd.user.timers = lib.mapAttrs' (name: job: lib.nameValuePair "wsctl-${name}" {
        Unit.Description = "Run wsctl ${name} every ${toString job.interval}s";
        Timer = {
          OnBootSec = "${toString job.interval}s";
          OnUnitActiveSec = "${toString job.interval}s";
        };
        Install.WantedBy = [ "timers.target" ];
      }) cfg.jobs;
    })
  ];
}
