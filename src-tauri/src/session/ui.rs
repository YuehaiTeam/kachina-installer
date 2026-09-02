use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{oneshot, Mutex};

use crate::host::HostHandle;
use crate::session::state::{Phase, Progress, Prompt, UiState};
use crate::session::types::{PluginEvent, ProgressEvent, PromptEvent};
use crate::utils::code::{Coded, Extracted};
use crate::utils::i18n::{self, format_size};

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
    /// Plugin reply JSON text. Not decoded to `Value`: consumers `from_str`
    /// into their own type so deserialization monomorphizes on the Deserializer.
    Value(String),
    Unimplemented,
}

#[async_trait]
pub trait PluginHost: Send + Sync {
    async fn call(&self, args: PluginArgs) -> anyhow::Result<PluginResult>;
}

#[async_trait]
pub trait SessionUi: Send + Sync {
    fn state(&self, state: &UiState);
    async fn confirm(&self, prompt: Prompt) -> bool;
    fn notify(&self, coded: &Coded);
    fn plugin_host(&self) -> Option<Arc<dyn PluginHost>> {
        None
    }

    /// Step-2 intermediate: `run.rs` does not hold `UiSession` yet.
    fn progress(
        &self,
        sub_step: u32,
        percent: f64,
        stage: &'static str,
        subject: Option<&str>,
        done: Option<u64>,
        total: Option<u64>,
    ) {
        let mut state = UiState::default();
        state.phase = Phase::Running(Progress {
            sub_step,
            percent,
            stage,
            subject: subject.map(str::to_string),
            done,
            total,
        });
        self.state(&state);
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
    fn state(&self, state: &UiState) {
        self.inner.state(state);
    }
    async fn confirm(&self, prompt: Prompt) -> bool {
        self.inner.confirm(prompt).await
    }
    fn notify(&self, coded: &Coded) {
        self.inner.notify(coded);
    }
    fn plugin_host(&self) -> Option<Arc<dyn PluginHost>> {
        Some(self.host.clone())
    }
}

#[async_trait]
impl SessionUi for SilentUi {
    fn state(&self, state: &UiState) {
        match &state.phase {
            Phase::Running(p) => {
                tracing::debug!(
                    "progress sub={} percent={:.1} stage={} subject={:?} done={:?} total={:?}",
                    p.sub_step,
                    p.percent,
                    p.stage,
                    p.subject,
                    p.done,
                    p.total
                );
            }
            Phase::Failed(c) => match c.detail.as_deref().filter(|d| !d.is_empty()) {
                Some(d) => tracing::error!("{}: {d}", c.code),
                None => tracing::error!("{}", c.code),
            },
            _ => {}
        }
    }

    async fn confirm(&self, _prompt: Prompt) -> bool {
        true
    }

    fn notify(&self, coded: &Coded) {
        match coded.detail.as_deref().filter(|d| !d.is_empty()) {
            Some(d) => tracing::error!("{}: {d}", coded.code),
            None => tracing::error!("{}", coded.code),
        }
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

pub fn notice_text(coded: &Coded) -> (String, String) {
    let subject = coded.subject.clone().unwrap_or_default();
    let title = i18n::t("dialog.error", &[]);
    let body = i18n::t(coded.code, &[("subject", subject.as_str())]);
    let message = match coded.detail.as_deref().filter(|d| !d.is_empty()) {
        Some(d) => format!("{body}\n{d}"),
        None => body,
    };
    (title, message)
}

pub fn notice_from_error(err: &anyhow::Error) -> (String, String) {
    match crate::utils::code::extract(err) {
        Extracted::Coded(c) => notice_text(c),
        Extracted::Cancelled => (
            i18n::t("dialog.error", &[]),
            "cancelled".to_string(),
        ),
        Extracted::Uncoded { detail } => {
            let body = i18n::t(crate::utils::code::INTERNAL_ERROR, &[]);
            (
                i18n::t("dialog.error", &[]),
                if detail.is_empty() {
                    body
                } else {
                    format!("{body}\n{detail}")
                },
            )
        }
    }
}

pub(crate) fn progress_current(p: &Progress) -> String {
    let subject = p.subject.clone().unwrap_or_default();
    let done = p.done.map(format_size).unwrap_or_default();
    let total = p.total.map(format_size).unwrap_or_default();
    i18n::t(
        &format!("progress.{}", p.stage),
        &[
            ("subject", subject.as_str()),
            ("done", done.as_str()),
            ("total", total.as_str()),
        ],
    )
}

pub(crate) fn prompt_copy(prompt: &Prompt) -> (String, String) {
    let items = prompt.items.join("\n");
    let mut owned: Vec<(String, String)> = vec![("items".into(), items)];
    for (k, v) in &prompt.params {
        owned.push(((*k).to_string(), v.clone()));
    }
    let params: Vec<(&str, &str)> = owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    (
        i18n::t(&format!("prompt.{}.title", prompt.kind), &params),
        i18n::t(&format!("prompt.{}.message", prompt.kind), &params),
    )
}

#[derive(Default)]
pub struct PromptHub {
    pending: Mutex<HashMap<String, oneshot::Sender<bool>>>,
}

impl PromptHub {
    pub async fn register(&self, id: String) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        rx
    }

    pub async fn wait(&self, id: String) -> bool {
        self.register(id).await.await.unwrap_or(false)
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
    pub async fn register(&self, id: String) -> oneshot::Receiver<PluginAnswer> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        rx
    }

    pub async fn recv(&self, id: String, rx: oneshot::Receiver<PluginAnswer>) -> PluginAnswer {
        let failed = || PluginAnswer {
            id: String::new(),
            ok: false,
            data: None,
            error: None,
            unimplemented: false,
        };
        match tokio::time::timeout(Duration::from_secs(60), rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => failed(),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                failed()
            }
        }
    }

    pub async fn wait(&self, id: String) -> PluginAnswer {
        let rx = self.register(id.clone()).await;
        self.recv(id, rx).await
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
        let rx = self.hub.register(id.clone()).await;
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
        let reply = self.hub.recv(id, rx).await;
        if reply.unimplemented {
            return Ok(PluginResult::Unimplemented);
        }
        if !reply.ok {
            return Err(anyhow::Error::from(Coded::bare(crate::utils::code::PLUGIN_FAILED)));
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
    fn state(&self, state: &UiState) {
        if let Phase::Running(p) = &state.phase {
            self.window.emit(
                "session-progress",
                ProgressEvent {
                    sub_step: p.sub_step,
                    percent: p.percent,
                    current: progress_current(p),
                },
            );
        }
    }

    async fn confirm(&self, mut prompt: Prompt) -> bool {
        if self.auto_answer {
            return true;
        }
        if prompt.id.is_empty() {
            prompt.id = uuid::Uuid::new_v4().to_string();
        }
        let id = prompt.id.clone();
        let rx = self.hub.register(id.clone()).await;
        let (title, message) = prompt_copy(&prompt);
        self.window.emit(
            "session-prompt",
            PromptEvent {
                id,
                kind: prompt.kind.to_string(),
                title,
                message,
            },
        );
        rx.await.unwrap_or(false)
    }

    fn notify(&self, coded: &Coded) {
        let (title, message) = notice_text(coded);
        let parent = self.window.parent();
        let _ = tokio::task::spawn_blocking(move || {
            rfd::MessageDialog::new()
                .set_title(&title)
                .set_description(&message)
                .set_level(rfd::MessageLevel::Error)
                .set_parent(&parent)
                .show();
        });
    }

    fn plugin_host(&self) -> Option<Arc<dyn PluginHost>> {
        Some(Arc::new(GuiPluginHost {
            window: self.window.clone(),
            hub: self.plugins.clone(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn prompt_answer_before_await_is_not_lost() {
        let hub = PromptHub::default();
        let rx = hub.register("p1".into()).await;
        assert!(hub.answer("p1", true).await);
        assert_eq!(rx.await, Ok(true));
    }

    #[tokio::test]
    async fn plugin_answer_before_await_is_not_lost() {
        let hub = PluginHub::default();
        let rx = hub.register("g1".into()).await;
        let reply = PluginAnswer {
            id: "g1".into(),
            ok: true,
            data: Some("1".into()),
            error: None,
            unimplemented: false,
        };
        assert!(hub.answer(reply).await);
        let got = tokio::time::timeout(Duration::from_millis(200), rx)
            .await
            .expect("must not wait for plugin timeout")
            .expect("sender dropped");
        assert!(got.ok);
        assert_eq!(got.data.as_deref(), Some("1"));
    }
}
