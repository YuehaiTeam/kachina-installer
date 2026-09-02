use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/ui_assets.rs"));

static HTML: OnceLock<Vec<u8>> = OnceLock::new();
static I18N: OnceLock<Vec<u8>> = OnceLock::new();
static HTML_OVERRIDE: OnceLock<Vec<u8>> = OnceLock::new();
static THEME_WEBP: OnceLock<Vec<u8>> = OnceLock::new();
static THEME_CSS: OnceLock<Vec<u8>> = OnceLock::new();

pub fn set_html_override(bytes: Vec<u8>) {
    let _ = HTML_OVERRIDE.set(bytes);
}

pub fn set_theme_webp(bytes: Vec<u8>) {
    let _ = THEME_WEBP.set(bytes);
}

pub fn set_theme_css(bytes: Vec<u8>) {
    let _ = THEME_CSS.set(bytes);
}

/// Default UI is the bundled zstd `index.html`.
/// Custom packaged HTML later replaces this same entry (`\0IMAGE`).
pub fn lookup(path: &str) -> Option<(&'static [u8], &'static str)> {
    let path = path.trim_start_matches('/');
    if path.is_empty() || path == "index.html" {
        if let Some(html) = HTML_OVERRIDE.get() {
            return Some((html.as_slice(), "text/html; charset=utf-8"));
        }
        let (compressed, mime) = get("index.html")?;
        let html = HTML.get_or_init(|| zstd::decode_all(compressed).expect("decode embedded ui"));
        return Some((html.as_slice(), mime));
    }
    if path == "i18n.tsv" {
        let (compressed, mime) = get("i18n.tsv")?;
        let bytes = I18N.get_or_init(|| zstd::decode_all(compressed).expect("decode embedded i18n"));
        return Some((bytes.as_slice(), mime));
    }
    if path == "theme.webp" {
        return THEME_WEBP.get().map(|b| (b.as_slice(), "image/webp"));
    }
    if path == "theme.css" {
        return THEME_CSS
            .get()
            .map(|b| (b.as_slice(), "text/css; charset=utf-8"));
    }
    None
}
