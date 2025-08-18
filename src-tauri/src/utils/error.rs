// This file is part of the `anyhow-tauri` library.

use crate::dfs::InsightItem;
use serde::Serialize;
use std::sync::{Arc, Mutex};

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

// Every "renspose" from a tauri command needs to be serializeable into json with serde.
// This is why we cannot use `anyhow` directly. This piece of code fixes that.
impl Serialize for TACommandError {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        struct ErrorWithInsight {
            message: String,
            insight: Option<InsightItem>,
        }

        let response = ErrorWithInsight {
            message: format!("{:#}", self.error),
            insight: self.insight.clone(),
        };

        super::sentry::capture_anyhow(&self.error);
        response.serialize(serializer)
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

/// Use this as your command's return type.
///
/// Example usage:
/// ```
/// #[tauri::command]
/// fn test() -> anyhow_tauri::TAResult<String> {
///     Ok("No error thrown.".into())
/// }
/// ```
///
/// You can find more examples inside the library's repo at `/demo/src-tauri/src/main.rs`
pub type TAResult<T> = std::result::Result<T, TACommandError>;

pub trait IntoTAResult<T> {
    fn into_ta_result(self) -> TAResult<T>;
}

impl<T, E> IntoTAResult<T> for std::result::Result<T, E>
where
    E: Into<anyhow::Error>,
{
    /// Maps errors, which can be converted into `anyhow`'s error type, into `TACommandError` which can be returned from command call.
    /// This is a "quality of life" improvement.
    ///
    /// Example usage:
    /// ```
    /// #[tauri::command]
    /// fn test_into_ta_result() -> anyhow_tauri::TAResult<String> {
    ///     function_that_succeeds().into_ta_result()
    ///     // could also be written as:
    ///     // Ok(function_that_succeeds()?)
    /// }
    /// ```
    fn into_ta_result(self) -> TAResult<T> {
        self.map_err(|e| TACommandError {
            error: e.into(),
            insight: None,
        })
    }
}
impl<T> IntoTAResult<T> for anyhow::Error {
    /// Maps `anyhow`'s error type into `TACommandError` which can be returned from a command call.
    /// This is a "quality of life" improvement.
    ///
    /// Example usage:
    /// ```
    /// #[tauri::command]
    /// fn test_into_ta_result() -> anyhow_tauri::TAResult<String> {
    ///     function_that_succeeds().into_ta_result()
    ///     // could also be written as:
    ///     // Ok(function_that_succeeds()?)
    /// }
    /// ```
    fn into_ta_result(self) -> TAResult<T> {
        Err(TACommandError {
            error: self,
            insight: None,
        })
    }
}

pub trait IntoEmptyTAResult<T> {
    /// Usefull whenever you want to create `Result<(), TACommandError>` (or `TAResult<()>`)
    ///
    /// Example usage:
    /// ```
    /// #[tauri::command]
    /// fn test_into_ta_empty_result() -> anyhow_tauri::TAResult<()> {
    ///     anyhow::anyhow!("Showcase of the .into_ta_empty_result()").into_ta_empty_result()
    /// }
    /// ```
    fn into_ta_empty_result(self) -> TAResult<T>;
}
impl IntoEmptyTAResult<()> for anyhow::Error {
    fn into_ta_empty_result(self) -> TAResult<()> {
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
}
