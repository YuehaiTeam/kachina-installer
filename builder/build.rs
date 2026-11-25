use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    if env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("..");
    embed_app_manifest(&root);
    embed_app_resources(&root);
}

fn embed_app_manifest(crate_dir: &Path) {
    let app_manifest = crate_dir.join("resources/app.manifest");
    println!("cargo:rerun-if-changed={}", app_manifest.display());
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg=/MANIFESTINPUT:{}",
        app_manifest.display()
    );
}

/// 与安装器一致的图标与版本信息；rc 资源文件在仓库根部 resources/ 下共享。
fn embed_app_resources(crate_dir: &Path) {
    let icon = crate_dir.join("resources/icons/icon.ico");
    let rc = crate_dir.join("resources/app.rc");
    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-changed={}", rc.display());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let version = |key: &str| env::var(key).unwrap();
    fs::write(
        out_dir.join("app_version.h"),
        format!(
            "#define APP_VERSION_BIN {},{},{},0\n#define APP_VERSION_STR \"{}\"\n",
            version("CARGO_PKG_VERSION_MAJOR"),
            version("CARGO_PKG_VERSION_MINOR"),
            version("CARGO_PKG_VERSION_PATCH"),
            version("CARGO_PKG_VERSION"),
        ),
    )
    .expect("write app_version.h");
    embed_resource::compile_for(rc, ["kachina-builder"], embed_resource::NONE)
        .manifest_optional()
        .expect("embed default exe icon");
}
