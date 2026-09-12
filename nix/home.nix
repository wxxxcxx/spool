{ self, ... }:
{
  flake.homeModules.spool =
    {
      config,
      lib,
      pkgs,
      ...
    }:
    let
      cfg = config.services.spool;
    in
    {
      imports = [ (import ./_spool-common.nix { inherit self; }) ];

      config = lib.mkIf cfg.enable {
        assertions = [
          (lib.hm.assertions.assertPlatform "services.spool" pkgs lib.platforms.darwin)
          {
            assertion = cfg.config == null || cfg.luaConfig.enable;
            message = "services.spool.config (init.lua) requires services.spool.luaConfig.enable = true.";
          }
        ];
        home.packages = [ cfg.finalPackage ];
        launchd.agents.spool = {
          enable = true;
          config = {
            Label = "com.wxxxcxx.spool";
            KeepAlive = {
              Crashed = true;
              SuccessfulExit = false;
            };
            Nice = -20;
            ProcessType = "Interactive";
            EnvironmentVariables = {
              NO_COLOR = "1";
              XDG_CONFIG_HOME =
                if config.xdg.enable then config.xdg.configHome else "${config.home.homeDirectory}/.config";
            };
            RunAtLoad = true;
            StandardOutPath = "/tmp/spool.log";
            StandardErrorPath = "/tmp/spool.err.log";
            Program = lib.getExe cfg.finalPackage;
            ProgramArguments = [ (lib.getExe cfg.finalPackage) "service" "run" ];
          };
        };

        # Lua config (init.lua), following spool's discovery order:
        # $XDG_CONFIG_HOME/spool/init.lua, else ~/.spool.lua.
        xdg.configFile."spool/init.lua" = lib.mkIf (config.xdg.enable && cfg.config != null) {
          source = cfg.configFile;
        };
        home.file.".spool.lua" = lib.mkIf (!config.xdg.enable && cfg.config != null) {
          source = cfg.configFile;
        };
      };
    };
}
