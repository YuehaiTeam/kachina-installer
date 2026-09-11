//! Reqwest middleware that tunnels HTTP requests through SSH port-forwarding.
//!
//! URL format (all SSH credentials live in the **fragment** because reqwest
//! strips URL userinfo):
//!
//! ```text
//! ssh+http://ssh_host:ssh_port/http_path?http_query
//!     #user=<ssh_user>&pass=<ssh_pass>&fingerprint=<hex SHA-256>
//!      &internal_host=<tunnel target host, default "tunnel">
//!      &internal_port=<tunnel target port, default 80>
//! ```
//!
//! 连接模型：每请求一条 SSH 连接，不复用（见 docs/notes SSH/SFTP 栈整体瘦身）。
//! 弱网对抗依赖 TCP 多连接，复用与之相悖；连接随响应流结束而关闭。

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use reqwest_middleware::{Middleware, Next};
use tokio::task::AbortHandle;
use tracing::{debug, warn};

// ====================================================================
// SSH Handler — host key fingerprint verification
// ====================================================================

pub(crate) struct SshHandler {
    expected_fingerprint: String, // hex-encoded SHA-256, always required
}

impl russh::client::Handler for SshHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        let fp = server_public_key.fingerprint(russh::keys::HashAlg::Sha256);
        let actual_hex: String = fp.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
        let expected_norm = normalize_hex(&self.expected_fingerprint);

        let ok = actual_hex == expected_norm;
        if !ok {
            warn!(
                expected = %self.expected_fingerprint,
                actual_hex = %actual_hex,
                "SSH host key fingerprint mismatch"
            );
        }
        Ok(ok)
    }
}

/// Strip non-hex characters (colons, spaces, …) and lowercase.
pub(crate) fn normalize_hex(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_lowercase()
}

// ====================================================================
// URL parsing
// ====================================================================

pub(crate) struct SshUrlParts {
    pub(crate) ssh_user: String,
    pub(crate) ssh_pass: String,
    pub(crate) ssh_host: String,
    pub(crate) ssh_port: u16,
    pub(crate) fingerprint: String, // required
    pub(crate) internal_host: String,
    pub(crate) internal_port: u16,
    /// e.g. "/api/v1?foo=bar"
    pub(crate) http_path_and_query: String,
}

impl SshUrlParts {
    /// HTTP `Host` header value.
    fn http_host_header(&self) -> String {
        let host_part = if self.internal_host.contains(':') {
            format!("[{}]", self.internal_host)
        } else {
            self.internal_host.clone()
        };
        if self.internal_port == 80 {
            host_part
        } else {
            format!("{}:{}", host_part, self.internal_port)
        }
    }

    /// Human-readable target for error messages.
    fn ssh_target(&self) -> String {
        format!("{}@{}:{}", self.ssh_user, self.ssh_host, self.ssh_port)
    }
}

fn parse_ssh_url(url: &reqwest::Url) -> anyhow::Result<SshUrlParts> {
    anyhow::ensure!(
        url.scheme() == "ssh+http",
        "expected scheme 'ssh+http', got '{}'",
        url.scheme()
    );

    let raw_host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("missing SSH host in URL"))?;
    // Strip IPv6 brackets: url crate returns "[::1]" for non-special schemes
    let ssh_host = raw_host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(raw_host)
        .to_string();
    let ssh_port = url.port().unwrap_or(22);
    anyhow::ensure!(ssh_port > 0, "invalid ssh_port 0 in URL");

    let http_path_and_query = match url.query() {
        Some(q) => format!("{}?{}", url.path(), q),
        None => url.path().to_string(),
    };

    // Fragment: user=xxx&pass=xxx&fingerprint=xxx&internal_host=xxx&internal_port=xxx
    let frag_str = url.fragment().unwrap_or("");
    let mut frag: HashMap<&str, String> = HashMap::new();
    for pair in frag_str.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            frag.insert(k, percent_decode(v)?);
        }
    }

    // user is REQUIRED (in fragment, not URL userinfo — reqwest strips userinfo)
    let ssh_user = frag
        .get("user")
        .filter(|s| !s.is_empty())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing required 'user' in URL fragment"))?;

    let ssh_pass = frag.get("pass").cloned().unwrap_or_default();

    // fingerprint is REQUIRED
    let fingerprint = frag
        .get("fingerprint")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "missing required 'fingerprint' in URL fragment for {}:{}",
                ssh_host,
                ssh_port
            )
        })?;
    let fp_norm = normalize_hex(fingerprint);
    anyhow::ensure!(
        fp_norm.len() == 64,
        "fingerprint must be 64 hex chars (SHA-256), got {} chars from '{}'",
        fp_norm.len(),
        fingerprint
    );

    let internal_host = frag
        .get("internal_host")
        .filter(|s| !s.is_empty())
        .cloned()
        .unwrap_or_else(|| "tunnel".to_string());

    let internal_port: u16 = match frag.get("internal_port") {
        Some(s) => s
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid internal_port '{s}' in URL fragment"))?,
        None => 80,
    };
    anyhow::ensure!(internal_port > 0, "invalid internal_port 0 in URL fragment");

    Ok(SshUrlParts {
        ssh_user,
        ssh_pass,
        ssh_host,
        ssh_port,
        fingerprint: fp_norm,
        internal_host,
        internal_port,
        http_path_and_query,
    })
}

/// Minimal percent-decoding for URL fragment components.
pub(crate) fn percent_decode(input: &str) -> anyhow::Result<String> {
    let mut bytes = Vec::with_capacity(input.len());
    let src = input.as_bytes();
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'%' && i + 2 < src.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&src[i + 1..i + 3]).unwrap_or(""), 16)
            {
                bytes.push(byte);
                i += 3;
                continue;
            }
        }
        bytes.push(src[i]);
        i += 1;
    }
    String::from_utf8(bytes)
        .map_err(|e| anyhow::anyhow!("URL percent-decoded value is not valid UTF-8: {e}"))
}

// ====================================================================
// SSH connect
// ====================================================================

const SSH_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const SSH_AUTH_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const SSH_CHANNEL_TIMEOUT: Duration = Duration::from_secs(15);
const HTTP_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const HTTP_SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// Hop-by-hop headers that MUST NOT be forwarded through the tunnel.
const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// RAII guard — aborts a spawned task on drop.
struct AbortOnDrop(AbortHandle);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) async fn ssh_connect(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    fingerprint: &str,
) -> anyhow::Result<russh::client::Handle<SshHandler>> {
    let config = Arc::new(russh::client::Config::default());
    let handler = SshHandler {
        expected_fingerprint: fingerprint.to_string(),
    };

    debug!(host, port, user, "SSH connecting");
    let mut session = tokio::time::timeout(
        SSH_CONNECT_TIMEOUT,
        russh::client::connect(config, (host, port), handler),
    )
    .await
    .map_err(|_| anyhow::anyhow!("SSH connect timeout to {user}@{host}:{port}"))??;

    let auth_fut = async {
        if pass.is_empty() {
            anyhow::ensure!(
                session.authenticate_none(user).await?.success(),
                "SSH none-auth failed for {user}@{host}:{port}"
            );
        } else {
            anyhow::ensure!(
                session.authenticate_password(user, pass).await?.success(),
                "SSH password-auth failed for {user}@{host}:{port}"
            );
        }
        Ok::<_, anyhow::Error>(())
    };
    tokio::time::timeout(SSH_AUTH_TIMEOUT, auth_fut)
        .await
        .map_err(|_| anyhow::anyhow!("SSH auth timeout for {user}@{host}:{port}"))??;

    debug!(host, port, user, "SSH authenticated");
    Ok(session)
}

/// 冷路径错误构造：格式化在函数体内完成，调用点只生成 `format_args!`，
/// 避免每个错误分支在巨型 async 状态机里各自展开一套 fmt 机器。
#[cold]
#[inline(never)]
pub(crate) fn mw_err_fmt(args: std::fmt::Arguments<'_>) -> reqwest_middleware::Error {
    reqwest_middleware::Error::Middleware(anyhow::anyhow!("{args}"))
}

// ====================================================================
// SshMiddleware
// ====================================================================

pub struct SshMiddleware;

impl SshMiddleware {
    async fn ssh_request(
        &self,
        req: reqwest::Request,
    ) -> Result<reqwest::Response, reqwest_middleware::Error> {
        let parts = parse_ssh_url(req.url()).map_err(reqwest_middleware::Error::Middleware)?;
        let method = req.method().clone();
        let req_headers = req.headers().clone();

        let body_bytes: Bytes = match req.body() {
            None => Bytes::new(),
            Some(b) => match b.as_bytes() {
                Some(slice) => Bytes::copy_from_slice(slice),
                None => {
                    return Err(mw_err_fmt(format_args!(
                        "SSH tunnel to {} does not support streaming request bodies",
                        parts.ssh_target()
                    )));
                }
            },
        };

        // 1. Fresh SSH connection + direct-tcpip channel
        let session_handle = ssh_connect(
            &parts.ssh_host,
            parts.ssh_port,
            &parts.ssh_user,
            &parts.ssh_pass,
            &parts.fingerprint,
        )
        .await
        .map_err(|e| {
            mw_err_fmt(format_args!(
                "SSH connect to {} failed: {e}",
                parts.ssh_target()
            ))
        })?;

        let channel = tokio::time::timeout(
            SSH_CHANNEL_TIMEOUT,
            session_handle.channel_open_direct_tcpip(
                parts.internal_host.as_str(),
                parts.internal_port as u32,
                "127.0.0.1",
                0u32,
            ),
        )
        .await
        .map_err(|_| {
            mw_err_fmt(format_args!(
                "SSH channel to {}:{} via {} timed out",
                parts.internal_host,
                parts.internal_port,
                parts.ssh_target()
            ))
        })?
        .map_err(|e| {
            mw_err_fmt(format_args!(
                "SSH channel to {}:{} via {} failed: {e}",
                parts.internal_host,
                parts.internal_port,
                parts.ssh_target()
            ))
        })?;
        let io = TokioIo::new(channel.into_stream());

        // 2. HTTP/1.1 handshake
        let (mut sender, conn) = tokio::time::timeout(
            HTTP_HANDSHAKE_TIMEOUT,
            hyper::client::conn::http1::handshake(io),
        )
        .await
        .map_err(|_| {
            mw_err_fmt(format_args!(
                "HTTP/1.1 handshake timeout over SSH to {}",
                parts.ssh_target()
            ))
        })?
        .map_err(|e| {
            mw_err_fmt(format_args!(
                "HTTP/1.1 handshake over SSH to {} failed: {e}",
                parts.ssh_target()
            ))
        })?;

        let conn_task = tokio::spawn(async move {
            if let Err(e) = conn.await {
                debug!(err = %e, "SSH HTTP connection driver finished");
            }
        });
        let abort_guard = AbortOnDrop(conn_task.abort_handle());

        // 3. Build the HTTP request
        let host_hdr = parts.http_host_header();
        let mut builder = http::Request::builder()
            .method(method.as_str())
            .uri(&parts.http_path_and_query)
            .header("host", &host_hdr);

        for (name, value) in req_headers.iter() {
            let lc = name.as_str();
            if lc == "host" || HOP_BY_HOP_HEADERS.contains(&lc) {
                continue;
            }
            builder = builder.header(name, value);
        }

        let hyper_req = builder
            .body(http_body_util::Full::new(body_bytes))
            .map_err(|e| mw_err_fmt(format_args!("failed to build HTTP request: {e}")))?;

        // 4. Send request
        let hyper_resp = tokio::time::timeout(HTTP_SEND_TIMEOUT, sender.send_request(hyper_req))
            .await
            .map_err(|_| {
                mw_err_fmt(format_args!(
                    "HTTP send timeout over SSH to {}{}",
                    parts.ssh_target(),
                    parts.http_path_and_query
                ))
            })?
            .map_err(|e| {
                mw_err_fmt(format_args!(
                    "HTTP send over SSH to {}{} failed: {e}",
                    parts.ssh_target(),
                    parts.http_path_and_query
                ))
            })?;

        let (resp_parts, body) = hyper_resp.into_parts();

        // 5. Stream response body — captures keep the SSH session and conn
        //    driver alive until the body is fully consumed or dropped; the
        //    connection dies with the stream.
        let byte_stream: Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>> =
            Box::pin(async_stream::try_stream! {
                let _abort = abort_guard;
                let _session = session_handle;

                let mut body = body;
                while let Some(frame_result) = body.frame().await {
                    let frame = frame_result.map_err(|e| {
                        std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string())
                    })?;
                    if let Ok(data) = frame.into_data() {
                        if !data.is_empty() {
                            yield data;
                        }
                    }
                }
            });

        // 6. Convert to reqwest::Response
        let mut resp_builder = http::Response::builder().status(resp_parts.status);
        for (name, value) in &resp_parts.headers {
            resp_builder = resp_builder.header(name, value);
        }
        let http_resp = resp_builder
            .body(reqwest::Body::wrap_stream(byte_stream))
            .map_err(|e| mw_err_fmt(format_args!("failed to build response: {e}")))?;

        Ok(http_resp.into())
    }
}

#[async_trait]
impl Middleware for SshMiddleware {
    async fn handle(
        &self,
        req: reqwest::Request,
        extensions: &mut http::Extensions,
        next: Next<'_>,
    ) -> Result<reqwest::Response, reqwest_middleware::Error> {
        if req.url().scheme() == "ssh+http" {
            self.ssh_request(req).await
        } else {
            next.run(req, extensions).await
        }
    }
}
