pub mod install_file;
pub mod manager;
pub mod operation;

use std::sync::Arc;

pub type ProgressNotify = Arc<dyn Fn(serde_json::Value) + Send + Sync>;

pub fn progress_notify(f: impl Fn(serde_json::Value) + Send + Sync + 'static) -> ProgressNotify {
    Arc::new(f)
}

pub fn progress_noop() -> ProgressNotify {
    progress_notify(|_| {})
}
