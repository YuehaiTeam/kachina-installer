//! 最小 tracing 后端，替代 tracing-subscriber 的 registry + fmt 栈。
//!
//! 单个全局 Subscriber 同时完成三件事：INFO 级别过滤、控制台/日志文件输出、
//! Sentry 面包屑写入。仓库内没有任何 span 使用，span 相关方法均为空实现。

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct LogSubscriber {
    file: Option<Mutex<File>>,
    next_span_id: AtomicU64,
}

/// 安装为全局 Subscriber。日志文件打不开时静默降级为仅控制台。
pub fn init(log_file: &std::path::Path) {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .ok();
    let _ = tracing::subscriber::set_global_default(LogSubscriber {
        file: file.map(Mutex::new),
        next_span_id: AtomicU64::new(1),
    });
}

struct FieldVisitor {
    message: String,
}

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let sep = if self.message.is_empty() { "" } else { " " };
            let _ = write!(self.message, "{sep}{}={value:?}", field.name());
        }
    }
}

fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let base = crate::utils::sentry::rfc3339(now.as_secs());
    // "....SSZ" → 去掉 Z 补毫秒
    format!("{}.{:03}Z", &base[..base.len() - 1], now.subsec_millis())
}

impl tracing::Subscriber for LogSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.level() <= &tracing::Level::INFO
    }

    fn event(&self, event: &tracing::Event<'_>) {
        let meta = event.metadata();
        let level = *meta.level();
        let mut visitor = FieldVisitor {
            message: String::new(),
        };
        event.record(&mut visitor);

        let level_str = match level {
            tracing::Level::ERROR => "error",
            tracing::Level::WARN => "warning",
            _ => "info",
        };
        crate::utils::sentry::add_breadcrumb(meta.target(), level_str, visitor.message.clone());

        let color = match level {
            tracing::Level::ERROR => "\x1b[31m",
            tracing::Level::WARN => "\x1b[33m",
            _ => "\x1b[32m",
        };
        let ts = timestamp();
        let line = format!(
            "{ts} {color}{level:>5}\x1b[0m {}: {}\n",
            meta.target(),
            visitor.message
        );
        {
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(line.as_bytes());
        }
        if let Some(file) = &self.file {
            let plain = format!("{ts} {level:>5} {}: {}\n", meta.target(), visitor.message);
            if let Ok(mut file) = file.lock() {
                let _ = file.write_all(plain.as_bytes());
            }
        }
    }

    fn new_span(&self, _attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(self.next_span_id.fetch_add(1, Ordering::Relaxed))
    }
    fn record(&self, _id: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _id: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn enter(&self, _id: &tracing::span::Id) {}
    fn exit(&self, _id: &tracing::span::Id) {}
}
