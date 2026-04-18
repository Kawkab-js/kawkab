use std::env;
use std::path::{Path, PathBuf};

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").ok().as_deref() != Some("linux") {
        return;
    }

    if env::var("KAWKAB_BUILD_LOCAL_QUICKJS").ok().as_deref() != Some("1") {
        return;
    }

    let Some(quickjs_dir) = locate_quickjs_source() else {
        println!(
            "cargo:warning=quickjs sources not found in Cargo registry; linker may fail to resolve -lquickjs"
        );
        return;
    };

    let mut build = cc::Build::new();
    build
        .include(&quickjs_dir)
        .file(quickjs_dir.join("quickjs.c"))
        .file(quickjs_dir.join("libbf.c"))
        .file(quickjs_dir.join("libregexp.c"))
        .file(quickjs_dir.join("libunicode.c"))
        .file(quickjs_dir.join("cutils.c"))
        .file(quickjs_dir.join("quickjs-libc.c"))
        .define("CONFIG_VERSION", "\"2019-07-09\"")
        .define("_GNU_SOURCE", None)
        .flag_if_supported("-Wno-sign-compare")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-old-style-declaration")
        .flag_if_supported("-Wno-cast-function-type")
        .flag_if_supported("-Wno-implicit-fallthrough")
        .flag_if_supported("-Wno-array-bounds")
        .flag_if_supported("-Wno-unused-result")
        .flag_if_supported("-Wno-format-truncation")
        .flag_if_supported("-Wno-extra");

    build.compile("quickjs");
}

fn locate_quickjs_source() -> Option<PathBuf> {
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(default_cargo_home)?;
    let src_root = cargo_home.join("registry").join("src");
    let entries = std::fs::read_dir(src_root).ok()?;

    for entry in entries.flatten() {
        let p = entry
            .path()
            .join("quickjs-sys-0.1.0")
            .join("quickjs-2019-07-09");
        if p.join("quickjs.c").exists() {
            return Some(p);
        }
    }
    None
}

fn default_cargo_home() -> Option<PathBuf> {
    let home = env::var_os("HOME").map(PathBuf::from)?;
    let path = Path::new(&home).join(".cargo");
    Some(path)
}
