//! H3 (HTTP/3 over QUIC) capability management for kachina-installer.
//!
//! This module provides:
//! - `init()` — Startup probe that checks H3 availability (Win11+, no proxy, MsQuic OK)
//! - `is_h3_available()` / `disable_h3()` — Runtime H3 state management
//! - `DynamicUaMiddleware` — Injects User-Agent with h3/enabled when available
//! - `H3FallbackMiddleware` — Intercepts http3:// URLs, falls back on failure

pub(crate) mod h3;
pub(crate) mod sftp;
pub(crate) mod ssh;

use self::h3::H3Middleware;
use async_trait::async_trait;
use http::Extensions;
use reqwest::{Request, Response};
use reqwest_middleware::{Middleware, Next, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

// ══════════════════════════════════════════════════════════════════════════════
// Global H3 availability state
// ══════════════════════════════════════════════════════════════════════════════

static H3_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Returns whether H3 is currently available for this session.
pub fn is_h3_available() -> bool {
    H3_AVAILABLE.load(Ordering::Relaxed)
}

/// Permanently disables H3 for this session (idempotent).
/// Called on first H3 connection failure.
pub fn disable_h3() {
    H3_AVAILABLE.store(false, Ordering::Relaxed);
    tracing::warn!("[H3] Disabled for this session");
}

// ══════════════════════════════════════════════════════════════════════════════
// Startup probe
// ══════════════════════════════════════════════════════════════════════════════

/// Probes H3 support at startup. Returns whether H3 is available.
/// Sets `H3_AVAILABLE` internally.
pub fn init() -> bool {
    let ok = probe_h3_support();
    H3_AVAILABLE.store(ok, Ordering::Relaxed);
    ok
}

fn probe_h3_support() -> bool {
    // 1. Win11+ check (MsQuic QUIC with Schannel requires build >= 22000)
    let (major, minor, build) = nt_version::get();
    let build_num = build & 0xffff;
    if !(major == 10 && minor == 0 && build_num >= 22000) {
        tracing::info!(
            "[H3] Not Win11 (build={}, need 22000+), disabled",
            build_num
        );
        return false;
    }

    // 2. System proxy check — H3 doesn't work through proxies
    if has_system_proxy() {
        tracing::info!("[H3] System proxy detected, disabled");
        return false;
    }

    // 3. MsQuic availability probe — create Registration to verify DLL + Schannel
    use h3_msquic_async::msquic_async::msquic;
    match msquic::Registration::new(&msquic::RegistrationConfig::default()) {
        Ok(_reg) => {
            tracing::info!("[H3] Probe succeeded, enabled");
            true
        }
        Err(e) => {
            tracing::info!("[H3] Probe failed: {:?}, disabled", e);
            false
        }
    }
}

fn has_system_proxy() -> bool {
    windows_registry::CURRENT_USER
        .open(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
        .ok()
        .and_then(|k| k.get_u32("ProxyEnable").ok())
        .map(|v| v != 0)
        .unwrap_or(false)
}

// ══════════════════════════════════════════════════════════════════════════════
// Dynamic User-Agent middleware
// ══════════════════════════════════════════════════════════════════════════════

/// Middleware that injects a dynamic User-Agent header.
/// Includes "h3/enabled" when H3 is available, so the control plane knows.
pub struct DynamicUaMiddleware;

impl Default for DynamicUaMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl DynamicUaMiddleware {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Middleware for DynamicUaMiddleware {
    async fn handle(
        &self,
        mut req: Request,
        ext: &mut Extensions,
        next: Next<'_>,
    ) -> Result<Response> {
        let ua = ua_string();
        if let Ok(value) = http::HeaderValue::from_str(&ua) {
            req.headers_mut().insert(http::header::USER_AGENT, value);
        }
        next.run(req, ext).await
    }
}

/// Generates the User-Agent string with optional h3/enabled suffix.
pub fn ua_string() -> String {
    let (major, minor, build) = nt_version::get();
    let cpu_cores = num_cpus::get();
    let wv2ver = tauri::webview_version().unwrap_or_else(|_| "Unknown".to_string());

    let mut ua = format!(
        "KachinaInstaller/{} Webview2/{} Windows/{}.{}.{} Threads/{}",
        env!("CARGO_PKG_VERSION"),
        wv2ver,
        major,
        minor,
        build & 0xffff,
        cpu_cores
    );

    ua.push_str(" ssh/enabled");
    ua.push_str(" sftp/enabled");

    if is_h3_available() {
        ua.push_str(" h3/enabled");
    }

    ua
}

// ══════════════════════════════════════════════════════════════════════════════
// H3 Fallback middleware
// ══════════════════════════════════════════════════════════════════════════════

/// Middleware that intercepts `http3://` URLs and routes them through H3Middleware.
/// On any H3 failure, permanently disables H3 for this session and returns the error.
pub struct H3FallbackMiddleware {
    h3: H3Middleware,
}

impl H3FallbackMiddleware {
    pub fn new(idle_timeout: Duration) -> anyhow::Result<Self> {
        let h3 = H3Middleware::new(idle_timeout)?;
        Ok(Self { h3 })
    }

    /// Get a reference to the inner H3Middleware (for shutdown, discover, etc.)
    pub fn inner(&self) -> &H3Middleware {
        &self.h3
    }
}

#[async_trait]
impl Middleware for H3FallbackMiddleware {
    async fn handle(&self, req: Request, ext: &mut Extensions, next: Next<'_>) -> Result<Response> {
        // Only intercept http3:// scheme
        if req.url().scheme() != "http3" {
            return next.run(req, ext).await;
        }

        // Attempt H3 request
        match self.h3.h3_request(req).await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                // First H3 failure → disable for this session
                disable_h3();
                Err(e)
            }
        }
    }
}
