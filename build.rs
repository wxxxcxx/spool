use std::fs;
use std::path::Path;

const DEFAULT_SDK: &str = "MacOSX.sdk";

fn main() {
    let sdk_dir = std::env::var("DEVELOPER_DIR").map_or_else(
        |_| "/Library/Developer/CommandLineTools/SDKs".to_string(),
        |x| format!("{x}/Platforms/MacOSX.platform/Developer/SDKs"),
    );

    let sdk_bases: Vec<String> = std::iter::once(format!("{sdk_dir}/{DEFAULT_SDK}"))
        .chain(
            fs::read_dir(&sdk_dir)
                .expect("Failed to read SDK directory")
                .flatten()
                .filter_map(|entry| entry.file_name().to_str().map(String::from))
                // macOS's default filesystem is case-insensitive, so the on-disk
                // spelling of the extension is not guaranteed to be lowercase.
                .filter(|name| {
                    name.starts_with("MacOSX")
                        && Path::new(name)
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("sdk"))
                        && name != DEFAULT_SDK
                })
                .map(|name| format!("{sdk_dir}/{name}")),
        )
        .collect();

    for base in &sdk_bases {
        let private = format!("{base}/System/Library/PrivateFrameworks");
        let hit =
            format!("{base}/System/Library/Frameworks/Carbon.framework/Versions/A/Frameworks");

        if Path::new(&private).exists() {
            println!("cargo:rustc-link-search=framework={private}");
        }
        if Path::new(&hit).exists() {
            println!("cargo:rustc-link-search=framework={hit}");
        }
    }

    if cfg!(feature = "lua") {
        println!("cargo:rustc-env=SPOOL_LUA_VERSION=LuaJIT");
    }
}
