const SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";
const VALUE: &str = "AppsUseLightTheme";

pub fn is_dark_mode() -> windows_registry::Result<bool> {
    let hkcu = windows_registry::CURRENT_USER;
    let subkey = hkcu.options().read().open(SUBKEY)?;
    let dword: u32 = subkey.get_u32(VALUE)?;
    Ok((dword == 0).into())
}
