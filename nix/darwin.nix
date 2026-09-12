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
                SPOOL_LUA = lib.mkIf (cfg.config != null) (toString cfg.configFile);
                NO_COLOR = "1";
              };
              RunAtLoad = true;
              StandardOutPath = "/tmp/spool.log";
              StandardErrorPath = "/tmp/spool.err.log";
              Program = lib.getExe cfg.finalPackage;
              ProgramArguments = [ (lib.getExe cfg.finalPackage) "service" "run" ];
            };
          };
        };
      };

  };
}
