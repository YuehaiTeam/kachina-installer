use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{oneshot, Mutex};

use crate::host::HostHandle;

use super::types::{PluginEvent, ProgressEvent, PromptEvent};

#[derive(Debug, Clone, Copy)]
pub enum PromptKind {
    ProcessRunning,
    OccupiedFiles,
    VersionMismatch,
}

impl PromptKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProcessRunning => "process_running",
            Self::OccupiedFiles => "occupied_files",
            Self::VersionMismatch => "version_mismatch",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PluginArgs {
    pub method: String,
    pub name: String,
    pub url: String,
    pub range: Option<String>,
    pub diffchunks: Option<Vec<String>>,
    pub insights: Option<Value>,
}

pub enum PluginResult {
    /// 插件回复的原始 JSON 文本。不解成 `Value`：消费端各自 `from_str` 成自己的
    /// 类型，反序列化按 Deserializer 类型单态化，经 Value 中转会为每个目标类型
    /// 多留一整棵副本（`Dfs2Data` 里的 `RepoMetadata` 就是一棵 15KiB 的）。
    Value(String),
    Unimplemented,
}

#[async_trait]
pub trait PluginHost: Send + Sync {
    async fn call(&self, args: PluginArgs) -> anyhow::Result<PluginResult>;
}

#[async_trait]
pub trait SessionUi: Send + Sync {
    async fn confirm(&self, kind: PromptKind, title: &str, message: &str) -> bool;
    fn progress(&self, event: ProgressEvent);
    async fn alert(&self, title: &str, message: &str) {
        tracing::error!("{title}: {message}");
    }
    fn insight(&self, _url: &str, event: &str, data: Option<Value>) {
        tracing::info!("insight {event} {data:?}");
    }
    fn reopen_source(&self) {}
    fn plugin_host(&self) -> Option<Arc<dyn PluginHost>> {
        None
    }
}

pub struct SilentUi;

pub struct SilentPluginUi {
    inner: SilentUi,
    host: Arc<dyn PluginHost>,
}

impl SilentPluginUi {
    pub fn new(window: HostHandle, plugins: Arc<PluginHub>) -> Self {
        Self {
            inner: SilentUi,
            host: Arc::new(GuiPluginHost {
                window,
                hub: plugins,
            }),
        }
    }
}

#[async_trait]
impl SessionUi for SilentPluginUi {
    async fn confirm(&self, kind: PromptKind, title: &str, message: &str) -> bool {
        self.inner.confirm(kind, title, message).await
    }
    fn progress(&self, event: ProgressEvent) {
        self.inner.progress(event);
    }
    fn insight(&self, url: &str, event: &str, data: Option<Value>) {
        self.inner.insight(url, event, data);
    }
    fn plugin_host(&self) -> Option<Arc<dyn PluginHost>> {
        Some(self.host.clone())
    }
}

#[async_trait]
impl SessionUi for SilentUi {
    async fn confirm(&self, _kind: PromptKind, _title: &str, _message: &str) -> bool {
        true
    }
    fn progress(&self, event: ProgressEvent) {
        tracing::debug!(
            "progress sub={} percent={:.1} {}",
            event.sub_step,
            event.percent,
            event.current.replace('\n', " ")
        );
    }
    fn insight(&self, url: &str, event: &str, data: Option<Value>) {
        let url = url.to_string();
        let event = event.to_string();
        tokio::spawn(async move {
            send_ev_insight(&url, &event, data).await;
        });
    }
}

fn encode_uri(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b';'
            | b','
            | b'/'
            | b'?'
            | b':'
            | b'@'
            | b'&'
            | b'='
            | b'+'
            | b'$'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')'
            | b'#' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub async fn send_ev_insight(url: &str, event: &str, data: Option<Value>) {
    let body = serde_json::json!({
        "type": "event",
        "payload": {
            "website": "16d32274-7313-4db6-80d3-340ce9db7689",
            "url": encode_uri(url),
            "name": event,
            "data": data,
        }
    });
    if let Err(err) = crate::REQUEST_CLIENT
        .post("https://77.cocogoat.cn/ev")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
    {
        tracing::debug!("insight failed: {err}");
    }
}

#[derive(Default)]
pub struct PromptHub {
    pending: Mutex<HashMap<String, oneshot::Sender<bool>>>,
}

impl PromptHub {
    pub async fn wait(&self, id: String) -> bool {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        rx.await.unwrap_or(false)
    }

    pub async fn answer(&self, id: &str, accept: bool) -> bool {
        if let Some(tx) = self.pending.lock().await.remove(id) {
            let _ = tx.send(accept);
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PluginAnswer {
    pub id: String,
    pub ok: bool,
    pub data: Option<String>,
    pub error: Option<String>,
    #[serde(default)]
    pub unimplemented: bool,
}

#[derive(Default)]
pub struct PluginHub {
    pending: Mutex<HashMap<String, oneshot::Sender<PluginAnswer>>>,
}

impl PluginHub {
    pub async fn wait(&self, id: String) -> PluginAnswer {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        rx.await.unwrap_or(PluginAnswer {
            id: String::new(),
            ok: false,
            data: None,
            error: Some("插件无响应".to_string()),
            unimplemented: false,
        })
    }

    pub async fn answer(&self, reply: PluginAnswer) -> bool {
        if let Some(tx) = self.pending.lock().await.remove(&reply.id) {
            let _ = tx.send(reply);
            true
        } else {
            false
        }
    }
}

struct GuiPluginHost {
    window: HostHandle,
    hub: Arc<PluginHub>,
}

#[async_trait]
impl PluginHost for GuiPluginHost {
    async fn call(&self, args: PluginArgs) -> anyhow::Result<PluginResult> {
        let id = uuid::Uuid::new_v4().to_string();
        self.window.emit(
            "session-plugin",
            PluginEvent {
                id: id.clone(),
                method: args.method,
                name: args.name,
                url: args.url,
                range: args.range,
                diffchunks: args.diffchunks,
                insights: args.insights,
            },
        );
        let reply = self.hub.wait(id).await;
        if reply.unimplemented {
            return Ok(PluginResult::Unimplemented);
        }
        if !reply.ok {
            let msg = reply
                .error
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "插件执行失败".to_string());
            return Err(crate::session::error::user(msg));
        }
        Ok(PluginResult::Value(
            reply.data.unwrap_or_else(|| "null".to_string()),
        ))
    }
}

pub struct GuiUi {
    window: HostHandle,
    hub: Arc<PromptHub>,
    plugins: Arc<PluginHub>,
    auto_answer: bool,
}

impl GuiUi {
    pub fn new(
        window: HostHandle,
        hub: Arc<PromptHub>,
        plugins: Arc<PluginHub>,
        auto_answer: bool,
    ) -> Self {
        Self {
            window,
            hub,
            plugins,
            auto_answer,
        }
    }
}

#[async_trait]
impl SessionUi for GuiUi {
    async fn confirm(&self, kind: PromptKind, title: &str, message: &str) -> bool {
        if self.auto_answer {
            return true;
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.window.emit(
            "session-prompt",
            PromptEvent {
                id: id.clone(),
                kind: kind.as_str().to_string(),
                title: title.to_string(),
                message: message.to_string(),
            },
        );
        self.hub.wait(id).await
    }

    fn progress(&self, event: ProgressEvent) {
        self.window.emit("session-progress", event);
    }

    async fn alert(&self, title: &str, message: &str) {
        rfd::MessageDialog::new()
            .set_title(title)
            .set_description(message)
            .set_level(rfd::MessageLevel::Error)
            .set_parent(&self.window.parent())
            .show();
    }

    fn insight(&self, _url: &str, event: &str, data: Option<Value>) {
        self.window.emit(
            "session-insight",
            serde_json::json!({
                "event": event,
                "data": data,
            }),
        );
    }

    fn reopen_source(&self) {
        self.window.emit("session-reopen-source", ());
    }

    fn plugin_host(&self) -> Option<Arc<dyn PluginHost>> {
        Some(Arc::new(GuiPluginHost {
            window: self.window.clone(),
            hub: self.plugins.clone(),
        }))
    }
}
