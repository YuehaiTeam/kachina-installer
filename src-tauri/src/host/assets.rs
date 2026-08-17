include!(concat!(env!("OUT_DIR"), "/ui_assets.rs"));

pub fn lookup(path: &str) -> Option<(&'static [u8], &'static str)> {
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        return get("index.html");
    }
    get(path).or_else(|| get(&format!("{path}/index.html")))
}
