{ self }:

{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.tsk-serv;
in
{
  options.services.tsk-serv = {
    enable = lib.mkEnableOption "tsk read-only HTTP browser";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "self.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      description = "tsk package to use for the tsk-serv binary.";
    };

    directory = lib.mkOption {
      type = lib.types.str;
      example = "/srv/tasks";
      description = ''
        Directory inside the Git repository whose tsk data should be served.
        This is passed to tsk-serv with -C.
      '';
    };

    host = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1";
      example = "0.0.0.0";
      description = "Address for tsk-serv to bind.";
    };

    port = lib.mkOption {
      type = lib.types.port;
      default = 7935;
      description = "Port for tsk-serv to bind.";
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "tsk-serv";
      description = "User account that runs tsk-serv.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "tsk-serv";
      description = "Group account that runs tsk-serv.";
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether to open the tsk-serv port in the firewall.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.directory != "";
        message = "services.tsk-serv.directory must point at a Git repository.";
      }
    ];

    users.users = lib.mkIf (cfg.user == "tsk-serv") {
      tsk-serv = {
        inherit (cfg) group;
        isSystemUser = true;
      };
    };

    users.groups = lib.mkIf (cfg.group == "tsk-serv") {
      tsk-serv = { };
    };

    networking.firewall.allowedTCPPorts = lib.mkIf cfg.openFirewall [ cfg.port ];

    systemd.services.tsk-serv = {
      description = "tsk read-only HTTP browser";
      after = [ "network.target" ];
      wantedBy = [ "multi-user.target" ];

      serviceConfig = {
        ExecStart = "${lib.getExe' cfg.package "tsk-serv"} -C ${lib.escapeShellArg cfg.directory} --host ${lib.escapeShellArg cfg.host} --port ${toString cfg.port}";
        Restart = "on-failure";
        User = cfg.user;
        Group = cfg.group;
        WorkingDirectory = cfg.directory;
      };
    };
  };
}
