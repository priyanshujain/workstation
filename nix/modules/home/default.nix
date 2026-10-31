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

    display = {
      enable = lib.mkEnableOption "keeping the menu bar on the preferred display (macOS)";
      preferredMain = lib.mkOption {
        type = lib.types.nullOr (lib.types.submodule {
          options = {
            vendor = lib.mkOption { type = lib.types.int; };
            model = lib.mkOption { type = lib.types.int; };
            serial = lib.mkOption { type = lib.types.int; };
          };
        });
        default = null;
        description = "Display that should hold the menu bar whenever it is connected.";
      };
    };

    merges = lib.mkOption {
      default = { };
      description = "Scripts run on every activation that merge declared settings into files apps also write; doctor reruns them on copies to find drift.";
      type = lib.types.attrsOf (lib.types.submodule {
        options = {
          script = lib.mkOption {
            type = lib.types.package;
            description = "Executable that edits the files under $HOME in place.";
          };
          files = lib.mkOption {
            type = lib.types.listOf lib.types.str;
            description = "Files the script edits, relative to $HOME; shell globs allowed.";
          };
        };
      });
    };

    checks = lib.mkOption {
      default = { };
      description = "Shell commands doctor runs; each exits 0 when the live state matches what is declared elsewhere.";
      type = lib.types.attrsOf (lib.types.submodule {
        options.command = lib.mkOption { type = lib.types.str; };
      });
    };

    defaultApps = {
      types = lib.mkOption {
        type = lib.types.attrsOf lib.types.str;
        default = { };
        description = "Content types or filename extensions, each mapped to the bundle id of the app that opens it (macOS).";
        example = { "public.plain-text" = "dev.zed.Zed"; yaml = "dev.zed.Zed"; };
      };
      replace = lib.mkOption {
        type = lib.types.attrsOf lib.types.str;
        default = { };
        description = "Apps that must not stay a default handler, each mapped to the app that takes the types it holds (macOS).";
        example = { "com.apple.TextEdit" = "dev.zed.Zed"; };
      };
    };

    xcode = {
      version = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Xcode version `wsctl apply` installs with xcodes, without simulators, and selects (macOS).";
        example = "27.1";
      };
      appleId = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "rbw entry holding the Apple ID that downloads Xcode; without it xcodes asks.";
      };
    };

    trackInputs = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "Flake inputs `wsctl apply` moves to their latest commit; every other input moves only on `wsctl upgrade`.";
      example = [ "workstation" ];
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

    (lib.mkIf (cfg.merges != { }) {
      xdg.configFile."wsctl/merges.json".text = builtins.toJSON (lib.mapAttrs (_: merge: {
        script = "${merge.script}";
        inherit (merge) files;
      }) cfg.merges);
      home.activation = lib.mapAttrs' (name: merge: lib.nameValuePair "wsctl-merge-${name}"
        (lib.hm.dag.entryAfter [ "linkGeneration" ] "run ${merge.script}")) cfg.merges;
    })

    (lib.mkIf (cfg.checks != { }) {
      xdg.configFile."wsctl/checks.json".text = builtins.toJSON (lib.mapAttrs (_: check: check.command) cfg.checks);
    })

    (lib.mkIf (pkgs.stdenv.isDarwin && cfg.xcode.version != null) {
      xdg.configFile."wsctl/xcode.json".text = builtins.toJSON {
        inherit (cfg.xcode) version appleId;
        xcodes = lib.getExe' pkgs.xcodes "xcodes";
      };
      services.wsctl.checks.xcode.command =
        "${lib.getExe' pkgs.xcodes "xcodes"} installed | grep -F '(Selected)' | grep -q '^${cfg.xcode.version} '";
    })

    (lib.mkIf (pkgs.stdenv.isDarwin && (cfg.defaultApps.types != { } || cfg.defaultApps.replace != { })) {
      xdg.configFile."wsctl/default-apps.json".text = builtins.toJSON cfg.defaultApps;
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

    (lib.mkIf (pkgs.stdenv.isDarwin && cfg.display.enable) {
      xdg.configFile."wsctl/display.json".text = builtins.toJSON (
        lib.optionalAttrs (cfg.display.preferredMain != null) { preferred_main = cfg.display.preferredMain; }
      );

      launchd.agents.wsctl-display = {
        enable = true;
        config = {
          ProgramArguments = [ (lib.getExe cfg.package) "-v" "display" "apply" ];
          RunAtLoad = true;
          WatchPaths = [ "/Library/Preferences/com.apple.windowserver.displays.plist" ];
          StartInterval = 5;
          ProcessType = "Background";
          StandardOutPath = "${config.home.homeDirectory}/Library/Logs/wsctl-display.log";
          StandardErrorPath = "${config.home.homeDirectory}/Library/Logs/wsctl-display.log";
        };
      };
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
