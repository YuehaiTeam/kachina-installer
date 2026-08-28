use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/ui_assets.rs"));

static HTML: OnceLock<Vec<u8>> = OnceLock::new();

/// Default UI is the bundled zstd `index.html`.
/// Custom packaged HTML later replaces this same entry (`\0IMAGE`).
pub fn lookup(path: &str) -> Option<(&'static [u8], &'static str)> {
    let path = path.trim_start_matches('/');
    if path.is_empty() || path == "index.html" {
        let (compressed, mime) = get("index.html")?;
        let html = HTML.get_or_init(|| zstd::decode_all(compressed).expect("decode embedded ui"));
        return Some((html.as_slice(), mime));
    }
    None
}
