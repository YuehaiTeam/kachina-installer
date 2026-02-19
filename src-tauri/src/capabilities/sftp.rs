// ====================================================================
// SFTP download middleware
//
// Intercepts `sftp://` URLs and serves file content as HTTP responses,
// reusing the shared SSH connection pool from SshMiddleware.
//
// URL format:
//   sftp://host:port/remote/path#user=xxx&pass=yyy&fingerprint=sha256hex
// ====================================================================

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use async_stream::try_stream;
use futures::TryStreamExt;
use bytes::Bytes;
use http::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use reqwest_middleware::{Middleware, Next};
use russh_sftp::client::SftpSession;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tracing::{debug, warn};

use super::ssh::{
    is_recoverable_ssh_error, mw_err, normalize_hex, percent_decode, ActiveStreamGuard, PoolKey,
    SshMiddleware, SshPoolInner, SshUrlParts,
};

// ====================================================================
// URL parsing
// ====================================================================

struct SftpUrlParts {
    user: String,
    pass: String,
    host: String,
    port: u16,
    fingerprint: String,
    remote_path: String,
}

impl SftpUrlParts {
    fn pool_key(&self) -> PoolKey {
        PoolKey {
            host: self.host.to_ascii_lowercase(),
            port: self.port,
            user: self.user.clone(),
            fingerprint: self.fingerprint.clone(),
        }
    }

    /// Build a `SshUrlParts` for reusing `SshMiddleware::get_session`.
    fn as_ssh_url_parts(&self) -> SshUrlParts {
        SshUrlParts {
            ssh_user: self.user.clone(),
            ssh_pass: self.pass.clone(),
            ssh_host: self.host.clone(),
            ssh_port: self.port,
            fingerprint: self.fingerprint.clone(),
            // Not used by get_session, but required by the struct
            internal_host: String::new(),
            internal_port: 0,
            http_path_and_query: String::new(),
        }
    }
}

fn parse_sftp_url(url: &reqwest::Url) -> anyhow::Result<SftpUrlParts> {
    anyhow::ensure!(url.scheme() == "sftp", "not an sftp:// URL");

    let host_raw = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("sftp URL missing host"))?;
    // Strip IPv6 brackets: "[::1]" → "::1"
    let host = host_raw
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host_raw)
        .to_string();
    let port = url.port().unwrap_or(22);

    let fragment = url.fragment().unwrap_or("");
    let mut user = String::new();
    let mut pass = String::new();
    let mut fingerprint = String::new();

    for pair in fragment.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            match k {
                "user" => user = percent_decode(v)?,
                "pass" => pass = percent_decode(v)?,
                "fingerprint" => fingerprint = normalize_hex(v),
                _ => {}
            }
        }
    }

    anyhow::ensure!(!user.is_empty(), "sftp URL missing user= in fragment");
    anyhow::ensure!(
        !fingerprint.is_empty(),
        "sftp URL missing fingerprint= in fragment"
    );

    // Percent-decode the remote path and validate
    let raw_path = url.path().to_string();
    let remote_path = percent_decode(&raw_path).unwrap_or(raw_path);
    anyhow::ensure!(
        remote_path.len() > 1,
        "sftp URL path must refer to a file, not root"
    );

    Ok(SftpUrlParts {
        user,
        pass,
        host,
        port,
        fingerprint,
        remote_path,
    })
}

// ====================================================================
// SFTP session pool
// ====================================================================

/// SFTP sessions are cached per (PoolKey, SSH connection identity).
/// This allows multiple SSH connections to the same host, each with its
/// own SFTP session — leveraging multi-connection download parallelism
/// while respecting MAX_STREAMS_PER_SESSION from the SSH pool.
type SftpCacheKey = (PoolKey, usize); // usize = Arc::as_ptr() of SSH active_streams counter

struct SftpSessionEntry {
    session: Arc<SftpSession>,
    last_used: Instant,
}

// ====================================================================
// SFTP middleware
// ====================================================================

const SFTP_SESSION_TIMEOUT: u64 = 60; // seconds

pub struct SftpMiddleware {
    ssh_pool: Arc<SshPoolInner>,
    sftp_pool: tokio::sync::Mutex<HashMap<SftpCacheKey, SftpSessionEntry>>,
}

impl SftpMiddleware {
    pub fn new(ssh_pool: Arc<SshPoolInner>) -> Self {
        Self {
            ssh_pool,
            sftp_pool: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    // ---- pool helpers -----------------------------------------------

    fn sweep_sftp_pool(&self, pool: &mut HashMap<SftpCacheKey, SftpSessionEntry>) {
        let now = Instant::now();
        let idle = self.ssh_pool.idle_timeout;
        pool.retain(|_k, entry| now.duration_since(entry.last_used) < idle);
    }

    async fn evict_sftp(&self, key: &PoolKey) {
        // Evict ALL SFTP sessions for this host (any SSH connection)
        self.sftp_pool
            .lock()
            .await
            .retain(|(k, _conn_id), _| k != key);
    }

    /// Get or create an SFTP session.
    ///
    /// Always calls `SshMiddleware::get_session` first, which:
    /// - Respects `MAX_STREAMS_PER_SESSION` (rotates to new SSH connections)
    /// - Returns the correct `ActiveStreamGuard` for the SSH connection
    ///
    /// SFTP sessions are then cached per SSH connection identity, so:
    /// - Concurrent downloads spread across multiple SSH connections
    /// - Each SSH connection has at most one SFTP session (channel)
    /// - The guard always matches the SSH connection the SFTP session lives on
    async fn get_or_create_sftp_session(
        &self,
        parts: &SftpUrlParts,
    ) -> anyhow::Result<(Arc<SftpSession>, ActiveStreamGuard)> {
        // Step 1: Always get SSH handle (respects MAX_STREAMS, may rotate connections)
        let ssh_mw = SshMiddleware::with_pool(Arc::clone(&self.ssh_pool));
        let ssh_parts = parts.as_ssh_url_parts();
        let (ssh_handle, stream_guard) = ssh_mw.get_session(&ssh_parts).await?;

        // Connection identity: unique pointer of this SSH connection's active_streams counter
        let conn_id = Arc::as_ptr(&stream_guard.counter) as usize;
        let cache_key = (parts.pool_key(), conn_id);

        // Step 2: Check SFTP cache for this specific SSH connection
        {
            let mut sftp_sessions = self.sftp_pool.lock().await;
            self.sweep_sftp_pool(&mut sftp_sessions);
            if let Some(entry) = sftp_sessions.get_mut(&cache_key) {
                entry.last_used = Instant::now();
                // Guard from get_session already protects THIS SSH connection ✓
                return Ok((Arc::clone(&entry.session), stream_guard));
            }
        }
        // Lock released

        // Step 3: Create SFTP session (no lock held)
        let channel = ssh_handle.channel_open_session().await?;
        channel.request_subsystem(true, "sftp").await?;
        let sftp = SftpSession::new(channel.into_stream()).await?;
        // russh-sftp default timeout is 10s — too short for slow networks
        sftp.set_timeout(SFTP_SESSION_TIMEOUT).await;
        let sftp = Arc::new(sftp);

        // Step 4: Race-safe insert
        {
            let mut sftp_sessions = self.sftp_pool.lock().await;
            if let Some(entry) = sftp_sessions.get_mut(&cache_key) {
                // Another task created a session for this connection — use theirs
                entry.last_used = Instant::now();
                return Ok((Arc::clone(&entry.session), stream_guard));
            }
            sftp_sessions.insert(
                cache_key,
                SftpSessionEntry {
                    session: Arc::clone(&sftp),
                    last_used: Instant::now(),
                },
            );
        }

        Ok((sftp, stream_guard))
    }

    // ---- Range parsing ----------------------------------------------

    /// Parse a single-range `Range` header value.
    /// Returns `Some((start, Option<end>))` on success.
    /// Returns `None` for multi-range (comma), suffix-range (-N), or
    /// any unparseable value — caller should respond with 416.
    fn parse_single_range(header_value: &str) -> Option<(u64, Option<u64>)> {
        let s = header_value.strip_prefix("bytes=")?;
        // Reject multi-range
        if s.contains(',') {
            return None;
        }
        let (start_s, end_s) = s.split_once('-')?;
        // Reject suffix-range like "-500"
        if start_s.is_empty() {
            return None;
        }
        let start: u64 = start_s.parse().ok()?;
        let end = if end_s.is_empty() {
            None
        } else {
            Some(end_s.parse::<u64>().ok()?)
        };
        Some((start, end))
    }

    /// Build a 416 Range Not Satisfiable response.
    fn build_416_response(total_size: u64) -> anyhow::Result<reqwest::Response> {
        let http_resp = http::Response::builder()
            .status(416)
            .header(CONTENT_RANGE, format!("bytes */{total_size}"))
            .header(CONTENT_LENGTH, 0)
            .body(reqwest::Body::from(vec![]))
            .map_err(|e| anyhow::anyhow!("SFTP: failed to build 416 response: {e}"))?;
        Ok(reqwest::Response::from(http_resp))
    }

    // ---- core SFTP download -----------------------------------------

    async fn sftp_request_once(
        sftp: &Arc<SftpSession>,
        stream_guard: ActiveStreamGuard,
        remote_path: &str,
        range_header: Option<&str>,
    ) -> anyhow::Result<reqwest::Response> {
        // stat for total size (needed for Content-Range and open-ended ranges)
        let metadata = sftp.metadata(remote_path).await?;
        let total_size = metadata
            .size
            .ok_or_else(|| anyhow::anyhow!("SFTP: server did not return file size"))?;

        // Compute range — reject invalid/multi-range with 416
        let (offset, limit_len, status) = if let Some(range_val) = range_header {
            match Self::parse_single_range(range_val) {
                Some((start, end_opt)) => {
                    let end = end_opt.unwrap_or(total_size.saturating_sub(1));
                    if start > end || start >= total_size {
                        return Self::build_416_response(total_size);
                    }
                    let clamped_end = end.min(total_size - 1);
                    (start, clamped_end - start + 1, 206u16)
                }
                None => {
                    // Unparseable, multi-range, or suffix-range → 416
                    warn!(range = range_val, "SFTP: rejecting unsupported Range header");
                    return Self::build_416_response(total_size);
                }
            }
        } else {
            (0, total_size, 200)
        };

        // Open + seek
        let mut file = sftp
            .open_with_flags(
                remote_path,
                russh_sftp::protocol::OpenFlags::READ,
            )
            .await?;
        if offset > 0 {
            file.seek(std::io::SeekFrom::Start(offset)).await?;
        }

        debug!(
            offset, limit_len, status, total_size, "SFTP: serving file"
        );

        // Streaming body — capture guards to keep SSH/SFTP alive
        let sftp_clone = Arc::clone(sftp);
        let body_stream = try_stream! {
            let _guard = stream_guard;     // keep SSH pool entry alive
            let _session = sftp_clone;     // keep SftpSession alive
            let mut buf = vec![0u8; 256 * 1024]; // 256 KB chunks
            let mut remaining = limit_len;
            while remaining > 0 {
                let to_read = (remaining as usize).min(buf.len());
                let n = file.read(&mut buf[..to_read]).await?;
                if n == 0 {
                    break;
                }
                remaining -= n as u64;
                yield Bytes::copy_from_slice(&buf[..n]);
            }
            if remaining > 0 {
                Err::<Bytes, std::io::Error>(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!(
                        "SFTP: short read — expected {limit_len} bytes, got {}",
                        limit_len - remaining
                    ),
                ))?;
            }
        };

        // Build HTTP response
        let mut builder = http::Response::builder()
            .status(status)
            .header(CONTENT_LENGTH, limit_len)
            .header(ACCEPT_RANGES, "bytes");

        if status == 206 {
            let end = offset + limit_len - 1;
            builder = builder
                .header(CONTENT_RANGE, format!("bytes {offset}-{end}/{total_size}"));
        }

        let http_resp = builder
            .body(reqwest::Body::wrap_stream(
                body_stream.map_err(|e: std::io::Error| e),
            ))
            .map_err(|e| anyhow::anyhow!("SFTP: failed to build response: {e}"))?;

        Ok(reqwest::Response::from(http_resp))
    }

    /// Top-level request handler: try once, retry on recoverable transport error.
    async fn sftp_request(
        &self,
        req: reqwest::Request,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        let parts =
            parse_sftp_url(req.url()).map_err(|e| mw_err(format!("SFTP URL parse: {e}")))?;
        let key = parts.pool_key();

        let range_header = req
            .headers()
            .get(RANGE)
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        // First attempt
        match self.try_sftp(&parts, range_header.as_deref()).await {
            Ok(resp) => return Ok(resp),
            Err(e) if Self::is_recoverable_transport_error(&e) => {
                warn!("SFTP: recoverable error, retrying: {e:#}");
                self.evict_sftp(&key).await;
                SshMiddleware::with_pool(Arc::clone(&self.ssh_pool)).evict(&key);
            }
            Err(e) => return Err(mw_err(format!("SFTP: {e:#}"))),
        }

        // Retry
        self.try_sftp(&parts, range_header.as_deref())
            .await
            .map_err(|e| mw_err(format!("SFTP retry: {e:#}")))
    }

    async fn try_sftp(
        &self,
        parts: &SftpUrlParts,
        range_header: Option<&str>,
    ) -> anyhow::Result<reqwest::Response> {
        let (sftp, guard) = self.get_or_create_sftp_session(parts).await?;
        Self::sftp_request_once(&sftp, guard, &parts.remote_path, range_header).await
    }

    fn is_recoverable_transport_error(err: &anyhow::Error) -> bool {
        // Check for russh transport errors
        if let Some(ssh_err) = err.downcast_ref::<russh::Error>() {
            return is_recoverable_ssh_error(ssh_err);
        }
        // Check for SFTP-level transport errors
        if let Some(sftp_err) = err.downcast_ref::<russh_sftp::client::error::Error>() {
            return matches!(
                sftp_err,
                russh_sftp::client::error::Error::IO(_)
                    | russh_sftp::client::error::Error::Timeout
            );
        }
        // Check for I/O errors
        if let Some(io_err) = err.downcast_ref::<std::io::Error>() {
            return matches!(
                io_err.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::TimedOut
            );
        }
        false
    }
}

#[async_trait::async_trait]
impl Middleware for SftpMiddleware {
    async fn handle(
        &self,
        req: reqwest::Request,
        extensions: &mut http::Extensions,
        next: Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        if req.url().scheme() != "sftp" {
            return next.run(req, extensions).await;
        }
        self.sftp_request(req).await
    }
}
