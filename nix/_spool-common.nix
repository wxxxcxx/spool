# Shared option schema and Lua-wrapping helpers for the spool Home Manager and
# nix-darwin modules, which otherwise duplicate this verbatim and differ only in
# their platform-specific `config` block.
#
# Underscore-prefixed so flake.nix's `imports = [ (import-tree ./nix) ]` skips
# it: this is a plain module fragment imported by home.nix / darwin.nix, not a
# flake-parts module. Applied as `import ./_spool-common.nix { inherit self; }`.
{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.spool;
  tomlFormat = pkgs.formats.toml { };

  luaPackages = cfg.package.luaModule.lua.pkgs;
  resolvedExtraLuaPackages = if cfg.luaConfig.enable then cfg.extraLuaPackages luaPackages else [ ];
  luaPaths = lib.optional (resolvedExtraLuaPackages != [ ]) (
    lib.concatMapStringsSep ";" luaPackages.getLuaPath resolvedExtraLuaPackages
  );
  luaCPaths = lib.optional (resolvedExtraLuaPackages != [ ]) (
    lib.concatMapStringsSep ";" luaPackages.getLuaCPath resolvedExtraLuaPackages
  );
  makeWrapperArgs = lib.flatten (
    lib.filter (x: x != [ ]) [
      (lib.optional (cfg.extraPackages != [ ]) [
        "--prefix"
        "PATH"
        ":"
        "${lib.makeBinPath cfg.extraPackages}"
      ])

      (lib.optional (luaPaths != [ ]) [
        "--prefix"
        "LUA_PATH"
        ";"
        "${lib.concatStringsSep ";" luaPaths}"
      ])

      (lib.optional (luaCPaths != [ ]) [
        "--prefix"
        "LUA_CPATH"
        ";"
        "${lib.concatStringsSep ";" luaCPaths}"
      ])
    ]
  );
  wrapSpool =
    package:
    pkgs.symlinkJoin {
      name = "spool-with-lua-wrapped";
      paths = [ package ];
      nativeBuildInputs = [ pkgs.makeWrapper ];
      passthru = package.passthru;
      postBuild = ''
        wrapProgram $out/bin/spool ${lib.escapeShellArgs makeWrapperArgs}
      '';
      inherit (cfg.package) meta;
    };
in
{
  options.services.spool = {
    enable = lib.mkEnableOption ''
      Install spool and configure the launchd agent.

      The first time this is enabled after installing/updating, macOS will prompt you
      to grant accessibilty permissions item in System Settings.

      After granting permissions you may have to manually restart the service:
      `launchctl start com.wxxxcxx.spool`

      You can verify the service is running correctly from your terminal.
      Run: `launchctl list | grep spool`

      In case of failure, check the logs with `cat /tmp/spool.err.log`.
    '';

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      description = "The spool package to use.";
    };

    extraPackages = lib.mkOption {
      type = with lib.types; listOf package;
      default = [ ];
      example = lib.literalExpression "[ pkgs.sketchybar ]";
      description = ''
        Extra packages to add to the PATH when spool is run. This is
        useful for adding dependencies that spool's Lua scripts may
        require, such as `sketchybar` for `require("sbar")`.
      '';
    };

    finalPackage = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      default =
        if cfg.luaConfig.enable then
          wrapSpool (
            cfg.package.override {
              enableLua = true;
            }
          )
        else
          cfg.package.override { enableLua = false; };
      description = ''
        The final spool package that will be installed and run. This is
        the result of `package.override { enableLua = ...; }` (see
        `luaConfig.enable`), so it may differ from `package` when that option
        is set.
      '';
    };

    luaConfig = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Whether `services.spool.package` is built with the embedded Lua
          scripting runtime (`init.lua`, `spool.on`/`spool.bind`,
          `spool.setup`) and its vendored LuaJIT compiled in — the complete
          `lua` Cargo feature. Disable for a build with no Lua dependency. Only takes effect when
          `package` is left at its default (an overrideable `spool.override
          { enableLua = ...; }` derivation); implies `extraLuaPackages` and
          `config` are ignored when `false`.
        '';
      };
    };

    settings = lib.mkOption {
      type = lib.types.nullOr lib.types.attrs;
      default = null;
      description = ''
        Spool configuration, rendered to a `spool.toml`. Ignored for the
        options a `config` (`init.lua`) `spool.setup{...}` call declares,
        which take precedence — see the spool configuration guide.
      '';
      example = {
        options = {
          focus_follows_mouse = true;
          mouse_follows_focus = true;
        };
        bindings = {
          window_focus_west = "cmd+h";
          window_focus_east = "cmd+l";
          window_grow_width = "alt+equal";
          window_center = "alt+c";
          quit = "ctrl+alt+q";
        };
      };
    };

    config = lib.mkOption {
      type = with lib.types; nullOr (either lines path);
      default = null;
      example = ''
        spool.setup {
          options = { focus_follows_mouse = true },
        }
        spool.bind("alt+l", spool.action.window.focus_east)
      '';
      description = ''
        Contents of spool's `init.lua` — either a block of Lua source or a
        path to a file — written to `$XDG_CONFIG_HOME/spool/init.lua` (or
        `~/.spool.lua` when XDG is disabled). Mirrors Home Manager's
        `services.sketchybar.config`.

        Requires `luaConfig.enable = true`. When the script calls
        `spool.setup{...}` it becomes the authoritative configuration and the
        TOML `settings` are ignored; otherwise the two coexist (see the spool
        configuration guide). Unlike sketchybar's config it is not marked
        executable — spool loads it, it is not run as a shell script.
      '';
    };

    extraLuaPackages = lib.mkOption {
      type = with lib.types; functionTo (listOf package);
      default = _: [ ];
      defaultText = lib.literalExpression "luaPs: [ ]";
      example = lib.literalExpression "luaPs: [ (luaPs.callPackage ./sbarlua.nix { }) ]";
      description = ''
        Extra Lua packages made available to spool's embedded Lua runtime
        via `require(...)` (e.g. `require("sbar")` to call SketchyBar's
        Lua bridge directly from an `init.lua` `spool.on(...)` handler).
        This option accepts a function that takes a Lua package set and
        returns the packages to expose; it is deliberately the same shape
        as Home Manager's `programs.sketchybar.extraLuaPackages`, so the
        same package (e.g. `sbarlua`) can be passed to both options
        without duplicating the derivation.
      '';
    };

    # Computed, read-only: the generated config files the platform modules
    # write out (or reference via env vars), so neither has to repeat the
    # path-or-lines / TOML-generation logic.
    configFile = lib.mkOption {
      type = with lib.types; nullOr path;
      readOnly = true;
      default =
        if cfg.config == null then
          null
        else if lib.isPath cfg.config || lib.isStorePath cfg.config then
          cfg.config
        else
          pkgs.writeText "init.lua" cfg.config;
      description = "The generated `init.lua` file (from `config`), or `null`.";
    };

    settingsFile = lib.mkOption {
      type = with lib.types; nullOr path;
      readOnly = true;
      default = if cfg.settings == null then null else tomlFormat.generate "spool.toml" cfg.settings;
      description = "The generated `spool.toml` file (from `settings`), or `null`.";
    };
  };
}
