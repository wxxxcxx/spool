{ self, ... }:
{
  flake.darwinModules.paneru-bar =
    {
      config,
      lib,
      pkgs,
      ...
    }:
    let
      cfg = config.services.paneruBar;
    in
    {
      options.services.paneruBar = {
        enable = lib.mkEnableOption "the Paneru native workspace bar";

        package = lib.mkOption {
          type = lib.types.package;
          default = self.packages.${pkgs.stdenv.hostPlatform.system}.paneru-bar;
          description = "The PaneruBar package to install and run.";
        };

        paneruPackage = lib.mkOption {
          type = lib.types.package;
          default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          description = "The matching Paneru package whose CLI PaneruBar uses.";
        };
      };

      config = lib.mkIf cfg.enable {
        environment.systemPackages = [ cfg.package ];
        launchd.user.agents.paneru-bar = {
          serviceConfig = {
            Label = "com.wxxxcxx.paneru-bar";
            ProgramArguments = [ (lib.getExe cfg.package) ];
            EnvironmentVariables = {
              PANERU_CLI = lib.getExe cfg.paneruPackage;
            };
            KeepAlive = {
              Crashed = true;
              SuccessfulExit = false;
            };
            ProcessType = "Interactive";
            RunAtLoad = true;
            StandardOutPath = "/tmp/paneru-bar.log";
            StandardErrorPath = "/tmp/paneru-bar.err.log";
          };
        };
      };
    };
}
