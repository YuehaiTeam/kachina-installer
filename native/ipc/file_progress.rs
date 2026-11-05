//! 文件进度快照同时携带有效完成量和实际处理量，丢失中间事件不改变计数含义。
use super::{Progress, ProgressNotify};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Snapshot {
    pub index: u32,
    pub sequence: u64,
    pub completed: u64,
    pub work_total: u64,
    pub processed: u64,
    pub finished: bool,
}

struct State {
    snapshot: Snapshot,
    /// `completed` when the current segment began
    base: u64,
    /// bytes the current segment has reported
    done: u64,
}

#[derive(Clone)]
pub struct Reporter {
    state: Arc<Mutex<State>>,
    notify: ProgressNotify,
}
impl Reporter {
    pub fn new(total: u64, notify: ProgressNotify) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                snapshot: Snapshot {
                    index: 0,
                    sequence: 0,
                    completed: 0,
                    work_total: total,
                    processed: 0,
                    finished: false,
                },
                base: 0,
                done: 0,
            })),
            notify,
        }
    }
    fn update(&self, change: impl FnOnce(&mut State)) {
        let snapshot = {
            let mut state = self.state.lock().unwrap();
            change(&mut state);
            state.snapshot.sequence += 1;
            state.snapshot.clone()
        };
        (self.notify)(Progress::File(snapshot));
    }
    /// Starts a part of the file's work whose byte counts restart at zero
    /// (the base of a hybrid patch, then its diff); they add to what earlier
    /// parts completed.
    pub fn segment(&self) {
        self.update(|state| {
            state.base = state.snapshot.completed;
            state.done = 0;
        });
    }
    /// Bytes of the current segment so far; a repeated count adds nothing.
    pub fn bytes(&self, done: usize) {
        self.update(|state| {
            let done = done as u64;
            state.snapshot.processed += done.saturating_sub(state.done);
            state.done = done;
            state.snapshot.completed = state.base + done;
            state.snapshot.work_total = state.snapshot.work_total.max(state.snapshot.completed);
        });
    }
    pub fn finish(&self) {
        self.update(|state| state.snapshot.finished = true);
    }
}
