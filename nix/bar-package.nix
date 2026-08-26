{ self, ... }:
{
  perSystem =
    { lib, pkgs, ... }:
    {
      packages.spool-bar = pkgs.stdenv.mkDerivation {
        pname = "spool-bar";
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
          app="$out/Applications/SpoolBar.app"
          mkdir -p "$app/Contents/MacOS" "$out/bin"
          cp .build/release/SpoolBar "$app/Contents/MacOS/SpoolBar"
          cp Resources/Info.plist "$app/Contents/Info.plist"
          ln -s "$app/Contents/MacOS/SpoolBar" "$out/bin/spool-bar"
          runHook postInstall
        '';

        meta = {
          description = "Native multi-display workspace bar for Spool";
          homepage = "https://github.com/wxxxcxx/spool";
          license = lib.licenses.mit;
          mainProgram = "spool-bar";
          platforms = lib.platforms.darwin;
        };
      };
    };
}
