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
        let mut lines = text
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'));
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

    /// Map a BCP 47 tag to one of the table's columns: exact match, then the
    /// first column sharing the primary language (`zh-TW` → `zh-CN`), then the
    /// first column. Returns `None` for an empty table.
    pub fn resolve_lang(&self, requested: &str) -> Option<&str> {
        if let Some(l) = self.langs.iter().find(|l| l.eq_ignore_ascii_case(requested)) {
            return Some(l.as_str());
        }
        let primary = requested.split('-').next().unwrap_or("");
        if !primary.is_empty() {
            if let Some(l) = self.langs.iter().find(|l| {
                l.split('-')
                    .next()
                    .is_some_and(|p| p.eq_ignore_ascii_case(primary))
            }) {
                return Some(l.as_str());
            }
        }
        self.langs.first().map(String::as_str).filter(|s| !s.is_empty())
    }

    /// Look up `key` in `lang`'s column (no match → first language column).
    /// Missing key returns the key. `{name}` placeholders are replaced.
    pub fn t(&self, lang: &str, key: &str, params: &[(&str, &str)]) -> String {
        let col = self.langs.iter().position(|l| l == lang).unwrap_or(0);
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
    let requested = if n > 1 {
        String::from_utf16_lossy(&buf[..n as usize - 1]).replace('_', "-")
    } else {
        String::new()
    };
    catalog()
        .resolve_lang(&requested)
        .map(str::to_string)
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

    /// Every `locales/*.tsv`, parsed, keyed by file stem.
    fn locale_files() -> Vec<(String, Catalog)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        let mut out: Vec<(String, Catalog)> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("tsv"))
            .map(|p| {
                let bytes =
                    std::fs::read(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
                let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
                (stem, Catalog::parse(&bytes))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        assert!(
            out.iter().any(|(l, _)| l == "zh-CN"),
            "locales/zh-CN.tsv is the reference table"
        );
        out
    }

    #[test]
    fn locale_covers_codes_stages_prompts() {
        for (lang, cat) in locale_files() {
            let mut missing = Vec::new();
            for key in ALL_CODES
                .iter()
                .copied()
                .chain(STAGE_KEYS.iter().copied())
                .chain(PROMPT_KEYS.iter().copied())
            {
                if !cat.has_key(key) {
                    missing.push(key);
                }
            }
            assert!(
                missing.is_empty(),
                "locales/{lang}.tsv missing keys: {missing:?}"
            );
        }
    }

    #[test]
    fn every_locale_has_the_same_keys_as_zh_cn() {
        let files = locale_files();
        let reference = &files.iter().find(|(l, _)| l == "zh-CN").unwrap().1;
        for (lang, cat) in &files {
            let missing: Vec<&String> = reference
                .rows
                .keys()
                .filter(|k| !cat.has_key(k))
                .collect();
            let extra: Vec<&String> = cat
                .rows
                .keys()
                .filter(|k| !reference.has_key(k))
                .collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "locales/{lang}.tsv: missing {missing:?}, extra {extra:?}"
            );
        }
    }

    #[test]
    fn resolve_lang_exact_then_primary_then_first() {
        let cat = Catalog::parse("KEY\ten-US\tzh-CN\nk\ta\tb\n".as_bytes());
        assert_eq!(cat.resolve_lang("zh-CN"), Some("zh-CN"));
        assert_eq!(cat.resolve_lang("zh-cn"), Some("zh-CN"));
        assert_eq!(cat.resolve_lang("zh-TW"), Some("zh-CN"));
        assert_eq!(cat.resolve_lang("en-GB"), Some("en-US"));
        assert_eq!(cat.resolve_lang("ja-JP"), Some("en-US"));
        assert_eq!(cat.resolve_lang(""), Some("en-US"));
        assert_eq!(Catalog::parse(b"").resolve_lang("zh-CN"), None);
    }

    #[test]
    fn t_picks_column_interpolates_and_falls_back() {
        let bytes =
            "KEY\tzh-CN\ten-US\nhello\t你好{name}\tHello {name}\nonly_zh\t仅中文\t\n".as_bytes();
        let cat = Catalog::parse(bytes);
        assert_eq!(cat.langs(), &["zh-CN".to_string(), "en-US".to_string()]);
        assert_eq!(cat.t("zh-CN", "hello", &[("name", "A")]), "你好A");
        assert_eq!(cat.t("en-US", "hello", &[("name", "A")]), "Hello A");
        assert_eq!(cat.t("fr-FR", "hello", &[("name", "A")]), "你好A");
        assert_eq!(cat.t("en-US", "only_zh", &[]), "仅中文");
        assert_eq!(cat.t("zh-CN", "nope", &[]), "nope");
    }
}
