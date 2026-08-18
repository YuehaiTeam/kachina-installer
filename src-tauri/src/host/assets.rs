include!(concat!(env!("OUT_DIR"), "/ui_assets.rs"));

/// Default UI is the bundled gzipped `index.html`.
/// Custom packaged HTML later replaces this same entry (`\0IMAGE`).
pub fn lookup(path: &str) -> Option<(&'static [u8], &'static str, bool)> {
    let path = path.trim_start_matches('/');
    if path.is_empty() || path == "index.html" {
        return get("index.html");
    }
    None
}
