//! 每次 IPC 操作的累计网络量。读取路径只更新原子值，定时快照与最终结果共用计数。

use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicU32, AtomicU64, Ordering},
    Arc,
};

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Snapshot {
    pub bytes: u64,
    pub active: u32,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub network: Snapshot,
    pub files: Vec<super::file_progress::Snapshot>,
    pub insights: Vec<crate::dfs::InsightItem>,
}
impl Stats {
    pub fn publish(self, notify: &super::ProgressNotify) {
        for insight in self.insights {
            notify(super::Progress::Insight(insight));
        }
        for file in self.files {
            notify(super::Progress::File(file));
        }
        notify(super::Progress::NetworkFinal(self.network));
    }
}

#[derive(Default)]
pub struct Meter {
    bytes: AtomicU64,
    active: AtomicU32,
}

tokio::task_local! {
    pub static CURRENT: Arc<Meter>;
}

impl Meter {
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            bytes: self.bytes.load(Ordering::Relaxed),
            active: self.active.load(Ordering::Relaxed),
        }
    }

    pub fn add(&self, bytes: u64) {
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub struct Reading(Option<Arc<Meter>>);

impl Reading {
    pub fn begin() -> Self {
        let meter = CURRENT.try_with(Arc::clone).ok();
        if let Some(meter) = &meter {
            meter.active.fetch_add(1, Ordering::Relaxed);
        }
        Self(meter)
    }

    pub fn add(&self, bytes: u64) {
        if let Some(meter) = &self.0 {
            meter.add(bytes);
        }
    }

    pub fn finish(&mut self) {
        if let Some(meter) = self.0.take() {
            meter.active.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        self.finish();
    }
}

pub struct Reader<R> {
    inner: R,
    reading: Reading,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    remaining: Option<u64>,
    cancel: Option<std::pin::Pin<Box<tokio_util::sync::WaitForCancellationFutureOwned>>>,
}

impl<R> Reader<R> {
    pub fn new(
        inner: R,
        reading: Reading,
        permit: Option<tokio::sync::OwnedSemaphorePermit>,
        length: Option<u64>,
    ) -> Self {
        let cancel = super::download::CURRENT
            .try_with(|c| Box::pin(c.stop.clone().cancelled_owned()))
            .ok();
        Self {
            inner,
            reading,
            permit,
            remaining: length,
            cancel,
        }
    }
    fn finish(&mut self) {
        self.reading.finish();
        self.permit.take();
    }
}

impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for Reader<R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        use std::{future::Future, task::Poll};
        if self
            .cancel
            .as_mut()
            .is_some_and(|cancel| cancel.as_mut().poll(cx).is_ready())
        {
            self.finish();
            return Poll::Ready(Err(std::io::Error::other(super::download::SharedError(
                Arc::new(crate::utils::code::Cancelled.into()),
            ))));
        }
        let before = buf.filled().len();
        let capacity = buf.remaining();
        let result = std::pin::Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(ref outcome) = result {
            let read = (buf.filled().len() - before) as u64;
            self.reading.add(read);
            if let Some(remaining) = &mut self.remaining {
                *remaining = remaining.saturating_sub(read);
            }
            if outcome.is_err() || (read == 0 && capacity > 0) || self.remaining == Some(0) {
                self.finish();
            }
        }
        result
    }
}

/// 操作结束后返回同一计数的最终值，调用者须在完成通知前处理它。
pub async fn measure<F, Make>(notify: super::ProgressNotify, operation: Make) -> (F::Output, Stats)
where
    F: std::future::Future,
    Make: FnOnce(super::ProgressNotify) -> F,
{
    let meter = Arc::new(Meter::default());
    let files = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
    let insights = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recording = {
        let notify = notify.clone();
        let files = files.clone();
        let insights = insights.clone();
        super::progress_notify(move |p| {
            if let super::Progress::Insight(insight) = p {
                insights.lock().unwrap().push(insight);
                return;
            }
            if let super::Progress::File(file) = &p {
                files.lock().unwrap().insert(file.index, file.clone());
            }
            notify(p);
        })
    };
    let mut operation = Box::pin(CURRENT.scope(meter.clone(), operation(recording)));
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
    loop {
        tokio::select! {
            result = &mut operation => return (result, Stats { network: meter.snapshot(), files: files.lock().unwrap().values().cloned().collect(), insights: insights.lock().unwrap().clone() }),
            _ = interval.tick() => notify(super::Progress::Network(meter.snapshot())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn reading_lifetime_and_large_counters() {
        let meter = Arc::new(Meter::default());
        CURRENT
            .scope(meter.clone(), async {
                let mut first = Reading::begin();
                let second = Reading::begin();
                first.add(u32::MAX as u64 + 7);
                second.add(3);
                assert_eq!(meter.snapshot().active, 2);
                first.finish();
                first.finish();
                assert_eq!(meter.snapshot().active, 1);
            })
            .await;
        assert_eq!(
            meter.snapshot(),
            Snapshot {
                bytes: u32::MAX as u64 + 10,
                active: 0
            }
        );
    }
    #[tokio::test]
    async fn failed_operation_keeps_final_counts_and_cancellation_wakes_reader() {
        use tokio::io::AsyncReadExt;
        let (result, stats) = measure(super::super::progress_noop(), |notify| async move {
            let reading = Reading::begin();
            reading.add(31);
            let reporter = super::super::file_progress::Reporter::new(20, notify);
            reporter.action(crate::session::state::FileAction::Patch, Some(20));
            reporter.bytes(12);
            Err::<(), _>(anyhow::anyhow!("failed attempt"))
        })
        .await;
        assert!(result.is_err());
        assert_eq!(
            stats.network,
            Snapshot {
                bytes: 31,
                active: 0
            }
        );
        assert_eq!(stats.files[0].processed, 12);
        let id = uuid::Uuid::new_v4().to_string();
        super::super::download::begin(super::super::download::Config {
            id: id.clone(),
            concurrency: 1,
            dl_dir: String::new(),
            prefetch_bytes: 0,
        });
        let context = super::super::download::context(
            &super::super::download::Job {
                session: id.clone(),
                large: true,
            },
            false,
        )
        .unwrap();
        super::super::download::CURRENT
            .scope(context.clone(), async {
                let (reader, _writer) = tokio::io::duplex(1);
                let mut reader = Reader::new(reader, Reading::default(), None, None);
                context.stop.cancel();
                let err = tokio::time::timeout(std::time::Duration::from_secs(1), reader.read_u8())
                    .await
                    .unwrap()
                    .unwrap_err();
                assert!(matches!(
                    crate::utils::code::extract(&err.into()),
                    crate::utils::code::Extracted::Cancelled
                ));
            })
            .await;
        super::super::download::finish(&context, false)
            .await
            .unwrap();
        super::super::download::end(&id);
    }
}
