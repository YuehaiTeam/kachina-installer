//! 文件动作快照同时携带有效完成量和实际处理量，丢失中间事件不改变计数含义。
use super::{Progress, ProgressNotify};
use crate::session::state::FileAction;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Snapshot {
    pub index: u32,
    pub sequence: u64,
    pub action: FileAction,
    pub done: Option<u64>,
    pub total: Option<u64>,
    pub completed: u64,
    pub work_total: u64,
    pub processed: u64,
    pub finished: bool,
}

#[derive(Clone)]
pub struct Reporter {
    state: Arc<Mutex<(Snapshot, u64)>>,
    notify: ProgressNotify,
}
impl Reporter {
    pub fn new(total: u64, notify: ProgressNotify) -> Self {
        Self {
            state: Arc::new(Mutex::new((
                Snapshot {
                    index: 0,
                    sequence: 0,
                    action: FileAction::Download,
                    done: None,
                    total: None,
                    completed: 0,
                    work_total: total,
                    processed: 0,
                    finished: false,
                },
                0,
            ))),
            notify,
        }
    }
    pub fn action(&self, action: FileAction, total: Option<u64>) {
        let snapshot = {
            let mut state = self.state.lock().unwrap();
            state.1 = state.0.completed;
            state.0.action = action;
            state.0.done = total.map(|_| 0);
            state.0.total = total;
            state.0.sequence += 1;
            state.0.clone()
        };
        (self.notify)(Progress::File(snapshot));
    }
    pub fn bytes(&self, done: usize) {
        let snapshot = {
            let mut state = self.state.lock().unwrap();
            let done = done as u64;
            state.0.processed += done.saturating_sub(state.0.done.unwrap_or(0));
            state.0.done = Some(done);
            state.0.total = state.0.total.map(|total| total.max(done));
            state.0.completed = state.1 + done;
            state.0.work_total = state.0.work_total.max(state.0.completed);
            state.0.sequence += 1;
            state.0.clone()
        };
        (self.notify)(Progress::File(snapshot));
    }
    pub fn finish(&self) {
        let snapshot = {
            let mut state = self.state.lock().unwrap();
            state.0.finished = true;
            state.0.sequence += 1;
            state.0.clone()
        };
        (self.notify)(Progress::File(snapshot));
    }
}
