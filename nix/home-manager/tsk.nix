{ self }:

{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.tsk;
  package = cfg.package;
  skillFile = pkgs.runCommand "tsk-skill.md" { nativeBuildInputs = [ package ]; } ''
    tsk skill > "$out"
  '';
in
{
  options.programs.tsk = {
    enable = lib.mkEnableOption "tsk, a command-line first task manager";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "inputs.tsk.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      description = "The tsk package to install.";
    };

    installSkill = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Whether to install the Codex/agent skill emitted by `tsk skill`.
      '';
    };

    skillPath = lib.mkOption {
      type = lib.types.str;
      default = ".agents/skills/tsk/SKILL.md";
      description = "Home-relative path where the tsk agent skill is installed.";
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        home.packages = [ package ];
      }

      (lib.mkIf cfg.installSkill {
        home.file.${cfg.skillPath}.source = skillFile;
      })
    ]
  );
}
