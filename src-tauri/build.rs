use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::write::GzEncoder;
use flate2::Compression;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    embed_app_manifest(&manifest);
    let dist = manifest.join("../dist");
    let html = dist.join("index.html");
    println!("cargo:rerun-if-changed={}", html.display());

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let mut code =
        String::from("pub fn get(path: &str) -> Option<(&'static [u8], &'static str, bool)> {\n");
    if html.is_file() {
        let gz_path = out_dir.join("index.html.gz");
        gzip_file(&html, &gz_path);
        let abs = gz_path
            .canonicalize()
            .unwrap_or(gz_path)
            .to_string_lossy()
            .replace('\\', "/");
        code.push_str(&format!(
            "    match path {{\n        \"index.html\" => Some((include_bytes!(r\"{abs}\"), \"text/html; charset=utf-8\", true)),\n        _ => None,\n    }}\n"
        ));
    } else {
        code.push_str("    let _ = path;\n    None\n");
    }
    code.push_str("}\n");
    fs::write(out_dir.join("ui_assets.rs"), code).unwrap();
}

fn gzip_file(src: &Path, dst: &Path) {
    let data = fs::read(src).expect("read dist/index.html");
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(&data).expect("gzip frontend");
    fs::write(dst, encoder.finish().expect("gzip finish")).expect("write index.html.gz");
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
