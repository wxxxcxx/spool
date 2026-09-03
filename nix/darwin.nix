{ lib, self, ... }:
{
  flake.darwinModules = {
    spool =
      { config, pkgs, ... }:
      let
        cfg = config.services.spool;
      in
      {
        imports = [ (import ./_spool-common.nix { inherit self; }) ];

        config = lib.mkIf cfg.enable {
          assertions = [
            {
              assertion = cfg.config == null || cfg.luaConfig.enable;
              message = "services.spool.config (init.lua) requires services.spool.luaConfig.enable = true.";
            }
          ];
          environment.systemPackages = [ cfg.finalPackage ];
          # TODO: Once nix-darwin supports it, prefer `launchd.agents.spool` so `system.primaryUser` is not needed.
          # See <https://github.com/nix-darwin/nix-darwin/issues/1255>
          launchd.user.agents.spool = {
            serviceConfig = {
              Label = "com.wxxxcxx.spool";
              KeepAlive = {
                Crashed = true;
                SuccessfulExit = false;
              };
              Nice = -20;
              ProcessType = "Interactive";
              EnvironmentVariables = {
                # The spool.setup{...} in SPOOL_LUA (init.lua) takes precedence
                # over the options in SPOOL_CONFIG (spool.toml).
                SPOOL_CONFIG = lib.mkIf (cfg.settings != null) (toString cfg.settingsFile);
                SPOOL_LUA = lib.mkIf (cfg.config != null) (toString cfg.configFile);
                NO_COLOR = "1";
              };
              RunAtLoad = true;
              StandardOutPath = "/tmp/spool.log";
              StandardErrorPath = "/tmp/spool.err.log";
              Program = lib.getExe cfg.finalPackage;
            };
          };
        };
      };

    "spool-bar" =
      {
        config,
        lib,
        pkgs,
        ...
      }:
      let
        cfg = config.services.spoolBar;
      in
      {
        options.services.spoolBar = {
          enable = lib.mkEnableOption "the Spool native workspace bar";

          package = lib.mkOption {
            type = lib.types.package;
            default = self.packages.${pkgs.stdenv.hostPlatform.system}.spool-bar;
            description = "The SpoolBar package to install and run.";
          };

          spoolPackage = lib.mkOption {
            type = lib.types.package;
            default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
            description = "The matching Spool package whose CLI SpoolBar uses.";
          };
        };

        config = lib.mkIf cfg.enable {
          environment.systemPackages = [ cfg.package ];
          launchd.user.agents.spool-bar = {
            serviceConfig = {
              Label = "com.wxxxcxx.spool-bar";
              ProgramArguments = [ (lib.getExe cfg.package) ];
              EnvironmentVariables = {
                SPOOL_CLI = lib.getExe cfg.spoolPackage;
              };
              KeepAlive = {
                Crashed = true;
                SuccessfulExit = false;
              };
              ProcessType = "Interactive";
              RunAtLoad = true;
              StandardOutPath = "/tmp/spool-bar.log";
              StandardErrorPath = "/tmp/spool-bar.err.log";
            };
          };
        };
      };
  };
}
