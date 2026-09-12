# Evaluate both module constructors without building/activating a user system.
{ lib, pkgs, self }:
let
  package = pkgs.runCommand "spool-cli-argument-fixture" {
    meta.mainProgram = "spool";
  } "exit 1";
  config = {
    services.spool = {
      enable = true;
      finalPackage = package;
      config = null;
      luaConfig.enable = false;
    };
  };
  home = (import ./home.nix { inherit self; }).flake.homeModules.spool {
    inherit config lib pkgs;
  };
  darwin = (import ./darwin.nix { inherit self lib; }).flake.darwinModules.spool {
    inherit config pkgs;
  };
  expected = [ (lib.getExe package) "service" "run" ];
in
assert home.config.content.launchd.agents.spool.config.ProgramArguments == expected;
assert darwin.config.content.launchd.user.agents.spool.serviceConfig.ProgramArguments == expected;
{
  home = home.config.content.launchd.agents.spool.config.ProgramArguments;
  darwin = darwin.config.content.launchd.user.agents.spool.serviceConfig.ProgramArguments;
}
