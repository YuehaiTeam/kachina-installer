use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{oneshot, Mutex};

use crate::host::HostHandle;
use crate::session::state::{Phase, Prompt, UiState};
use crate::session::types::PluginEvent;
use crate::utils::code::{log_line, Coded};

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
            Phase::Failed(c) => {
                tracing::error!("{}", log_line(&anyhow::Error::from(c.clone())));
            }
            _ => {}
        }
    }

    async fn confirm(&self, _prompt: Prompt) -> bool {
        true
    }

    fn notify(&self, coded: &Coded) {
        tracing::error!("{}", log_line(&anyhow::Error::from(coded.clone())));
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
            let coded = Coded::bare(crate::utils::code::PLUGIN_FAILED);
            // The plugin's own message is the raw detail.
            return Err(match reply.error.filter(|s| !s.is_empty()) {
                Some(msg) => coded.wrap(anyhow::anyhow!(msg)),
                None => anyhow::Error::from(coded),
            });
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
    session: Arc<std::sync::Mutex<crate::session::state::UiSession>>,
}

impl GuiUi {
    pub fn new(
        window: HostHandle,
        hub: Arc<PromptHub>,
        plugins: Arc<PluginHub>,
        auto_answer: bool,
        session: Arc<std::sync::Mutex<crate::session::state::UiSession>>,
    ) -> Self {
        Self {
            window,
            hub,
            plugins,
            auto_answer,
            session,
        }
    }
}

#[async_trait]
impl SessionUi for GuiUi {
    /// The GUI session object stays authoritative (it is what `window_show`
    /// re-emits and what `confirm` decorates with `pending`), so only `phase`
    /// is taken from the running session's copy.
    fn state(&self, state: &UiState) {
        let mut sess = self.session.lock().unwrap_or_else(|e| e.into_inner());
        sess.state.phase = state.phase.clone();
        let snap = sess.state.clone();
        drop(sess);
        self.window.emit("ui-state", &snap);
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
        {
            let mut sess = self.session.lock().unwrap_or_else(|e| e.into_inner());
            sess.state.pending = Some(prompt);
            self.window.emit("ui-state", &sess.state);
        }
        let ok = rx.await.unwrap_or(false);
        {
            let mut sess = self.session.lock().unwrap_or_else(|e| e.into_inner());
            sess.state.pending = None;
            self.window.emit("ui-state", &sess.state);
        }
        ok
    }

    fn notify(&self, coded: &Coded) {
        self.window.emit("ui-notice", coded);
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
