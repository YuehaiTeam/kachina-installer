use std::collections::{BTreeMap, BTreeSet};
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

    let locales = manifest.join("../locales");
    println!("cargo:rerun-if-changed={}", locales.display());

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let mut arms = Vec::new();

    if html.is_file() {
        let zst_path = out_dir.join("index.html.zst");
        zstd_file(&html, &zst_path);
        let abs = zst_path
            .canonicalize()
            .unwrap_or(zst_path)
            .to_string_lossy()
            .replace('\\', "/");
        arms.push(format!(
            "        \"index.html\" => Some((include_bytes!(r\"{abs}\"), \"text/html; charset=utf-8\")),"
        ));
    }

    let i18n_zst = out_dir.join("i18n.tsv.zst");
    merge_locales(&locales, &i18n_zst);
    let abs = i18n_zst
        .canonicalize()
        .unwrap_or_else(|_| i18n_zst.clone())
        .to_string_lossy()
        .replace('\\', "/");
    arms.push(format!(
        "        \"i18n.tsv\" => Some((include_bytes!(r\"{abs}\"), \"text/tab-separated-values; charset=utf-8\")),"
    ));

    let mut code =
        String::from("pub fn get(path: &str) -> Option<(&'static [u8], &'static str)> {\n    match path {\n");
    for arm in &arms {
        code.push_str(arm);
        code.push('\n');
    }
    code.push_str("        _ => None,\n    }\n}\n");
    fs::write(out_dir.join("ui_assets.rs"), code).unwrap();
}

fn merge_locales(locales: &Path, dst_zst: &Path) {
    let mut langs: Vec<(String, BTreeMap<String, String>)> = Vec::new();
    if locales.is_dir() {
        let mut files: Vec<PathBuf> = fs::read_dir(locales)
            .expect("read locales/")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("tsv"))
            .collect();
        files.sort();
        for f in files {
            println!("cargo:rerun-if-changed={}", f.display());
            let lang = f
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let text = fs::read_to_string(&f).unwrap_or_else(|e| panic!("read {}: {e}", f.display()));
            let mut map = BTreeMap::new();
            for line in text.lines() {
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let mut parts = line.splitn(2, '\t');
                let key = parts.next().unwrap_or("");
                if key.is_empty() || key == "KEY" {
                    continue;
                }
                let val = parts.next().unwrap_or("");
                map.insert(key.to_string(), val.to_string());
            }
            langs.push((lang, map));
        }
    }
    let mut keys = BTreeSet::new();
    for (_, map) in &langs {
        keys.extend(map.keys().cloned());
    }
    let mut wide = String::from("KEY");
    for (lang, _) in &langs {
        wide.push('\t');
        wide.push_str(lang);
    }
    wide.push('\n');
    for key in keys {
        wide.push_str(&key);
        for (_, map) in &langs {
            wide.push('\t');
            if let Some(v) = map.get(&key) {
                wide.push_str(v);
            }
        }
        wide.push('\n');
    }
    let compressed = zstd::encode_all(wide.as_bytes(), 22).expect("zstd i18n");
    fs::write(dst_zst, compressed).expect("write i18n.tsv.zst");
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
