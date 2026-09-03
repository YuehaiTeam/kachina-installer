//! Locale table. The bytes are the `i18n.tsv` asset that the WebView also
//! fetches, so both renderers read one table. Only renderers (native, silent,
//! `show_error`) and the session's few user-visible filesystem names call `t`.

use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct Catalog {
    langs: Vec<String>,
    rows: BTreeMap<String, Vec<String>>,
}

impl Catalog {
    /// Parse a TSV. A first line whose first cell is `KEY` is a wide table
    /// (`KEY\tlang1\tlang2…`); otherwise each line is `KEY\t文案` for a single
    /// anonymous column (used for `locales/<lang>.tsv`).
    pub fn parse(bytes: &[u8]) -> Self {
        let text = String::from_utf8_lossy(bytes);
        let mut lines = text.lines().filter(|l| !l.is_empty() && !l.starts_with('#'));
        let Some(first) = lines.next() else {
            return Self {
                langs: Vec::new(),
                rows: BTreeMap::new(),
            };
        };
        let first_cells: Vec<&str> = first.split('\t').collect();
        let (langs, mut rows) = if first_cells.first().copied() == Some("KEY") {
            let langs = first_cells[1..].iter().map(|s| (*s).to_string()).collect();
            (langs, BTreeMap::new())
        } else {
            let mut rows = BTreeMap::new();
            insert_row(&mut rows, first_cells);
            (vec![String::new()], rows)
        };
        for line in lines {
            insert_row(&mut rows, line.split('\t').collect());
        }
        Self { langs, rows }
    }

    pub fn langs(&self) -> &[String] {
        &self.langs
    }

    pub fn has_key(&self, key: &str) -> bool {
        self.rows.contains_key(key)
    }

    /// Look up `key` in `lang`'s column (no match → first language column).
    /// Missing key returns the key. `{name}` placeholders are replaced.
    pub fn t(&self, lang: &str, key: &str, params: &[(&str, &str)]) -> String {
        let col = self
            .langs
            .iter()
            .position(|l| l == lang)
            .unwrap_or(0);
        let Some(vals) = self.rows.get(key) else {
            return key.to_string();
        };
        let text = vals
            .get(col)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| vals.first().map(String::as_str).filter(|s| !s.is_empty()))
            .unwrap_or(key);
        interpolate(text, params)
    }
}

fn insert_row(rows: &mut BTreeMap<String, Vec<String>>, cells: Vec<&str>) {
    if cells.is_empty() {
        return;
    }
    let key = cells[0].to_string();
    if key.is_empty() {
        return;
    }
    let vals = cells[1..].iter().map(|s| (*s).to_string()).collect();
    rows.insert(key, vals);
}

fn interpolate(text: &str, params: &[(&str, &str)]) -> String {
    let mut s = text.to_string();
    for (name, value) in params {
        s = s.replace(&format!("{{{name}}}"), value);
    }
    s
}


use std::sync::OnceLock;

static CATALOG: OnceLock<Catalog> = OnceLock::new();
static LANG: OnceLock<String> = OnceLock::new();

pub fn catalog() -> &'static Catalog {
    CATALOG.get_or_init(|| {
        let bytes = crate::host::assets::lookup("i18n.tsv")
            .map(|(b, _)| b)
            .unwrap_or_default();
        Catalog::parse(bytes)
    })
}

pub fn lang() -> &'static str {
    LANG.get_or_init(system_lang).as_str()
}

fn system_lang() -> String {
    let mut buf = [0u16; 85];
    let n = unsafe { windows::Win32::Globalization::GetUserDefaultLocaleName(&mut buf) };
    if n > 1 {
        let s = String::from_utf16_lossy(&buf[..n as usize - 1]).replace('_', "-");
        if !s.is_empty() {
            return s;
        }
    }
    catalog()
        .langs()
        .first()
        .cloned()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "zh-CN".into())
}

/// Look up copy. Renderers only (GuiUi / NativeUi / show paths).
pub fn t(key: &str, params: &[(&str, &str)]) -> String {
    catalog().t(lang(), key, params)
}

pub fn format_size(size: u64) -> String {
    if size >= 1024 * 1024 {
        format!("{:.1}MB", size as f64 / 1024.0 / 1024.0)
    } else if size >= 1024 {
        format!("{:.0}KB", size as f64 / 1024.0)
    } else {
        format!("{size}B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::state::{PROMPT_KEYS, STAGE_KEYS};
    use crate::utils::code::ALL_CODES;

    fn zh_cn_bytes() -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../locales/zh-CN.tsv");
        std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    #[test]
    fn locale_covers_codes_stages_prompts() {
        let cat = Catalog::parse(&zh_cn_bytes());
        let mut missing = Vec::new();
        for key in ALL_CODES.iter().copied().chain(STAGE_KEYS.iter().copied()).chain(PROMPT_KEYS.iter().copied()) {
            if !cat.has_key(key) {
                missing.push(key);
            }
        }
        assert!(
            missing.is_empty(),
            "locales/zh-CN.tsv missing keys: {missing:?}"
        );
    }

    #[test]
    fn t_picks_column_interpolates_and_falls_back() {
        let bytes = "KEY\tzh-CN\ten-US\nhello\t你好{name}\tHello {name}\nonly_zh\t仅中文\t\n".as_bytes();
        let cat = Catalog::parse(bytes);
        assert_eq!(cat.langs(), &["zh-CN".to_string(), "en-US".to_string()]);
        assert_eq!(cat.t("zh-CN", "hello", &[("name", "A")]), "你好A");
        assert_eq!(cat.t("en-US", "hello", &[("name", "A")]), "Hello A");
        assert_eq!(cat.t("fr-FR", "hello", &[("name", "A")]), "你好A");
        assert_eq!(cat.t("en-US", "only_zh", &[]), "仅中文");
        assert_eq!(cat.t("zh-CN", "nope", &[]), "nope");
    }
}
