//! Session-layer i18n: user-visible strings in Chinese and English.
//!
//! Scope: only the session layer (run/plan/source/ui/commands). The native
//! fallback UI (host/native.rs, module/wv2.rs, utils/taskdialog.rs) stays
//! Chinese-only by design. Logs stay Chinese. The elevated helper process is
//! a fresh process and does not inherit the language override — it currently
//! produces no user-visible localized strings (errors cross the pipe as
//! codes and are formatted by the parent); if tr() is ever needed on the
//! child-process path, the language must be forwarded there first.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn current() -> Lang {
    match LANG.load(Ordering::Relaxed) {
        1 => Lang::En,
        _ => Lang::Zh,
    }
}

pub fn set(lang: Lang) {
    LANG.store(lang as u8, Ordering::Relaxed);
}

/// Set from a config value: "zh" | "en" | "auto" (anything else = auto).
pub fn set_from_config(v: &str) {
    match v {
        "zh" => set(Lang::Zh),
        "en" => set(Lang::En),
        _ => set_from_system(),
    }
}

/// Pick the language from the OS UI language. Called once at startup, before
/// any config is available (native fallback, WebView2 bootstrap errors).
pub fn init_from_system() {
    set_from_system();
}

fn set_from_system() {
    use windows::Win32::Globalization::GetUserDefaultUILanguage;
    let lang = unsafe { GetUserDefaultUILanguage() };
    // PRIMARYLANGID == LANG_CHINESE (0x04)
    let is_zh = (lang & 0x3ff) == 0x04;
    set(if is_zh { Lang::Zh } else { Lang::En });
}

/// Pick a string by the current language.
pub fn tr(zh: &str, en: &str) -> &str {
    match current() {
        Lang::Zh => zh,
        Lang::En => en,
    }
}

/// Like [`tr`], but interpolates `{placeholders}` in the chosen template.
#[macro_export]
macro_rules! trf {
    ($zh:expr, $en:expr, $($key:literal = $val:expr),+ $(,)?) => {{
        let mut out = $crate::session::i18n::tr($zh, $en).to_string();
        $(
            out = out.replace(concat!("{", $key, "}"), &$val.to_string());
        )+
        out
    }};
}
