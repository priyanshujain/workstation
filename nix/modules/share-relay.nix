self:
{ config, lib, pkgs, ... }:

let
  cfg = config.services.wsctl-share-relay;
  credentials = [ "secret:${cfg.secretFile}" ]
    ++ lib.optional (cfg.databaseTokenFile != null) "database-token:${cfg.databaseTokenFile}";
  arguments = [
    "share" "relay"
    "--viewer-listen" cfg.viewerListen
    "--agent-listen" cfg.agentListen
    "--public-url" cfg.publicUrl
    "--secret-file" "%d/secret"
    "--db-url" cfg.databaseUrl
  ] ++ lib.optionals (cfg.databaseTokenFile != null) [ "--db-token-file" "%d/database-token" ]
    ++ lib.optionals (cfg.trustProxyHeader != null) [ "--trust-proxy-header" cfg.trustProxyHeader ];
in
{
  options.services.wsctl-share-relay = {
    enable = lib.mkEnableOption "the relay that serves shared terminal tabs to browsers";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.wsctl;
      defaultText = lib.literalExpression "workstation.packages.\${system}.wsctl";
    };

    viewerListen = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1:7701";
      description = "Address for browser traffic; cloudflared forwards the public hostname here.";
    };

    agentListen = lib.mkOption {
      type = lib.types.str;
      example = "100.64.0.1:7702";
      description = "Address engines connect to, normally a tailnet address.";
    };

    publicUrl = lib.mkOption {
      type = lib.types.str;
      example = "https://share.example.com";
      description = "Public origin that share links start with.";
    };

    secretFile = lib.mkOption {
      type = lib.types.path;
      description = "File with the secret engines send to register a share.";
    };

    trustProxyHeader = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "CF-Connecting-IP";
      description = "Header the proxy in front sets to the client address; login rate limits key on it. Null uses the peer address.";
    };

    databaseUrl = lib.mkOption {
      type = lib.types.str;
      default = "file:/var/lib/wsctl-share-relay/share.db";
      example = "libsql://share.example.com";
      description = "libSQL database for shares, viewers and guest prompts: a file: path or a libsql, https or http URL.";
    };

    databaseTokenFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "File with the auth token for a remote database.";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.wsctl-share-relay = {
      description = "wsctl share relay";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        ExecStart = lib.escapeShellArgs ([ (lib.getExe cfg.package) ] ++ arguments);
        LoadCredential = credentials;
        DynamicUser = true;
        StateDirectory = "wsctl-share-relay";
        Restart = "on-failure";
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectKernelLogs = true;
        ProtectControlGroups = true;
        ProtectClock = true;
        ProtectHostname = true;
        ProtectProc = "invisible";
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        CapabilityBoundingSet = "";
        SystemCallArchitectures = "native";
        SystemCallFilter = [ "@system-service" "~@privileged" ];
        UMask = "0077";
      };
    };
  };
}
