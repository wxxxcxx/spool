{ self, ... }:
{
  perSystem =
    { lib, pkgs, ... }:
    {
      packages.paneru-bar = pkgs.stdenv.mkDerivation {
        pname = "paneru-bar";
        version = "0.1.0";
        src = lib.cleanSource ../bar;

        nativeBuildInputs = [ pkgs.swift ];
        dontConfigure = true;

        buildPhase = ''
          runHook preBuild
          export CLANG_MODULE_CACHE_PATH="$TMPDIR/clang-module-cache"
          export SWIFTPM_MODULECACHE_OVERRIDE="$TMPDIR/clang-module-cache"
          swift build --disable-sandbox -c release --scratch-path .build
          runHook postBuild
        '';

        installPhase = ''
          runHook preInstall
          app="$out/Applications/PaneruBar.app"
          mkdir -p "$app/Contents/MacOS" "$out/bin"
          cp .build/release/PaneruBar "$app/Contents/MacOS/PaneruBar"
          cp Resources/Info.plist "$app/Contents/Info.plist"
          ln -s "$app/Contents/MacOS/PaneruBar" "$out/bin/paneru-bar"
          runHook postInstall
        '';

        meta = {
          description = "Native multi-display workspace bar for Paneru";
          homepage = "https://github.com/wxxxcxx/paneru";
          license = lib.licenses.mit;
          mainProgram = "paneru-bar";
          platforms = lib.platforms.darwin;
        };
      };
    };
}
