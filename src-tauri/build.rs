use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    embed_app_manifest(&manifest);
    embed_app_icon(&manifest);
    let dist = manifest.join("../dist");
    let html = dist.join("index.html");
    println!("cargo:rerun-if-changed={}", html.display());

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let mut code =
        String::from("pub fn get(path: &str) -> Option<(&'static [u8], &'static str)> {\n");
    if html.is_file() {
        let zst_path = out_dir.join("index.html.zst");
        zstd_file(&html, &zst_path);
        let abs = zst_path
            .canonicalize()
            .unwrap_or(zst_path)
            .to_string_lossy()
            .replace('\\', "/");
        code.push_str(&format!(
            "    match path {{\n        \"index.html\" => Some((include_bytes!(r\"{abs}\"), \"text/html; charset=utf-8\")),\n        _ => None,\n    }}\n"
        ));
    } else {
        code.push_str("    let _ = path;\n    None\n");
    }
    code.push_str("}\n");
    fs::write(out_dir.join("ui_assets.rs"), code).unwrap();
}

fn zstd_file(src: &Path, dst: &Path) {
    let data = fs::read(src).expect("read dist/index.html");
    let compressed = zstd::encode_all(data.as_slice(), 22).expect("zstd frontend");
    fs::write(dst, compressed).expect("write index.html.zst");
}

fn embed_app_manifest(crate_dir: &Path) {
    let app_manifest = crate_dir.join("app.manifest");
    println!("cargo:rerun-if-changed={}", app_manifest.display());
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg=/MANIFESTINPUT:{}",
        app_manifest.display()
    );
}

fn embed_app_icon(crate_dir: &Path) {
    let icon = crate_dir.join("icons/icon.ico");
    let rc = crate_dir.join("app.rc");
    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-changed={}", rc.display());
    embed_resource::compile_for(
        rc,
        ["kachina-installer", "kachina-builder"],
        embed_resource::NONE,
    )
    .manifest_optional()
    .expect("embed default exe icon");
}
