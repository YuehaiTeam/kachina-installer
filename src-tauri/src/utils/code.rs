//! Error codes hung on anyhow chains. Display is the code only; user-facing
//! copy lives in the locale table. No constructor takes a message string.

use std::fmt;

use serde::Serialize;

// --- N: download itself, do not report ---
pub const DOWNLOAD_TIMEOUT: &str = "DOWNLOAD_TIMEOUT";
pub const DOWNLOAD_REFUSED: &str = "DOWNLOAD_REFUSED";
pub const DOWNLOAD_FAILED: &str = "DOWNLOAD_FAILED";
pub const DOWNLOAD_STALLED: &str = "DOWNLOAD_STALLED";
pub const SERVER_HTTP_ERROR: &str = "SERVER_HTTP_ERROR";
pub const HASH_MISMATCH: &str = "HASH_MISMATCH";
pub const SOURCE_NEEDS_VERIFICATION: &str = "SOURCE_NEEDS_VERIFICATION";

// --- E: machine environment, do not report ---
pub const PERMISSION_DENIED: &str = "PERMISSION_DENIED";
pub const DISK_FULL: &str = "DISK_FULL";
pub const FILE_IN_USE: &str = "FILE_IN_USE";
pub const FILE_IO_FAILED: &str = "FILE_IO_FAILED";
pub const TEMP_DIR_UNAVAILABLE: &str = "TEMP_DIR_UNAVAILABLE";
pub const PROCESS_KILL_FAILED: &str = "PROCESS_KILL_FAILED";
pub const REGISTRY_WRITE_FAILED: &str = "REGISTRY_WRITE_FAILED";
pub const SHORTCUT_FAILED: &str = "SHORTCUT_FAILED";
pub const ELEVATE_FAILED: &str = "ELEVATE_FAILED";
pub const RUNTIME_INSTALL_FAILED: &str = "RUNTIME_INSTALL_FAILED";
pub const WEBVIEW2_REQUIRED: &str = "WEBVIEW2_REQUIRED";
pub const WEBVIEW2_FAILED: &str = "WEBVIEW2_FAILED";
pub const SELF_UPDATE_FAILED: &str = "SELF_UPDATE_FAILED";

// --- U: user input, do not report ---
pub const MIRRORC_CDK_MISSING: &str = "MIRRORC_CDK_MISSING";
pub const MIRRORC_CDK_EXPIRED: &str = "MIRRORC_CDK_EXPIRED";
pub const MIRRORC_CDK_INVALID: &str = "MIRRORC_CDK_INVALID";
pub const MIRRORC_CDK_MISMATCH: &str = "MIRRORC_CDK_MISMATCH";
pub const MIRRORC_CDK_QUOTA_EXCEEDED: &str = "MIRRORC_CDK_QUOTA_EXCEEDED";
pub const MIRRORC_CDK_BANNED: &str = "MIRRORC_CDK_BANNED";
pub const INSTALL_PATH_INVALID: &str = "INSTALL_PATH_INVALID";
pub const PLUGIN_FAILED: &str = "PLUGIN_FAILED";

// --- C: packager config, report ---
pub const PKG_BROKEN: &str = "PKG_BROKEN";
pub const SOURCE_INVALID: &str = "SOURCE_INVALID";
pub const VERSION_REGEX_INVALID: &str = "VERSION_REGEX_INVALID";
pub const MIRRORC_CONFIG_INVALID: &str = "MIRRORC_CONFIG_INVALID";
pub const PLUGIN_NO_UI: &str = "PLUGIN_NO_UI";
pub const PLUGIN_NOT_FOUND: &str = "PLUGIN_NOT_FOUND";
pub const RUNTIME_UNSUPPORTED: &str = "RUNTIME_UNSUPPORTED";
pub const UNINSTALL_INFO_MISSING: &str = "UNINSTALL_INFO_MISSING";
pub const HASH_ALGORITHM_UNSUPPORTED: &str = "HASH_ALGORITHM_UNSUPPORTED";

// --- S: server / third-party, report ---
pub const SOURCE_METADATA_INVALID: &str = "SOURCE_METADATA_INVALID";
pub const REMOTE_FILE_MISSING: &str = "REMOTE_FILE_MISSING";
pub const NO_DOWNLOAD_NODE: &str = "NO_DOWNLOAD_NODE";
pub const EXTRACT_FAILED: &str = "EXTRACT_FAILED";
pub const MIRRORC_FAILED: &str = "MIRRORC_FAILED";
pub const MIRRORC_UNREACHABLE: &str = "MIRRORC_UNREACHABLE";

// --- M: first-party metadata API, report ---
pub const METADATA_UNREACHABLE: &str = "METADATA_UNREACHABLE";
pub const METADATA_HTTP_ERROR: &str = "METADATA_HTTP_ERROR";
pub const METADATA_INVALID: &str = "METADATA_INVALID";

/// Copy-table key only. Not an attach target.
pub const INTERNAL_ERROR: &str = "INTERNAL_ERROR";

/// Every code constant in this module, including `INTERNAL_ERROR`.
pub const ALL_CODES: &[&str] = &[
    DOWNLOAD_TIMEOUT,
    DOWNLOAD_REFUSED,
    DOWNLOAD_FAILED,
    DOWNLOAD_STALLED,
    SERVER_HTTP_ERROR,
    HASH_MISMATCH,
    SOURCE_NEEDS_VERIFICATION,
    PERMISSION_DENIED,
    DISK_FULL,
    FILE_IN_USE,
    FILE_IO_FAILED,
    TEMP_DIR_UNAVAILABLE,
    PROCESS_KILL_FAILED,
    REGISTRY_WRITE_FAILED,
    SHORTCUT_FAILED,
    ELEVATE_FAILED,
    RUNTIME_INSTALL_FAILED,
    WEBVIEW2_REQUIRED,
    WEBVIEW2_FAILED,
    SELF_UPDATE_FAILED,
    MIRRORC_CDK_MISSING,
    MIRRORC_CDK_EXPIRED,
    MIRRORC_CDK_INVALID,
    MIRRORC_CDK_MISMATCH,
    MIRRORC_CDK_QUOTA_EXCEEDED,
    MIRRORC_CDK_BANNED,
    INSTALL_PATH_INVALID,
    PLUGIN_FAILED,
    PKG_BROKEN,
    SOURCE_INVALID,
    VERSION_REGEX_INVALID,
    MIRRORC_CONFIG_INVALID,
    PLUGIN_NO_UI,
    PLUGIN_NOT_FOUND,
    RUNTIME_UNSUPPORTED,
    UNINSTALL_INFO_MISSING,
    HASH_ALGORITHM_UNSUPPORTED,
    SOURCE_METADATA_INVALID,
    REMOTE_FILE_MISSING,
    NO_DOWNLOAD_NODE,
    EXTRACT_FAILED,
    MIRRORC_FAILED,
    MIRRORC_UNREACHABLE,
    METADATA_UNREACHABLE,
    METADATA_HTTP_ERROR,
    METADATA_INVALID,
    INTERNAL_ERROR,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    N,
    E,
    U,
    C,
    S,
    M,
}

#[derive(Debug, Serialize)]
pub struct Coded {
    pub code: &'static str,
    pub detail: Option<String>,
    pub subject: Option<String>,
    pub sid: Option<String>,
    #[serde(skip)]
    source: Option<anyhow::Error>,
}

impl Clone for Coded {
    fn clone(&self) -> Self {
        Self {
            code: self.code,
            detail: self.detail.clone(),
            subject: self.subject.clone(),
            sid: self.sid.clone(),
            source: None,
        }
    }
}

impl PartialEq for Coded {
    fn eq(&self, other: &Self) -> bool {
        self.code == other.code
            && self.detail == other.detail
            && self.subject == other.subject
            && self.sid == other.sid
    }
}

impl Coded {
    pub fn bare(code: &'static str) -> Self {
        Self {
            code,
            detail: None,
            subject: None,
            sid: None,
            source: None,
        }
    }

    pub fn bare_with(code: &'static str, subject: impl Into<String>) -> Self {
        Self {
            code,
            detail: None,
            subject: Some(subject.into()),
            sid: None,
            source: None,
        }
    }

    pub fn with_sid(mut self, sid: impl Into<String>) -> Self {
        self.sid = Some(sid.into());
        self
    }

    /// Wrap `err` as the source of this code, filling `detail` from the source
    /// chain. If `err` is already coded, returns it unchanged (first code wins).
    pub fn wrap(self, err: anyhow::Error) -> anyhow::Error {
        attach_coded(self, err)
    }
}

impl fmt::Display for Coded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code)
    }
}

impl std::error::Error for Coded {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|e| e.as_ref())
    }
}

/// User-cancel marker. Independent of the code table; `extract` prefers it.
#[derive(Debug)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

#[derive(Debug)]
pub enum Extracted<'a> {
    Cancelled,
    Coded(&'a Coded),
    Uncoded { detail: String },
}

pub fn extract(err: &anyhow::Error) -> Extracted<'_> {
    let mut coded = None;
    for cause in err.chain() {
        if cause.downcast_ref::<Cancelled>().is_some() {
            return Extracted::Cancelled;
        }
        if coded.is_none() {
            if let Some(c) = cause.downcast_ref::<Coded>() {
                coded = Some(c);
            }
        }
    }
    match coded {
        Some(c) => Extracted::Coded(c),
        None => Extracted::Uncoded {
            detail: strip_urls(&format!("{err:#}")),
        },
    }
}

fn already_coded(err: &anyhow::Error) -> bool {
    err.chain().any(|c| c.downcast_ref::<Coded>().is_some())
}

fn attach_coded(mut coded: Coded, err: anyhow::Error) -> anyhow::Error {
    if already_coded(&err) {
        return err;
    }
    if coded.detail.is_none() {
        let d = strip_urls(&format!("{err:#}"));
        coded.detail = if d.is_empty() { None } else { Some(d) };
    }
    coded.source = Some(err);
    anyhow::Error::new(coded)
}

fn attach_error(err: anyhow::Error, code: &'static str, subject: Option<String>) -> anyhow::Error {
    attach_coded(
        Coded {
            code,
            detail: None,
            subject,
            sid: None,
            source: None,
        },
        err,
    )
}

/// Hang a code on `anyhow::Error` or `Result`. Idempotent: the first code in
/// the chain is kept.
pub trait Attach<Out> {
    fn attach(self, code: &'static str) -> Out;
    fn attach_with(self, code: &'static str, subject: impl Into<String>) -> Out;
}

impl Attach<anyhow::Error> for anyhow::Error {
    fn attach(self, code: &'static str) -> anyhow::Error {
        attach_error(self, code, None)
    }

    fn attach_with(self, code: &'static str, subject: impl Into<String>) -> anyhow::Error {
        attach_error(self, code, Some(subject.into()))
    }
}

impl<T, E> Attach<anyhow::Result<T>> for Result<T, E>
where
    E: Into<anyhow::Error>,
{
    fn attach(self, code: &'static str) -> anyhow::Result<T> {
        self.map_err(|e| attach_error(e.into(), code, None))
    }

    fn attach_with(self, code: &'static str, subject: impl Into<String>) -> anyhow::Result<T> {
        self.map_err(|e| attach_error(e.into(), code, Some(subject.into())))
    }
}

pub fn class_of(code: &str) -> Option<Class> {
    match code {
        DOWNLOAD_TIMEOUT
        | DOWNLOAD_REFUSED
        | DOWNLOAD_FAILED
        | DOWNLOAD_STALLED
        | SERVER_HTTP_ERROR
        | HASH_MISMATCH
        | SOURCE_NEEDS_VERIFICATION => Some(Class::N),
        PERMISSION_DENIED
        | DISK_FULL
        | FILE_IN_USE
        | FILE_IO_FAILED
        | TEMP_DIR_UNAVAILABLE
        | PROCESS_KILL_FAILED
        | REGISTRY_WRITE_FAILED
        | SHORTCUT_FAILED
        | ELEVATE_FAILED
        | RUNTIME_INSTALL_FAILED
        | WEBVIEW2_REQUIRED
        | WEBVIEW2_FAILED
        | SELF_UPDATE_FAILED => Some(Class::E),
        MIRRORC_CDK_MISSING
        | MIRRORC_CDK_EXPIRED
        | MIRRORC_CDK_INVALID
        | MIRRORC_CDK_MISMATCH
        | MIRRORC_CDK_QUOTA_EXCEEDED
        | MIRRORC_CDK_BANNED
        | INSTALL_PATH_INVALID
        | PLUGIN_FAILED => Some(Class::U),
        PKG_BROKEN
        | SOURCE_INVALID
        | VERSION_REGEX_INVALID
        | MIRRORC_CONFIG_INVALID
        | PLUGIN_NO_UI
        | PLUGIN_NOT_FOUND
        | RUNTIME_UNSUPPORTED
        | UNINSTALL_INFO_MISSING
        | HASH_ALGORITHM_UNSUPPORTED => Some(Class::C),
        SOURCE_METADATA_INVALID
        | REMOTE_FILE_MISSING
        | NO_DOWNLOAD_NODE
        | EXTRACT_FAILED
        | MIRRORC_FAILED
        | MIRRORC_UNREACHABLE => Some(Class::S),
        METADATA_UNREACHABLE | METADATA_HTTP_ERROR | METADATA_INVALID => Some(Class::M),
        _ => None,
    }
}

pub fn should_report(code: &str) -> bool {
    match class_of(code) {
        Some(Class::N | Class::E | Class::U) => false,
        Some(Class::C | Class::S | Class::M) => true,
        None => true,
    }
}

/// Mirror酱 numeric status → code. `0` is success (`None`); any other nonzero
/// maps to a `MIRRORC_*` code.
pub fn code_for_mirrorc_status(status: i64) -> Option<&'static str> {
    if status == 0 {
        return None;
    }
    Some(match status {
        1001 | 8001 | 8002 | 8003 | 8004 => MIRRORC_CONFIG_INVALID,
        7001 => MIRRORC_CDK_EXPIRED,
        7002 => MIRRORC_CDK_INVALID,
        7003 => MIRRORC_CDK_QUOTA_EXCEEDED,
        7004 => MIRRORC_CDK_MISMATCH,
        7005 => MIRRORC_CDK_BANNED,
        _ => MIRRORC_FAILED,
    })
}

fn strip_urls(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while !rest.is_empty() {
        let next_http = rest.find("http://");
        let next_https = rest.find("https://");
        let start = match (next_http, next_https) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        match start {
            None => {
                out.push_str(rest);
                break;
            }
            Some(i) => {
                out.push_str(&rest[..i]);
                let after = &rest[i..];
                let scheme_len = if after.starts_with("https://") { 8 } else { 7 };
                let url_body = &after[scheme_len..];
                let end_rel = url_body
                    .find(|c: char| {
                        c.is_whitespace() || matches!(c, ')' | ',' | '"' | '\'' | '>' | ']' | ';')
                    })
                    .unwrap_or(url_body.len());
                rest = &url_body[end_rel..];
            }
        }
    }
    out
}


impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Class::N => "n",
            Class::E => "e",
            Class::U => "u",
            Class::C => "c",
            Class::S => "s",
            Class::M => "m",
        }
    }
}

pub fn fail_kind(err: &anyhow::Error) -> &'static str {
    match extract(err) {
        Extracted::Cancelled => "cancelled",
        Extracted::Coded(c) => class_of(c.code).map(Class::as_str).unwrap_or("uncoded"),
        Extracted::Uncoded { .. } => "uncoded",
    }
}

pub fn should_report_error(err: &anyhow::Error) -> bool {
    match extract(err) {
        Extracted::Cancelled => false,
        Extracted::Coded(c) => should_report(c.code),
        Extracted::Uncoded { .. } => true,
    }
}

/// One-line silent/log form: code: detail or just code.
pub fn log_line(err: &anyhow::Error) -> String {
    match extract(err) {
        Extracted::Cancelled => "cancelled".to_string(),
        Extracted::Coded(c) => match c.detail.as_deref().filter(|d| !d.is_empty()) {
            Some(d) => format!("{}: {d}", c.code),
            None => c.code.to_string(),
        },
        Extracted::Uncoded { detail } => detail,
    }
}


fn reqwest_ref<'a>(err: &'a (dyn std::error::Error + 'static)) -> Option<&'a reqwest::Error> {
    if let Some(e) = err.downcast_ref::<reqwest::Error>() {
        return Some(e);
    }
    if let Some(e) = err.downcast_ref::<reqwest_middleware::Error>() {
        if let reqwest_middleware::Error::Reqwest(inner) = e {
            return Some(inner);
        }
    }
    None
}

fn code_for_reqwest(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        DOWNLOAD_TIMEOUT
    } else if e.is_connect() {
        DOWNLOAD_REFUSED
    } else if e.status().is_some() {
        SERVER_HTTP_ERROR
    } else {
        DOWNLOAD_FAILED
    }
}

fn code_for_io_kind(kind: std::io::ErrorKind) -> &'static str {
    use std::io::ErrorKind as K;
    match kind {
        K::PermissionDenied => PERMISSION_DENIED,
        K::StorageFull => DISK_FULL,
        K::TimedOut => DOWNLOAD_TIMEOUT,
        K::ConnectionRefused | K::AddrNotAvailable => DOWNLOAD_REFUSED,
        K::ConnectionReset
        | K::ConnectionAborted
        | K::NotConnected
        | K::UnexpectedEof
        | K::BrokenPipe => DOWNLOAD_FAILED,
        _ => FILE_IO_FAILED,
    }
}

fn download_code(err: &anyhow::Error) -> &'static str {
    for cause in err.chain() {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            let msg = io.to_string();
            if msg == crate::utils::error::DOWNLOAD_STALLED
                || msg == crate::utils::error::DOWNLOAD_TOO_SLOW
            {
                return DOWNLOAD_STALLED;
            }
        }
        if let Some(e) = reqwest_ref(cause) {
            return code_for_reqwest(e);
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return code_for_io_kind(io.kind());
        }
    }
    DOWNLOAD_FAILED
}

fn metadata_code(err: &anyhow::Error) -> &'static str {
    for cause in err.chain() {
        if cause.downcast_ref::<serde_json::Error>().is_some() {
            return METADATA_INVALID;
        }
        if let Some(e) = reqwest_ref(cause) {
            if e.is_timeout() || e.is_connect() {
                return METADATA_UNREACHABLE;
            }
            if e.status().is_some() {
                return METADATA_HTTP_ERROR;
            }
            return METADATA_UNREACHABLE;
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return match io.kind() {
                std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::BrokenPipe => METADATA_UNREACHABLE,
                _ => METADATA_UNREACHABLE,
            };
        }
    }
    let head = format!("{err}");
    let status = head.split([':', ' ']).next().unwrap_or("");
    if status.parse::<u16>().is_ok() {
        return METADATA_HTTP_ERROR;
    }
    METADATA_UNREACHABLE
}

/// Hang a download-family code inferred from reqwest / io. Idempotent.
pub fn attach_download(
    err: anyhow::Error,
    subject: Option<&str>,
    sid: Option<&str>,
) -> anyhow::Error {
    if already_coded(&err) {
        return err;
    }
    let mut coded = Coded::bare(download_code(&err));
    if let Some(s) = subject {
        coded.subject = Some(s.to_string());
    }
    if let Some(s) = sid {
        coded.sid = Some(s.to_string());
    }
    attach_coded(coded, err)
}

/// Hang a metadata-family code. Idempotent.
pub fn attach_metadata(err: anyhow::Error) -> anyhow::Error {
    let code = metadata_code(&err);
    err.attach(code)
}

/// True when the error is a retryable network failure (timeout / connect / reset).
pub fn is_retryable_network(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(e) = reqwest_ref(cause) {
            if e.is_timeout() || e.is_connect() {
                return true;
            }
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            use std::io::ErrorKind as K;
            if matches!(
                io.kind(),
                K::TimedOut
                    | K::ConnectionRefused
                    | K::ConnectionReset
                    | K::ConnectionAborted
                    | K::NotConnected
                    | K::UnexpectedEof
                    | K::BrokenPipe
            ) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_is_idempotent_first_code_wins() {
        let err = anyhow::anyhow!("inner")
            .attach(PERMISSION_DENIED)
            .attach(FILE_IO_FAILED);
        match extract(&err) {
            Extracted::Coded(c) => assert_eq!(c.code, PERMISSION_DENIED),
            other => panic!("expected Coded, got {other:?}"),
        }
        let res: anyhow::Result<()> =
            Result::<(), anyhow::Error>::Err(anyhow::anyhow!("x")).attach(DISK_FULL);
        let err = res.unwrap_err();
        match extract(&err) {
            Extracted::Coded(c) => assert_eq!(c.code, DISK_FULL),
            other => panic!("expected Coded, got {other:?}"),
        }
    }

    #[test]
    fn extract_three_states() {
        let cancelled = anyhow::Error::new(Cancelled);
        assert!(matches!(extract(&cancelled), Extracted::Cancelled));

        let coded = anyhow::anyhow!("os error 5").attach(PERMISSION_DENIED);
        match extract(&coded) {
            Extracted::Coded(c) => {
                assert_eq!(c.code, PERMISSION_DENIED);
                assert!(c.detail.as_ref().unwrap().contains("os error 5"));
            }
            other => panic!("expected Coded, got {other:?}"),
        }

        let plain = anyhow::anyhow!("ipc protocol violation");
        match extract(&plain) {
            Extracted::Uncoded { detail } => {
                assert!(detail.contains("ipc protocol violation"));
            }
            other => panic!("expected Uncoded, got {other:?}"),
        }
    }

    #[test]
    fn class_table_and_should_report_one_per_class() {
        assert_eq!(class_of(DOWNLOAD_TIMEOUT), Some(Class::N));
        assert!(!should_report(DOWNLOAD_TIMEOUT));

        assert_eq!(class_of(PERMISSION_DENIED), Some(Class::E));
        assert!(!should_report(PERMISSION_DENIED));

        assert_eq!(class_of(MIRRORC_CDK_MISSING), Some(Class::U));
        assert!(!should_report(MIRRORC_CDK_MISSING));

        assert_eq!(class_of(PKG_BROKEN), Some(Class::C));
        assert!(should_report(PKG_BROKEN));

        assert_eq!(class_of(NO_DOWNLOAD_NODE), Some(Class::S));
        assert!(should_report(NO_DOWNLOAD_NODE));

        assert_eq!(class_of(METADATA_INVALID), Some(Class::M));
        assert!(should_report(METADATA_INVALID));

        assert_eq!(class_of(INTERNAL_ERROR), None);
        assert!(should_report(INTERNAL_ERROR));
    }

    #[test]
    fn cancelled_beats_coded() {
        // Coded wraps Cancelled as source: extract must still prefer Cancelled.
        let err = anyhow::Error::new(Cancelled).attach(PKG_BROKEN);
        assert!(matches!(extract(&err), Extracted::Cancelled));
    }

    #[test]
    fn detail_strips_urls() {
        let err = anyhow::anyhow!(
            "error sending request for url (https://files.example.com/a.bin): timed out"
        )
        .attach(DOWNLOAD_TIMEOUT);
        match extract(&err) {
            Extracted::Coded(c) => {
                let d = c.detail.as_ref().expect("detail");
                assert!(!d.contains("https://"), "detail still has url: {d}");
                assert!(
                    !d.contains("files.example.com"),
                    "detail still has host: {d}"
                );
                assert!(d.contains("timed out"), "lost non-url text: {d}");
            }
            other => panic!("expected Coded, got {other:?}"),
        }
    }

    #[test]
    fn subject_and_sid_pass_through() {
        let err = Coded::bare_with(DOWNLOAD_FAILED, "cdn.example")
            .with_sid("dfs-sid")
            .wrap(anyhow::anyhow!("status 503"));
        match extract(&err) {
            Extracted::Coded(c) => {
                assert_eq!(c.code, DOWNLOAD_FAILED);
                assert_eq!(c.subject.as_deref(), Some("cdn.example"));
                assert_eq!(c.sid.as_deref(), Some("dfs-sid"));
                assert!(c.detail.as_ref().unwrap().contains("503"));
            }
            other => panic!("expected Coded, got {other:?}"),
        }
    }

    #[test]
    fn display_is_code_only() {
        let err = anyhow::anyhow!("os error 5").attach(PERMISSION_DENIED);
        assert_eq!(format!("{err}"), PERMISSION_DENIED);
        let bare = Coded::bare(PKG_BROKEN);
        assert_eq!(format!("{bare}"), PKG_BROKEN);
    }

    #[test]
    fn mirrorc_numeric_map() {
        assert_eq!(code_for_mirrorc_status(0), None);
        for n in [1001, 8001, 8002, 8003, 8004] {
            assert_eq!(code_for_mirrorc_status(n), Some(MIRRORC_CONFIG_INVALID));
        }
        assert_eq!(code_for_mirrorc_status(7001), Some(MIRRORC_CDK_EXPIRED));
        assert_eq!(code_for_mirrorc_status(7002), Some(MIRRORC_CDK_INVALID));
        assert_eq!(
            code_for_mirrorc_status(7003),
            Some(MIRRORC_CDK_QUOTA_EXCEEDED)
        );
        assert_eq!(code_for_mirrorc_status(7004), Some(MIRRORC_CDK_MISMATCH));
        assert_eq!(code_for_mirrorc_status(7005), Some(MIRRORC_CDK_BANNED));
        assert_eq!(code_for_mirrorc_status(42), Some(MIRRORC_FAILED));
    }
}
