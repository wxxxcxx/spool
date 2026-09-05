{
  lib,
  inputs,
  self,
  ...
}:
{
  perSystem =
    { pkgs, system, ... }:
    let
      buildFromConfig =
        configuration: sel:
        sel
          (import inputs.nix-darwin {
            inherit configuration system;
            nixpkgs = inputs.nixpkgs;
          }).config;

      makeTest =
        name: test:
        let
          configuration =
            {
              config,
              lib,
              pkgs,
              ...
            }:
            {
              imports = [
                self.darwinModules.spool
                test
              ];

              options = {
                out = lib.mkOption {
                  type = lib.types.package;
                };
                test = lib.mkOption {
                  type = lib.types.lines;
                  default = "";
                };
              };

              config = {
                out = config.system.build.toplevel;
                system.stateVersion = lib.mkDefault config.system.maxStateVersion;
                system.build.run-test =
                  pkgs.runCommand "darwin-test-${name}"
                    {
                      allowSubstitutes = false;
                      preferLocalBuild = true;
                    }
                    ''
                      #! ${pkgs.stdenv.shell}
                      set -e

                      echo >&2 "running tests for system ${config.out}"
                      echo >&2
                      ${config.test}
                      echo >&2 ok
                      touch $out
                    '';
              };
            };
        in
        buildFromConfig configuration (config: config.system.build.run-test);
    in
    {
      checks.darwin-module = makeTest "darwin-module" (
        { config, pkgs, ... }:
        let
          plistPath = "${config.out}/user/Library/LaunchAgents/com.wxxxcxx.spool.plist";
        in
        {
          system.primaryUser = "test-spool-user";
          services.spool = {
            enable = true;
            config = ''
              spool.setup {
                options = { focus_follows_mouse = true },
                bar = { show_workspace_labels = false },
              }
            '';
          };

          test = # sh
            ''
              PATH=${
                lib.makeBinPath [
                  pkgs.jq
                  pkgs.xcbuild
                ]
              }:$PATH

              echo >&2 "checking spool service in ~/Library/LaunchAgents"
              plutil -lint ${plistPath}
              plutil -convert json ${plistPath} -o service.json
              <service.json jq -e ".EnvironmentVariables.NO_COLOR == \"1\""
              <service.json jq -e ".KeepAlive.Crashed == true"
              <service.json jq -e ".KeepAlive.SuccessfulExit == false"
              <service.json jq -e ".Label == \"com.wxxxcxx.spool\""
              <service.json jq -e 'has("MachServices") | not'
              <service.json jq -e ".ProcessType == \"Interactive\""
              <service.json jq -e ".RunAtLoad == true"
              <service.json jq -e ".StandardErrorPath == \"/tmp/spool.err.log\""
              <service.json jq -e ".StandardOutPath == \"/tmp/spool.log\""

              <service.json jq -e '.EnvironmentVariables | has("SPOOL_CONFIG") | not'

              luaPath=`<service.json jq -r ".EnvironmentVariables.SPOOL_LUA"`
              echo >&2 "checking init.lua in $luaPath"
              grep -q "spool.setup" "$luaPath"
              grep -q "show_workspace_labels = false" "$luaPath"
            '';
        }
      );
    };
}
