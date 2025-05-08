use std::sync::{atomic::AtomicBool, Arc};

use sentry::{ClientOptions, Transport};
use tokio::sync::RwLock;

use crate::REQUEST_CLIENT;

pub enum SentryData {
    Breadcrumb(sentry::Breadcrumb),
    Envelope(sentry::Envelope),
}

pub struct AutoTransportFactory {
    use_mpsc: AtomicBool,
    pub mpsc_rx: Arc<RwLock<tokio::sync::mpsc::Receiver<SentryData>>>,
    mpsc_tx: Arc<tokio::sync::mpsc::Sender<SentryData>>,
}
impl Default for AutoTransportFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoTransportFactory {
    pub fn new() -> Self {
        let (mpsc_tx, mpsc_rx) = tokio::sync::mpsc::channel(10);
        Self {
            use_mpsc: AtomicBool::new(false),
            mpsc_rx: Arc::new(RwLock::new(mpsc_rx)),
            mpsc_tx: Arc::new(mpsc_tx),
        }
    }

    pub fn set_use_mpsc(&self, use_mpsc: bool) {
        self.use_mpsc
            .store(use_mpsc, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn get_mpsc_tx(&self) -> Arc<tokio::sync::mpsc::Sender<SentryData>> {
        self.mpsc_tx.clone()
    }
}

pub struct MpscTransport {
    mpsc_tx: Arc<tokio::sync::mpsc::Sender<SentryData>>,
}
impl Transport for MpscTransport {
    fn send_envelope(&self, envelope: sentry::Envelope) {
        // start a new thread to send the envelope
        let tx = self.mpsc_tx.clone();
        std::thread::spawn(move || {
            if let Err(e) = tx.blocking_send(SentryData::Envelope(envelope)) {
                tracing::warn!("Failed to send envelope: {}", e);
            }
        });
    }
}

impl sentry::TransportFactory for AutoTransportFactory {
    fn create_transport(&self, options: &ClientOptions) -> Arc<dyn sentry::Transport> {
        if self.use_mpsc.load(std::sync::atomic::Ordering::SeqCst) {
            let mpsc_tx = self.get_mpsc_tx();
            let transport = MpscTransport { mpsc_tx };
            return Arc::new(transport);
        }
        let transport =
            sentry::transports::ReqwestHttpTransport::with_client(options, REQUEST_CLIENT.clone());
        Arc::new(transport)
    }
}

lazy_static::lazy_static! {
    pub static ref AUTO_TRANSPORT: Arc<AutoTransportFactory> = Arc::new(AutoTransportFactory::new());
}

pub fn sentry_init(use_mpsc: bool) -> sentry::ClientInitGuard {
    AUTO_TRANSPORT.set_use_mpsc(use_mpsc);
    sentry::init((
        "http://f68ff71bf7fee106fb09fbae79031502@steambird.cocogoat.cn/insight/kachina-installer/0",
        sentry::ClientOptions {
            release: sentry::release_name!(),
            traces_sample_rate: 1.0,
            transport: Some(AUTO_TRANSPORT.clone()),
            before_breadcrumb: if use_mpsc {
                Some(Arc::new(|breadcrumb| {
                    let mpsc_tx = AUTO_TRANSPORT.get_mpsc_tx().clone();
                    // start a new thread to send the breadcrumb
                    std::thread::spawn(move || {
                        // send the breadcrumb to the mpsc channel
                        if let Err(e) = mpsc_tx.blocking_send(SentryData::Breadcrumb(breadcrumb)) {
                            tracing::warn!("Failed to send breadcrumb: {}", e);
                        }
                    });
                    None
                }))
            } else {
                None
            },
            ..sentry::ClientOptions::default()
        },
    ))
}

pub fn forward_envelope(envelope: sentry::Envelope) {
    if let Some(event) = envelope.event().cloned() {
        sentry::capture_event(event);
    } else {
        sentry::Hub::with_active(|hub| {
            let client = hub.client();
            if let Some(client) = client {
                client.send_envelope(envelope);
            }
        });
    }
}

pub fn forward_breadcrumb(breadcrumb: sentry::Breadcrumb) {
    tracing::info!("Forwarding breadcrumb: {:?}", breadcrumb);
    sentry::add_breadcrumb(breadcrumb);
}

pub fn sentry_set_info() {
    let wv2ver = tauri::webview_version();
    let wv2ver = if let Ok(ver) = wv2ver {
        ver
    } else {
        "Unknown".to_string()
    };
    sentry::configure_scope(|scope| {
        scope.set_context(
            "browser",
            sentry::protocol::Context::Browser(Box::new(sentry::protocol::BrowserContext {
                name: Some("Webview2".to_string()),
                version: Some(wv2ver),
                ..Default::default()
            })),
        );
        scope.set_context(
            "app",
            sentry::protocol::Context::App(Box::new(sentry::protocol::AppContext {
                app_name: Some("KachinaInstaller".to_string()),
                app_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                build_type: Some(if cfg!(debug_assertions) {
                    "Debug".to_string()
                } else {
                    "Release".to_string()
                }),
                ..Default::default()
            })),
        );
        let did = crate::utils::get_device_id().ok();
        scope.set_user(Some(sentry::User {
            id: did,
            ip_address: Some(sentry::protocol::IpAddress::Auto),
            ..Default::default()
        }));
    });
}
