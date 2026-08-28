//! 最小 Sentry 协议客户端，手拼 JSON、不依赖 SDK（设计见 docs/notes 手写最小 Sentry 协议客户端）。
//!
//! - 错误事件：`capture_anyhow` 按 anyhow 链拼 exception.values，附面包屑快照与 contexts。
//! - 面包屑：[`BreadcrumbLayer`] 从 tracing 事件写入环形缓冲。
//! - 提权进程：面包屑与 envelope 经管道交主进程，主进程不解析。
//! - 性能：会话级 [`Transaction`] + 一层阶段 span + measurements。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const DSN: &str =
    "http://f68ff71bf7fee106fb09fbae79031502@steambird.cocogoat.cn/insight/kachina-installer/0";
const ENVELOPE_URL: &str =
    "http://steambird.cocogoat.cn/insight/kachina-installer/api/0/envelope/";
const SENTRY_KEY: &str = "f68ff71bf7fee106fb09fbae79031502";
const RELEASE: &str = concat!(env!("CARGO_PKG_NAME"), "@", env!("CARGO_PKG_VERSION"));
/// X-Sentry-Auth 的 sentry_client 与 sdk 字段要求 `name/version` 格式。
const CLIENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
const ENVIRONMENT: &str = if cfg!(debug_assertions) {
    "development"
} else {
    "production"
};
const MAX_BREADCRUMBS: usize = 100;

struct State {
    breadcrumbs: Mutex<VecDeque<Value>>,
    contexts: Mutex<serde_json::Map<String, Value>>,
    user: Mutex<Value>,
    use_pipe: AtomicBool,
}

pub struct PipeOutbox {
    tx: tokio::sync::mpsc::Sender<Value>,
    pub rx: Arc<tokio::sync::RwLock<tokio::sync::mpsc::Receiver<Value>>>,
}

lazy_static::lazy_static! {
    static ref STATE: State = State {
        breadcrumbs: Mutex::new(VecDeque::with_capacity(MAX_BREADCRUMBS)),
        contexts: Mutex::new(serde_json::Map::new()),
        user: Mutex::new(Value::Null),
        use_pipe: AtomicBool::new(false),
    };
    /// 提权进程的出站队列（`{"envelope": …}` / `{"breadcrumb": …}`），
    /// 由 `uac_ipc_main` 消费后原样写入管道。
    pub static ref PIPE_OUTBOX: PipeOutbox = {
        let (tx, rx) = tokio::sync::mpsc::channel(256);
        PipeOutbox { tx, rx: Arc::new(tokio::sync::RwLock::new(rx)) }
    };
}

static INFLIGHT: AtomicUsize = AtomicUsize::new(0);

fn now_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn new_uuid() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn new_span_id() -> String {
    new_uuid()[..16].to_string()
}

/// `use_pipe`：提权进程置 true，面包屑与 envelope 经管道交主进程。
pub fn init(use_pipe: bool) {
    STATE.use_pipe.store(use_pipe, Ordering::SeqCst);
}

pub fn is_pipe_mode() -> bool {
    STATE.use_pipe.load(Ordering::SeqCst)
}

/// 丢弃时只记 DEBUG——WARN 会进面包屑层再次触发转发，形成反馈环。
fn pipe_send(msg: Value, kind: &str) {
    if let Err(e) = PIPE_OUTBOX.tx.try_send(msg) {
        tracing::debug!("sentry pipe outbox drop ({kind}): {e}");
    }
}

pub fn set_context(key: &str, value: Value) {
    if let Ok(mut ctx) = STATE.contexts.lock() {
        ctx.insert(key.to_string(), value);
    }
}

pub fn set_app_info() {
    let wv2ver = crate::host::webview_version().unwrap_or_else(|_| "Unknown".to_string());
    set_context(
        "browser",
        json!({ "type": "browser", "name": "Webview2", "version": wv2ver }),
    );
    set_context(
        "app",
        json!({
            "type": "app",
            "app_name": "KachinaInstaller",
            "app_version": env!("CARGO_PKG_VERSION"),
            "build_type": if cfg!(debug_assertions) { "Debug" } else { "Release" },
        }),
    );
    if let Ok(mut user) = STATE.user.lock() {
        *user = json!({
            "id": crate::utils::get_device_id().ok(),
            "ip_address": "{{auto}}",
        });
    }
}

pub fn add_breadcrumb(category: &str, level: &str, message: String) {
    let crumb = json!({
        "timestamp": now_f64(),
        "type": "default",
        "category": category,
        "level": level,
        "message": message,
    });
    // 提权进程本地留一份（panic 事件用），同时转发主进程合并出完整时间线
    if is_pipe_mode() {
        pipe_send(json!({ "breadcrumb": crumb }), "breadcrumb");
    }
    add_breadcrumb_value(crumb);
}

/// 主进程也用它接收提权进程转发来的面包屑。
pub fn add_breadcrumb_value(crumb: Value) {
    if let Ok(mut crumbs) = STATE.breadcrumbs.lock() {
        if crumbs.len() >= MAX_BREADCRUMBS {
            crumbs.pop_front();
        }
        crumbs.push_back(crumb);
    }
}

fn breadcrumbs_snapshot() -> Value {
    let values: Vec<Value> = STATE
        .breadcrumbs
        .lock()
        .map(|c| c.iter().cloned().collect())
        .unwrap_or_default();
    json!({ "values": values })
}

fn base_event(event_id: &str) -> Value {
    let contexts = STATE
        .contexts
        .lock()
        .map(|c| c.clone())
        .unwrap_or_default();
    let user = STATE
        .user
        .lock()
        .map(|u| u.clone())
        .unwrap_or(Value::Null);
    json!({
        "event_id": event_id,
        "timestamp": now_f64(),
        "platform": "native",
        "release": RELEASE,
        "environment": ENVIRONMENT,
        "sdk": { "name": env!("CARGO_PKG_NAME"), "version": env!("CARGO_PKG_VERSION") },
        "user": user,
        "contexts": Value::Object(contexts),
    })
}

/// 过滤（Expected 标记）由调用方负责，本函数无条件发送。
pub fn capture_anyhow(err: &anyhow::Error) {
    // exception.values 按协议要求 root cause 在前、最外层（主异常）在后
    let mut values: Vec<Value> = err
        .chain()
        .map(|cause| json!({ "type": "Error", "value": cause.to_string() }))
        .collect();
    values.reverse();
    let event_id = new_uuid();
    let mut event = base_event(&event_id);
    event["level"] = json!("error");
    event["exception"] = json!({ "values": values });
    event["breadcrumbs"] = breadcrumbs_snapshot();
    dispatch(envelope(&event_id, "event", &event), false);
}

/// `event_id` 由调用方生成——须在上报前就能交给崩溃提示进程展示。
pub fn capture_panic(event_id: &str, message: String) {
    let mut event = base_event(event_id);
    event["level"] = json!("fatal");
    event["exception"] = json!({ "values": [{ "type": "panic", "value": message }] });
    event["breadcrumbs"] = breadcrumbs_snapshot();
    dispatch(envelope(event_id, "event", &event), true);
}

fn envelope(event_id: &str, item_type: &str, payload: &Value) -> String {
    let payload = payload.to_string();
    format!(
        "{}\n{}\n{}\n",
        json!({ "event_id": event_id, "dsn": DSN, "sent_at": rfc3339(now_f64() as u64) }),
        json!({ "type": item_type, "length": payload.len() }),
        payload
    )
}

/// RFC3339 UTC 时间戳，不引入 chrono——envelope header 的 sent_at 只需秒级精度。
fn rfc3339(secs: u64) -> String {
    let days = secs / 86400;
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    // civil_from_days (Howard Hinnant 算法)
    let z = days as i64 + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// 主进程收到提权进程转发的 envelope 原文，不解析直接 POST。
pub fn forward_raw_envelope(body: String) {
    dispatch(body, false);
}

async fn post_envelope(body: String) {
    let res = crate::RAW_CLIENT
        .post(ENVELOPE_URL)
        .header("content-type", "application/x-sentry-envelope")
        .header(
            "x-sentry-auth",
            format!("Sentry sentry_version=7, sentry_client={CLIENT}, sentry_key={SENTRY_KEY}"),
        )
        .body(body)
        .send()
        .await;
    if let Err(e) = res {
        tracing::debug!("sentry send failed: {e}");
    }
}

fn dispatch(body: String, blocking: bool) {
    // 同步路径（panic hook）不走管道：panic = "abort" 下进程随即终止，
    // 排队进管道的消息来不及被写循环消费，必须在本进程直发 HTTP。
    if blocking {
        // 独立线程 + 独立 runtime，避免依赖（可能已损坏的）主 runtime。
        // 5 秒上限：客户端 read_timeout 30 秒，黑洞后端会让崩溃中的进程僵住半分钟。
        let _ = std::thread::spawn(move || {
            block_on_new_runtime(async {
                let _ = tokio::time::timeout(Duration::from_secs(5), post_envelope(body)).await;
            })
        })
        .join();
        return;
    }
    if is_pipe_mode() {
        pipe_send(json!({ "envelope": body }), "envelope");
        return;
    }
    INFLIGHT.fetch_add(1, Ordering::SeqCst);
    let send = async move {
        post_envelope(body).await;
        INFLIGHT.fetch_sub(1, Ordering::SeqCst);
    };
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(send);
    } else {
        std::thread::spawn(move || block_on_new_runtime(send));
    }
}

fn block_on_new_runtime<F: std::future::Future>(fut: F) {
    if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        rt.block_on(fut);
    }
}

pub fn flush(timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    while INFLIGHT.load(Ordering::SeqCst) > 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `show_dialog`：silent 等无人值守场景为 false。
pub fn install_panic_hook(show_dialog: bool) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "Box<dyn Any>".to_string());
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".to_string());
        let event_id = new_uuid();
        // writeln! 而非 eprintln!：后者写失败会 panic，在 hook 里意味着双重 panic
        {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr(),
                "kachina-installer crashed at {location}: {message}\ncrash report id: {event_id}"
            );
        }
        if show_dialog {
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::process::Command::new(exe)
                    .args(["crash-dialog", &event_id])
                    .spawn();
            }
        }
        capture_panic(&event_id, format!("panic at {location}: {message}"));
        prev(info);
    }));
}

// ---- tracing 过滤与面包屑 ----

pub struct InfoFilter {}

impl<S> tracing_subscriber::layer::Filter<S> for InfoFilter {
    fn enabled(
        &self,
        metadata: &tracing::Metadata<'_>,
        _: &tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        metadata.level() <= &tracing::Level::INFO
    }
}

pub struct BreadcrumbLayer;

struct MessageVisitor {
    message: String,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for BreadcrumbLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = *event.metadata().level();
        if level > tracing::Level::INFO {
            return;
        }
        let mut visitor = MessageVisitor {
            message: String::new(),
        };
        event.record(&mut visitor);
        let level_str = match level {
            tracing::Level::ERROR => "error",
            tracing::Level::WARN => "warning",
            _ => "info",
        };
        add_breadcrumb(event.metadata().target(), level_str, visitor.message);
    }
}

// ---- 会话级性能事务 ----

/// 一次安装会话一个 transaction，阶段为一层扁平子 span（parent 一律指向根）。
pub struct Transaction {
    name: String,
    op: String,
    trace_id: String,
    span_id: String,
    start: f64,
    spans: Mutex<Vec<Value>>,
    measurements: Mutex<serde_json::Map<String, Value>>,
}

impl Transaction {
    pub fn start(name: &str, op: &str) -> Self {
        Self {
            name: name.to_string(),
            op: op.to_string(),
            trace_id: new_uuid(),
            span_id: new_span_id(),
            start: now_f64(),
            spans: Mutex::new(Vec::new()),
            measurements: Mutex::new(serde_json::Map::new()),
        }
    }

    pub fn span(&self, op: &str, start: f64, end: f64) {
        let span = json!({
            "span_id": new_span_id(),
            "parent_span_id": self.span_id,
            "trace_id": self.trace_id,
            "op": op,
            "start_timestamp": start,
            "timestamp": end,
        });
        if let Ok(mut spans) = self.spans.lock() {
            spans.push(span);
        }
    }

    pub async fn timed<T, F: std::future::Future<Output = T>>(&self, op: &str, fut: F) -> T {
        let start = now_f64();
        let out = fut.await;
        self.span(op, start, now_f64());
        out
    }

    pub fn set_measurement(&self, name: &str, value: f64, unit: &str) {
        if let Ok(mut m) = self.measurements.lock() {
            m.insert(name.to_string(), json!({ "value": value, "unit": unit }));
        }
    }

    /// `status` 取 Sentry span status（"ok"/"cancelled"/"internal_error"）。
    pub fn finish(self, status: &str) {
        let event_id = new_uuid();
        let mut event = base_event(&event_id);
        event["type"] = json!("transaction");
        event["transaction"] = json!(self.name);
        event["start_timestamp"] = json!(self.start);
        event["contexts"]["trace"] = json!({
            "trace_id": self.trace_id,
            "span_id": self.span_id,
            "op": self.op,
            "status": status,
        });
        event["spans"] = Value::Array(self.spans.into_inner().unwrap_or_default());
        event["measurements"] = Value::Object(self.measurements.into_inner().unwrap_or_default());
        dispatch(envelope(&event_id, "transaction", &event), false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_has_three_lines_and_valid_headers() {
        let event = json!({ "event_id": "abc", "level": "error" });
        let body = envelope("abc", "event", &event);
        let lines: Vec<&str> = body.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 3);
        let header: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(header["event_id"], "abc");
        assert_eq!(header["dsn"], DSN);
        let item: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(item["type"], "event");
        assert_eq!(item["length"].as_u64().unwrap() as usize, lines[2].len());
    }

    #[test]
    fn exception_chain_order_root_cause_first() {
        let err = anyhow::anyhow!("root cause").context("mid").context("outer");
        let values: Vec<String> = err.chain().map(|c| c.to_string()).collect();
        // anyhow chain 自外向内；capture_anyhow reverse 后 root cause 在前
        assert_eq!(values, vec!["outer", "mid", "root cause"]);
    }

    #[test]
    fn rfc3339_known_values() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        // 2026-01-01T00:00:00Z
        assert_eq!(rfc3339(1_767_225_600), "2026-01-01T00:00:00Z");
        // 闰年边界 2024-02-29T12:34:56Z
        assert_eq!(rfc3339(1_709_210_096), "2024-02-29T12:34:56Z");
    }

    #[test]
    fn breadcrumb_ring_buffer_caps() {
        for i in 0..(MAX_BREADCRUMBS + 10) {
            add_breadcrumb("test", "info", format!("crumb {i}"));
        }
        let crumbs = STATE.breadcrumbs.lock().unwrap();
        assert_eq!(crumbs.len(), MAX_BREADCRUMBS);
        assert_eq!(crumbs.back().unwrap()["message"], format!("crumb {}", MAX_BREADCRUMBS + 9));
    }
}
