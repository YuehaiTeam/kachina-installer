use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    embed_app_manifest(&manifest);
    let dist = manifest.join("../dist");
    println!("cargo:rerun-if-changed={}", dist.display());

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    if dist.is_dir() {
        collect_files(&dist, &dist, &mut files);
    }

    let mut code = String::from(
        "pub fn get(path: &str) -> Option<(&'static [u8], &'static str)> {\n    match path {\n",
    );
    for (url_path, file_path) in &files {
        let abs = file_path
            .canonicalize()
            .unwrap_or_else(|_| file_path.clone());
        let abs = abs.to_string_lossy().replace('\\', "/");
        let mime = mime_for(url_path);
        code.push_str(&format!(
            "        {url_path:?} => Some((include_bytes!(r\"{abs}\"), {mime:?})),\n"
        ));
    }
    code.push_str("        _ => None,\n    }\n}\n");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("ui_assets.rs");
    fs::write(out, code).unwrap();
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

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out);
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        out.push((rel, path));
    }
}

fn mime_for(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}
