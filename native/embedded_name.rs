//! Shared embedded-entry names for the installer and builder.
//!
//! Internal pack slots (`\0CONFIG` and the others) are exact exceptions.
//! Everything else is a single Win32 file name: no path separators, reserved
//! characters, trailing space/period, or DOS device names. TLV names are at
//! most `u16` UTF-8 bytes; names written into `\0INDEX` are at most 255.

use std::fmt;

pub const TLV_NAME_MAX: usize = u16::MAX as usize;
pub const INDEX_NAME_MAX: usize = u8::MAX as usize;

const INTERNAL_NAMES: &[&str] = &["\0CONFIG", "\0META", "\0INDEX", "\0IMAGE", "\0THEME"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddedNameError {
    Invalid {
        name: String,
        reason: &'static str,
    },
    TooLong {
        name: String,
        bytes: usize,
        limit: usize,
    },
}

impl fmt::Display for EmbeddedNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid { name, reason } => {
                write!(f, "invalid embedded name {name:?}: {reason}")
            }
            Self::TooLong { name, bytes, limit } => {
                write!(
                    f,
                    "embedded name {name:?} is {bytes} bytes, limit is {limit}"
                )
            }
        }
    }
}

impl std::error::Error for EmbeddedNameError {}

pub fn is_internal_name(name: &str) -> bool {
    INTERNAL_NAMES.contains(&name)
}

pub fn is_embedded_name(name: &str) -> bool {
    check_embedded_name(name).is_ok()
}

pub fn check_tlv_name(name: &str) -> Result<(), EmbeddedNameError> {
    check_embedded_name(name)?;
    check_len(name, TLV_NAME_MAX)
}

pub fn check_index_name(name: &str) -> Result<(), EmbeddedNameError> {
    check_embedded_name(name)?;
    check_len(name, INDEX_NAME_MAX)
}

pub fn check_embedded_name(name: &str) -> Result<(), EmbeddedNameError> {
    if is_internal_name(name) {
        return Ok(());
    }
    if name.is_empty() {
        return invalid(name, "name is empty");
    }
    if name == "." || name == ".." {
        return invalid(name, "name is '.' or '..'");
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return invalid(name, "trailing space or period");
    }
    if name.chars().any(is_forbidden_char) {
        return invalid(
            name,
            "contains a path separator, reserved character, or control character",
        );
    }
    if is_reserved_device(name) {
        return invalid(name, "Windows device name");
    }
    Ok(())
}

fn check_len(name: &str, limit: usize) -> Result<(), EmbeddedNameError> {
    let bytes = name.len();
    if bytes > limit {
        Err(EmbeddedNameError::TooLong {
            name: name.to_string(),
            bytes,
            limit,
        })
    } else {
        Ok(())
    }
}

fn invalid(name: &str, reason: &'static str) -> Result<(), EmbeddedNameError> {
    Err(EmbeddedNameError::Invalid {
        name: name.to_string(),
        reason,
    })
}

fn is_forbidden_char(c: char) -> bool {
    matches!(
        c,
        '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\0'
    ) || ('\u{0001}'..='\u{001F}').contains(&c)
}

fn is_reserved_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    let stem = stem.to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || is_com_or_lpt(&stem)
}

fn is_com_or_lpt(stem: &str) -> bool {
    let rest = if let Some(rest) = stem.strip_prefix("COM") {
        rest
    } else if let Some(rest) = stem.strip_prefix("LPT") {
        rest
    } else {
        return false;
    };
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_names_are_exact_exceptions() {
        for name in INTERNAL_NAMES {
            assert!(is_embedded_name(name), "{name:?}");
            assert!(check_tlv_name(name).is_ok());
            assert!(check_index_name(name).is_ok());
        }
        assert!(!is_embedded_name("\0CONFIG "));
        assert!(!is_embedded_name("\0config"));
        assert!(!is_embedded_name("\0OTHER"));
    }

    #[test]
    fn allows_win32_file_names() {
        for name in [
            "Microsoft.VCRedist.2015+.x64",
            "Microsoft.VCRedist.2015+.x86",
            "说明 文件.bin",
            "a b.txt",
            "lib(1).dll",
            "deadbeef",
            "DEADBEEF",
            "0123456789abcdef0123456789abcdef",
        ] {
            assert!(check_embedded_name(name).is_ok(), "{name:?}");
        }
    }

    #[test]
    fn rejects_separators_reserved_chars_and_controls() {
        for name in [
            "",
            ".",
            "..",
            "foo.",
            "foo ",
            "a/b",
            "a\\b",
            "a:b",
            "a<b",
            "a>b",
            "a\"b",
            "a|b",
            "a?b",
            "a*b",
            "a\0b",
            "a\u{0001}b",
            "a\tb",
        ] {
            assert!(check_embedded_name(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn rejects_device_names_and_extensions() {
        for name in [
            "CON", "con", "Prn", "AUX", "NUL", "NUL.txt", "COM1", "com1.dll", "COM0", "LPT1",
            "lpt9.log",
        ] {
            assert!(check_embedded_name(name).is_err(), "{name:?}");
        }
        assert!(check_embedded_name("COM").is_ok());
        assert!(check_embedded_name("COMX").is_ok());
        assert!(check_embedded_name("LPT").is_ok());
        assert!(check_embedded_name("COM1X").is_ok());
    }

    #[test]
    fn tlv_and_index_length_limits() {
        let ok_index = "n".repeat(INDEX_NAME_MAX);
        assert!(check_index_name(&ok_index).is_ok());
        let over_index = "n".repeat(INDEX_NAME_MAX + 1);
        match check_index_name(&over_index) {
            Err(EmbeddedNameError::TooLong { bytes, limit, .. }) => {
                assert_eq!(bytes, INDEX_NAME_MAX + 1);
                assert_eq!(limit, INDEX_NAME_MAX);
            }
            other => panic!("{other:?}"),
        }
        assert!(check_tlv_name(&over_index).is_ok());

        let ok_tlv = "n".repeat(512);
        assert!(check_tlv_name(&ok_tlv).is_ok());
        let over_tlv = "n".repeat(TLV_NAME_MAX + 1);
        match check_tlv_name(&over_tlv) {
            Err(EmbeddedNameError::TooLong { bytes, limit, .. }) => {
                assert_eq!(bytes, TLV_NAME_MAX + 1);
                assert_eq!(limit, TLV_NAME_MAX);
            }
            other => panic!("{other:?}"),
        }
    }
}
