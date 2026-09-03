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

// --- C: packager config, report ---
pub const PKG_BROKEN: &str = "PKG_BROKEN";
pub const SOURCE_INVALID: &str = "SOURCE_INVALID";
pub const VERSION_REGEX_INVALID: &str = "VERSION_REGEX_INVALID";
pub const MIRRORC_CONFIG_INVALID: &str = "MIRRORC_CONFIG_INVALID";
pub const PLUGIN_NO_UI: &str = "PLUGIN_NO_UI";
pub const PLUGIN_NOT_FOUND: &str = "PLUGIN_NOT_FOUND";
pub const PLUGIN_FAILED: &str = "PLUGIN_FAILED";
pub const PLUGIN_HOST_FAILED: &str = "PLUGIN_HOST_FAILED";
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
    PKG_BROKEN,
    SOURCE_INVALID,
    VERSION_REGEX_INVALID,
    MIRRORC_CONFIG_INVALID,
    PLUGIN_NO_UI,
    PLUGIN_NOT_FOUND,
    PLUGIN_FAILED,
    PLUGIN_HOST_FAILED,
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

/// Map a code received as text (pipe, JSON) back to its `'static` constant.
pub fn code_from_str(s: &str) -> Option<&'static str> {
    ALL_CODES.iter().copied().find(|c| *c == s)
}

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
    /// Sentry event id, filled at the session boundary once the error has been
    /// reported, so the user can quote it (the copy button includes it).
    pub event_id: Option<String>,
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
            event_id: self.event_id.clone(),
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
            && self.event_id == other.event_id
    }
}

impl Coded {
    pub fn bare(code: &'static str) -> Self {
        Self {
            code,
            detail: None,
            subject: None,
            sid: None,
            event_id: None,
            source: None,
        }
    }

    pub fn bare_with(code: &'static str, subject: impl Into<String>) -> Self {
        Self {
            code,
            detail: None,
            subject: Some(subject.into()),
            sid: None,
            event_id: None,
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

/// DFS2 session id, hung once by the session layer (`tag_session`) on any
/// failure after the session exists. `extract` folds it into `Coded.sid` so
/// inner layers that already hung a code do not need to know the id.
#[derive(Debug)]
pub struct DfsSession {
    pub sid: String,
    source: anyhow::Error,
}

impl fmt::Display for DfsSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "dfs session {}", self.sid)
    }
}

impl std::error::Error for DfsSession {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

pub fn tag_session(err: anyhow::Error, sid: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(DfsSession {
        sid: sid.into(),
        source: err,
    })
}

#[derive(Debug)]
pub enum Extracted {
    Cancelled,
    Coded(Coded),
    Uncoded { detail: String },
}

pub fn extract(err: &anyhow::Error) -> Extracted {
    let mut coded: Option<Coded> = None;
    let mut sid: Option<&str> = None;
    for cause in err.chain() {
        if cause.downcast_ref::<Cancelled>().is_some() {
            return Extracted::Cancelled;
        }
        if coded.is_none() {
            if let Some(c) = cause.downcast_ref::<Coded>() {
                coded = Some(c.clone());
            }
        }
        if sid.is_none() {
            if let Some(s) = cause.downcast_ref::<DfsSession>() {
                sid = Some(&s.sid);
            }
        }
    }
    match coded {
        Some(mut c) => {
            if c.sid.is_none() {
                c.sid = sid.map(str::to_string);
            }
            Extracted::Coded(c)
        }
        None => Extracted::Uncoded {
            detail: strip_urls(&format!("{err:#}")),
        },
    }
}

/// Whether the copy button is worth showing: someone on the other end can act
/// on the copied text. Download (N), server (S) and metadata API (M) failures
/// carry a session / event id the operator can look up; uncoded errors are our
/// defects. Environment (E), user input (U) and packager config (C) failures
/// can only be fixed locally, and a broken package cannot report anyway.
pub fn copy_useful(code: &str) -> bool {
    match class_of(code) {
        Some(Class::N | Class::S | Class::M) | None => true,
        Some(Class::E | Class::U | Class::C) => false,
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
            event_id: None,
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
        | INSTALL_PATH_INVALID => Some(Class::U),
        PKG_BROKEN
        | SOURCE_INVALID
        | VERSION_REGEX_INVALID
        | MIRRORC_CONFIG_INVALID
        | PLUGIN_NO_UI
        | PLUGIN_NOT_FOUND
        | PLUGIN_FAILED
        | PLUGIN_HOST_FAILED
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

/// Mirror酱 API response (`{ code, msg, data }`) → `Coded` with the API's own
/// `msg` as detail. `None` when `code` is absent or `0`.
pub fn coded_for_mirrorc_response(status: &serde_json::Value) -> Option<Coded> {
    let code = status.get("code").and_then(|v| v.as_i64())?;
    let mapped = code_for_mirrorc_status(code)?;
    let mut coded = Coded::bare(mapped);
    coded.detail = status
        .get("msg")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Some(coded)
}

/// `Coded` to render at a session boundary. `None` for user cancel. Uncoded
/// errors become `INTERNAL_ERROR` with the whole chain as detail.
pub fn coded_from_error(err: &anyhow::Error) -> Option<Coded> {
    match extract(err) {
        Extracted::Cancelled => None,
        Extracted::Coded(c) => Some(c),
        Extracted::Uncoded { detail } => {
            let mut c = Coded::bare(INTERNAL_ERROR);
            c.detail = Some(detail).filter(|d| !d.is_empty());
            Some(c)
        }
    }
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

pub fn code_for_network_type(kind: &crate::fs::NetworkErrorType) -> &'static str {
    use crate::fs::NetworkErrorType as N;
    match kind {
        N::DownloadStalled | N::DownloadTooSlow => DOWNLOAD_STALLED,
        N::ConnectionTimeout | N::RequestTimeout => DOWNLOAD_TIMEOUT,
        N::DnsResolutionFailed | N::NetworkUnreachable => DOWNLOAD_REFUSED,
        _ => DOWNLOAD_FAILED,
    }
}

/// `io::Error::source()` skips the custom payload itself, so the stream layer's
/// `ClassifiedNetworkError` has to be read through `get_ref()`.
fn classified_ref(io: &std::io::Error) -> Option<&crate::fs::ClassifiedNetworkError> {
    io.get_ref()
        .and_then(|e| e.downcast_ref::<crate::fs::ClassifiedNetworkError>())
}

/// Download-family code inferred from typed causes only. `None` when the chain
/// has no network / io / HTTP-status cause (e.g. a malformed API response).
fn download_code(err: &anyhow::Error) -> Option<&'static str> {
    download_code_in(err.chain())
}

fn download_code_in<'a>(
    chain: impl IntoIterator<Item = &'a (dyn std::error::Error + 'static)>,
) -> Option<&'static str> {
    for cause in chain {
        if let Some(c) = cause.downcast_ref::<crate::fs::ClassifiedNetworkError>() {
            return Some(code_for_network_type(&c.error_type));
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            if let Some(c) = classified_ref(io) {
                return Some(code_for_network_type(&c.error_type));
            }
            return Some(code_for_io_kind(io.kind()));
        }
        if let Some(e) = reqwest_ref(cause) {
            return Some(code_for_reqwest(e));
        }
        if cause.downcast_ref::<crate::dfs::HttpStatus>().is_some() {
            return Some(SERVER_HTTP_ERROR);
        }
    }
    None
}

/// Code to record in `InsightItem.error`: the hung code if any, otherwise the
/// download-family inference, otherwise `INTERNAL_ERROR`.
pub fn insight_code(err: &anyhow::Error) -> &'static str {
    match extract(err) {
        Extracted::Cancelled => "cancelled",
        Extracted::Coded(c) => c.code,
        Extracted::Uncoded { .. } => download_code(err).unwrap_or(INTERNAL_ERROR),
    }
}

pub fn insight_code_for_io(io: &std::io::Error) -> &'static str {
    let dyn_err: &(dyn std::error::Error + 'static) = io;
    download_code_in(std::iter::once(dyn_err)).unwrap_or(FILE_IO_FAILED)
}

fn metadata_code(err: &anyhow::Error) -> &'static str {
    for cause in err.chain() {
        if cause.downcast_ref::<serde_json::Error>().is_some() {
            return METADATA_INVALID;
        }
        if cause.downcast_ref::<crate::dfs::HttpStatus>().is_some() {
            return METADATA_HTTP_ERROR;
        }
        if let Some(e) = reqwest_ref(cause) {
            if e.status().is_some() {
                return METADATA_HTTP_ERROR;
            }
            return METADATA_UNREACHABLE;
        }
        if cause.downcast_ref::<std::io::Error>().is_some() {
            return METADATA_UNREACHABLE;
        }
    }
    METADATA_INVALID
}

/// Hang a download-family code inferred from typed network / io causes;
/// `DOWNLOAD_FAILED` when none is found. Idempotent.
pub fn attach_download(
    err: anyhow::Error,
    subject: Option<&str>,
    sid: Option<&str>,
) -> anyhow::Error {
    attach_download_or(err, DOWNLOAD_FAILED, subject, sid)
}

/// Like `attach_download`, but a chain without any network / io / HTTP-status
/// cause (the peer answered, the answer is unusable) gets `fallback` instead.
pub fn attach_download_or(
    err: anyhow::Error,
    fallback: &'static str,
    subject: Option<&str>,
    sid: Option<&str>,
) -> anyhow::Error {
    if already_coded(&err) {
        return err;
    }
    let mut coded = Coded::bare(download_code(&err).unwrap_or(fallback));
    coded.subject = subject.map(str::to_string);
    coded.sid = sid.map(str::to_string);
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
        assert_eq!(class_of(PLUGIN_FAILED), Some(Class::C));
        assert!(should_report(PLUGIN_FAILED));
        assert_eq!(class_of(PLUGIN_HOST_FAILED), Some(Class::C));
        assert!(should_report(PLUGIN_HOST_FAILED));

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
    fn download_or_uses_fallback_only_without_network_cause() {
        let typed = attach_download_or(
            anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::TimedOut)),
            NO_DOWNLOAD_NODE,
            None,
            None,
        );
        assert!(matches!(extract(&typed), Extracted::Coded(c) if c.code == DOWNLOAD_TIMEOUT));

        let status = attach_download_or(
            anyhow::Error::new(crate::dfs::HttpStatus::new(503, "busy")),
            NO_DOWNLOAD_NODE,
            None,
            None,
        );
        assert!(matches!(extract(&status), Extracted::Coded(c) if c.code == SERVER_HTTP_ERROR));

        let garbage = attach_download_or(
            anyhow::anyhow!("Invalid challenge"),
            NO_DOWNLOAD_NODE,
            None,
            None,
        );
        assert!(matches!(extract(&garbage), Extracted::Coded(c) if c.code == NO_DOWNLOAD_NODE));

        let stalled = attach_download(
            anyhow::Error::new(std::io::Error::from(crate::fs::ClassifiedNetworkError::new(
                crate::fs::NetworkErrorType::DownloadStalled,
                Box::new(std::io::Error::other("stalled")),
                "https://x".into(),
                vec![],
            ))),
            None,
            None,
        );
        assert!(matches!(extract(&stalled), Extracted::Coded(c) if c.code == DOWNLOAD_STALLED));
    }

    #[test]
    fn metadata_code_is_typed() {
        let http = attach_metadata(anyhow::Error::new(crate::dfs::HttpStatus::new(500, "x")));
        assert!(matches!(extract(&http), Extracted::Coded(c) if c.code == METADATA_HTTP_ERROR));
        let json = attach_metadata(serde_json::from_str::<u8>("nope").unwrap_err().into());
        assert!(matches!(extract(&json), Extracted::Coded(c) if c.code == METADATA_INVALID));
        assert_eq!(code_from_str("PKG_BROKEN"), Some(PKG_BROKEN));
        assert_eq!(code_from_str("NOPE"), None);
    }

    #[test]
    fn session_id_folds_into_coded_and_copy_rule_follows_class() {
        let inner = anyhow::anyhow!("mismatch").attach(HASH_MISMATCH);
        let err = tag_session(inner, "sid-9");
        let Extracted::Coded(c) = extract(&err) else {
            panic!("coded lost under session tag");
        };
        assert_eq!(c.code, HASH_MISMATCH);
        assert_eq!(c.sid.as_deref(), Some("sid-9"));

        let explicit = tag_session(
            Coded::bare(DOWNLOAD_FAILED)
                .with_sid("sid-own")
                .wrap(anyhow::anyhow!("x")),
            "sid-outer",
        );
        let Extracted::Coded(c) = extract(&explicit) else {
            panic!("coded lost");
        };
        assert_eq!(c.sid.as_deref(), Some("sid-own"), "an explicit sid wins");

        assert!(copy_useful(DOWNLOAD_TIMEOUT));
        assert!(copy_useful(NO_DOWNLOAD_NODE));
        assert!(copy_useful(METADATA_INVALID));
        assert!(copy_useful(INTERNAL_ERROR));
        assert!(!copy_useful(PERMISSION_DENIED));
        assert!(!copy_useful(MIRRORC_CDK_EXPIRED));
        assert!(!copy_useful(PKG_BROKEN));
    }

    #[test]
    fn log_line_code_colon_detail() {
        let with_detail = anyhow::anyhow!("http 502").attach(METADATA_HTTP_ERROR);
        assert_eq!(log_line(&with_detail), "METADATA_HTTP_ERROR: http 502");
        let bare = anyhow::Error::from(Coded::bare(PKG_BROKEN));
        assert_eq!(log_line(&bare), "PKG_BROKEN");
        let cancelled = anyhow::Error::new(Cancelled);
        assert_eq!(log_line(&cancelled), "cancelled");
        let uncoded = anyhow::anyhow!("ipc protocol violation");
        assert_eq!(log_line(&uncoded), "ipc protocol violation");
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
