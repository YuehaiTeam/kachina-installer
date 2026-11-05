//! Error type of bridge commands and IPC operations: an `anyhow::Error` plus
//! the DFS insight of a failed download. Adapted from the `anyhow-tauri`
//! crate, which the `TA` prefix comes from.

use crate::dfs::InsightItem;
use std::sync::{Arc, Mutex};

// Download error constants
pub const DOWNLOAD_STALLED: &str = "DOWNLOAD_STALLED";
pub const DOWNLOAD_TOO_SLOW: &str = "DOWNLOAD_TOO_SLOW";

// Just extending the `anyhow::Error`
#[derive(Debug)]
pub struct TACommandError {
    pub error: anyhow::Error,
    pub insight: Option<InsightItem>,
}
impl std::error::Error for TACommandError {}
impl std::fmt::Display for TACommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.error)
    }
}

// Ability to convert between `anyhow::Error` and `TACommandError`
impl From<anyhow::Error> for TACommandError {
    fn from(error: anyhow::Error) -> Self {
        Self {
            error,
            insight: None,
        }
    }
}

pub type TAResult<T> = std::result::Result<T, TACommandError>;

/// Wrap an error convertible to `anyhow::Error` as a `TACommandError` without
/// insight.
pub trait IntoTAResult<T> {
    fn into_ta_result(self) -> TAResult<T>;
}

impl<T, E> IntoTAResult<T> for std::result::Result<T, E>
where
    E: Into<anyhow::Error>,
{
    fn into_ta_result(self) -> TAResult<T> {
        self.map_err(|e| TACommandError {
            error: e.into(),
            insight: None,
        })
    }
}
impl<T> IntoTAResult<T> for anyhow::Error {
    fn into_ta_result(self) -> TAResult<T> {
        Err(TACommandError {
            error: self,
            insight: None,
        })
    }
}

pub trait IntoAnyhow<T> {
    // convert TAResult<T> into anyhow::Result<T>
    fn into_anyhow(self) -> std::result::Result<T, anyhow::Error>;
}
impl<T> IntoAnyhow<T> for TAResult<T> {
    fn into_anyhow(self) -> std::result::Result<T, anyhow::Error> {
        self.map_err(|e| e.error)
    }
}

pub fn return_ta_result<T>(msg: String, ctx: &str) -> TAResult<T> {
    Err(TACommandError {
        error: anyhow::anyhow!(msg).context(ctx.to_string()),
        insight: None,
    })
}

pub fn return_anyhow_result<T>(msg: String, ctx: &str) -> anyhow::Result<T> {
    Err(anyhow::anyhow!(msg).context(ctx.to_string()))
}

impl TACommandError {
    pub fn new(error: anyhow::Error) -> Self {
        Self {
            error,
            insight: None,
        }
    }

    pub fn with_insight(error: anyhow::Error, insight: InsightItem) -> Self {
        Self {
            error,
            insight: Some(insight),
        }
    }

    pub fn with_insight_handle(
        error: anyhow::Error,
        insight_handle: Arc<Mutex<InsightItem>>,
    ) -> Self {
        let insight = if let Ok(insight) = insight_handle.lock() {
            Some(insight.clone())
        } else {
            None
        };

        Self { error, insight }
    }

    /// 会话失败上报咽喉。非 pipe、无 insight、且码的类要求上报时发错误事件，
    /// 返回事件 id。提权进程把错误送回主进程再报；带 insight 的走 DFS。
    pub fn report_if_needed(&self) -> Option<String> {
        if !crate::utils::sentry::is_pipe_mode()
            && self.insight.is_none()
            && crate::utils::code::should_report_error(&self.error)
        {
            return Some(super::sentry::capture_anyhow(&self.error));
        }
        None
    }
}
