{ self, inputs, ... }:
{
  perSystem =
    {
      lib,
      pkgs,
      self',
      config,
      ...
    }:
    let
      craneLib = inputs.crane.mkLib pkgs;

      mkDate =
        longDate:
        (lib.concatStringsSep "-" [
          (builtins.substring 0 4 longDate)
          (builtins.substring 4 2 longDate)
          (builtins.substring 6 2 longDate)
        ]);
      props = builtins.fromTOML (builtins.readFile ../Cargo.toml);
      pname = "spool";
      version = props.package.version;

      # One source tree for every derivation here: the daemon and the loadable
      # Lua module are members of the same workspace sharing one lock file, so
      # they vendor identical dependencies. `commonCargoSources` keeps only
      # Rust/Cargo files, so the embedded plists and default Lua script
      # are added back explicitly.
      spoolSource = lib.fileset.toSource {
        root = ../.;
        fileset = lib.fileset.unions [
          (craneLib.fileset.commonCargoSources ../.)
          (lib.fileset.fileFilter (file: file.hasExt "plist") ../.)
          ../config/default.lua
        ];
      };

      # `luajit-src` copies its source tree into OUT_DIR and then overwrites
      # `.relver`. Nix store files are read-only and the recursive copy keeps
      # that mode, so remove the copied file before recreating it. Patch just
      # this crate while vendoring instead of copying the entire dependency
      # tree to a writable directory for every build.
      cargoVendorDir = craneLib.vendorCargoDeps {
        src = spoolSource;
        overrideVendorCargoPackage =
          package: drv:
          if package.name == "luajit-src" then
            drv.overrideAttrs (_old: {
              patches = [ ./luajit-src-recreate-relver.patch ];
            })
          else
            drv;
      };

      # Arguments common to everything built from this workspace.
      commonArgs = {
        inherit version cargoVendorDir;
        src = spoolSource;
        # The daemon links AppKit/etc.; nothing here runs tests as part of the
        # build.
        doCheck = false;
      };

      # Spool has one Lua implementation. The daemon embeds its own vendored
      # LuaJIT; this package supplies headers for the independently loadable C
      # module and the package set used by `extraLuaPackages`.
      lua = pkgs.luajit;

      # Third-party dependencies, compiled once and reused by every derivation
      # here instead of being rebuilt per source change.
      #
      # Built with the widest feature set once. A non-Lua build simply ignores
      # the Lua artifacts in this shared dependency derivation.
      sharedDeps = craneLib.buildDepsOnly (
        commonArgs
        // {
          inherit pname;
          cargoExtraArgs = "--no-default-features --features lua";
          buildInputs = [ pkgs.apple-sdk.privateFrameworksHook ];
        }
      );

      # Overrideable like a nixpkgs package (`spool.override { enableLua =
      # false; }`). `enableLua` is the only Lua build choice: enabled means the
      # complete vendored LuaJIT capability, disabled means no Lua dependency.
      mkSpool =
        {
          enableLua ? true,
        }:
        let
          cargoExtraArgs = "--no-default-features" + lib.optionalString enableLua " --features lua";

          # --- Loadable LuaJIT C module ---------------------------------------
          #
          # Unlike the daemon it embeds no Lua, resolving `lua_*` from its
          # LuaJIT host at load time. The package only supplies headers here.
          luaModule =
            let
              ver = lua.luaversion;
              drv = craneLib.buildPackage (
                commonArgs
                // {
                  inherit version;
                  pname = "lua${ver}-spool";
                  # The module is a workspace member, just not a default one
                  # (see the root Cargo.toml), so it builds from the same
                  # source and dependency artifacts as the daemon.
                  cargoArtifacts = sharedDeps;
                  nativeBuildInputs = [ pkgs.pkg-config ];
                  buildInputs = [ lua ];
                  cargoExtraArgs = "-p spool-lua --no-default-features --features module";
                  # Install the cdylib as `spool.so` under the interpreter's
                  # C-module path, so a `${moduleDir}/?.so` cpath entry finds it.
                  installPhaseCommand = ''
                    so=$(find target -name 'libspool_lua.dylib' -print -quit)
                    if [ -z "$so" ]; then
                      echo "spool-lua: could not find built cdylib" >&2
                      exit 1
                    fi
                    install -Dm555 "$so" "$out/lib/lua/${ver}/spool.so"
                  '';
                  meta = {
                    description = "Loadable Lua module for the Spool window manager";
                    platforms = lib.platforms.darwin;
                  };
                }
              );
            in
            drv.overrideAttrs (old: {
              passthru = (old.passthru or { }) // {
                inherit lua;
                moduleDir = "${drv}/lib/lua/${ver}";
                modulePath = "${drv}/lib/lua/${ver}/spool.so";
              };
            });
        in
        craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoExtraArgs;
            pname = "spool${if enableLua then "-with-lua" else ""}";
            cargoArtifacts = sharedDeps;

            # Expose the loadable Lua module so downstream configs can
            # reference it as `spool.luaModule`, `spool.luaModule.moduleDir`
            # (the dir for a `package.cpath` `?.so` entry), or
            # `spool.luaModule.modulePath` (the `spool.so` file).
            passthru.luaModule = luaModule;

            meta = {
              # Tells `lib.getExe` which package name to get.
              mainProgram = pname;
              platforms = lib.platforms.darwin;
            };
          }
        );
    in
    {
      devShells.default = craneLib.devShell {
        inputsFrom = [
          self'.packages.spool
          self'.packages.spool.luaModule
        ];
        packages = [
          pkgs.rustc
          pkgs.cargo
          pkgs.rustfmt
          pkgs.cargo-flamegraph
          pkgs.clippy
          pkgs.pkg-config
        ];
      };

      packages.default = self'.packages.spool;

      packages.spool = lib.makeOverridable mkSpool { };
      packages.spool-lua = self'.packages.spool.override { enableLua = true; };

      checks.package-contract =
        let
          variants = [
            self'.packages.default
            (self'.packages.spool.override { enableLua = false; })
          ];
          overlayPackage = (self.overlays.default pkgs pkgs).spool;
        in
        assert lib.assertMsg (
          builtins.all (
            package:
            lib.getExe package == "${package}/bin/spool"
            && package.system == pkgs.stdenv.hostPlatform.system
          ) variants
        ) "Spool package variants must expose bin/spool for the selected host architecture";
        assert lib.assertMsg (
          overlayPackage.system == pkgs.stdenv.hostPlatform.system
        ) "Spool's overlay must select the host architecture";
        pkgs.runCommand "spool-package-contract" { } "touch $out";
    };
}
