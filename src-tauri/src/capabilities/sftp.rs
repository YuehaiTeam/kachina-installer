// ====================================================================
// SFTP download middleware
//
// Intercepts `sftp://` URLs and serves file content as HTTP responses.
// 连接模型：每请求一条 SSH 连接（见 docs/notes SSH/SFTP 栈整体瘦身）。
// SFTP 协议为自写最小客户端：INIT/OPEN/STAT/READ 四条请求报文，
// CLOSE 省略——连接随响应流结束整体关闭，服务端自然回收句柄。
//
// URL format:
//   sftp://host:port/remote/path#user=xxx&pass=yyy&fingerprint=sha256hex
// ====================================================================

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use async_stream::try_stream;
use bytes::Bytes;
use futures::TryStreamExt;
use http::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use reqwest_middleware::{Middleware, Next};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, warn};

use super::ssh::{mw_err_fmt, normalize_hex, percent_decode, ssh_connect, SSH_CHANNEL_TIMEOUT};

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
// Minimal SFTP v3 client (draft-ietf-secsh-filexfer-02)
// ====================================================================

const SFTP_OP_TIMEOUT: Duration = Duration::from_secs(30);
/// 单条 READ 请求长度。OpenSSH 服务端上限 256KiB，取 128KiB 留余量。
const READ_CHUNK: u32 = 128 * 1024;
/// 流水线中的 outstanding READ 数，掩盖 RTT 保吞吐。
const PIPELINE: usize = 4;

const FXP_INIT: u8 = 1;
const FXP_VERSION: u8 = 2;
const FXP_OPEN: u8 = 3;
const FXP_READ: u8 = 5;
const FXP_STAT: u8 = 17;
const FXP_STATUS: u8 = 101;
const FXP_HANDLE: u8 = 102;
const FXP_DATA: u8 = 103;
const FXP_ATTRS: u8 = 105;

const FX_EOF: u32 = 1;
const FXF_READ: u32 = 0x0000_0001;
const ATTR_SIZE: u32 = 0x0000_0001;

struct SftpClient<S> {
    stream: S,
    next_id: u32,
}

/// 解析游标：`(payload, pos)` 上的读取原语。
fn get_u32(buf: &[u8], pos: &mut usize) -> anyhow::Result<u32> {
    let end = *pos + 4;
    anyhow::ensure!(buf.len() >= end, "SFTP packet truncated (u32)");
    let v = u32::from_be_bytes(buf[*pos..end].try_into().unwrap());
    *pos = end;
    Ok(v)
}

fn get_u64(buf: &[u8], pos: &mut usize) -> anyhow::Result<u64> {
    let end = *pos + 8;
    anyhow::ensure!(buf.len() >= end, "SFTP packet truncated (u64)");
    let v = u64::from_be_bytes(buf[*pos..end].try_into().unwrap());
    *pos = end;
    Ok(v)
}

fn get_bytes<'a>(buf: &'a [u8], pos: &mut usize) -> anyhow::Result<&'a [u8]> {
    let len = get_u32(buf, pos)? as usize;
    let end = *pos + len;
    anyhow::ensure!(buf.len() >= end, "SFTP packet truncated (string)");
    let s = &buf[*pos..end];
    *pos = end;
    Ok(s)
}

fn put_str(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&(s.len() as u32).to_be_bytes());
    out.extend_from_slice(s);
}

/// STATUS 报文转错误。payload 含 request id。
fn status_to_err(payload: &[u8]) -> anyhow::Error {
    let mut pos = 4; // skip request id
    let code = get_u32(payload, &mut pos).unwrap_or(u32::MAX);
    let msg = get_bytes(payload, &mut pos)
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default();
    anyhow::anyhow!("SFTP: server status {code}: {msg}")
}

impl<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> SftpClient<S> {
    fn new(stream: S) -> Self {
        Self { stream, next_id: 0 }
    }

    async fn send_packet(&mut self, body: &[u8]) -> anyhow::Result<()> {
        let mut framed = Vec::with_capacity(4 + body.len());
        framed.extend_from_slice(&(body.len() as u32).to_be_bytes());
        framed.extend_from_slice(body);
        self.stream.write_all(&framed).await?;
        Ok(())
    }

    /// 读一个完整报文，返回 (type, payload)。payload 不含 type 字节。
    async fn recv_packet(&mut self) -> anyhow::Result<(u8, Vec<u8>)> {
        let fut = async {
            let mut len_buf = [0u8; 4];
            self.stream.read_exact(&mut len_buf).await?;
            let len = u32::from_be_bytes(len_buf) as usize;
            anyhow::ensure!(
                (1..=16 * 1024 * 1024).contains(&len),
                "SFTP packet length {len} out of range"
            );
            let mut body = vec![0u8; len];
            self.stream.read_exact(&mut body).await?;
            let ptype = body[0];
            body.remove(0);
            Ok((ptype, body))
        };
        tokio::time::timeout(SFTP_OP_TIMEOUT, fut)
            .await
            .map_err(|_| anyhow::anyhow!("SFTP: response timeout"))?
    }

    fn take_id(&mut self) -> u32 {
        self.next_id = self.next_id.wrapping_add(1);
        self.next_id
    }

    async fn init(&mut self) -> anyhow::Result<()> {
        let mut body = vec![FXP_INIT];
        body.extend_from_slice(&3u32.to_be_bytes());
        self.send_packet(&body).await?;
        let (ptype, payload) = self.recv_packet().await?;
        anyhow::ensure!(ptype == FXP_VERSION, "SFTP: expected VERSION, got {ptype}");
        let mut pos = 0;
        let version = get_u32(&payload, &mut pos)?;
        anyhow::ensure!(version >= 3, "SFTP: server version {version} < 3");
        Ok(())
    }

    async fn stat_size(&mut self, path: &str) -> anyhow::Result<u64> {
        let id = self.take_id();
        let mut body = vec![FXP_STAT];
        body.extend_from_slice(&id.to_be_bytes());
        put_str(&mut body, path.as_bytes());
        self.send_packet(&body).await?;
        let (ptype, payload) = self.recv_packet().await?;
        match ptype {
            FXP_ATTRS => {
                let mut pos = 4; // skip request id
                let flags = get_u32(&payload, &mut pos)?;
                anyhow::ensure!(
                    flags & ATTR_SIZE != 0,
                    "SFTP: server did not return file size"
                );
                get_u64(&payload, &mut pos)
            }
            FXP_STATUS => Err(status_to_err(&payload)),
            other => Err(anyhow::anyhow!("SFTP: expected ATTRS, got {other}")),
        }
    }

    async fn open_read(&mut self, path: &str) -> anyhow::Result<Vec<u8>> {
        let id = self.take_id();
        let mut body = vec![FXP_OPEN];
        body.extend_from_slice(&id.to_be_bytes());
        put_str(&mut body, path.as_bytes());
        body.extend_from_slice(&FXF_READ.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes()); // empty ATTRS
        self.send_packet(&body).await?;
        let (ptype, payload) = self.recv_packet().await?;
        match ptype {
            FXP_HANDLE => {
                let mut pos = 4; // skip request id
                Ok(get_bytes(&payload, &mut pos)?.to_vec())
            }
            FXP_STATUS => Err(status_to_err(&payload)),
            other => Err(anyhow::anyhow!("SFTP: expected HANDLE, got {other}")),
        }
    }

    async fn send_read(&mut self, handle: &[u8], offset: u64, len: u32) -> anyhow::Result<u32> {
        let id = self.take_id();
        let mut body = vec![FXP_READ];
        body.extend_from_slice(&id.to_be_bytes());
        put_str(&mut body, handle);
        body.extend_from_slice(&offset.to_be_bytes());
        body.extend_from_slice(&len.to_be_bytes());
        self.send_packet(&body).await?;
        Ok(id)
    }
}

// ====================================================================
// SFTP middleware
// ====================================================================

pub struct SftpMiddleware;

impl SftpMiddleware {
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

    async fn sftp_request(&self, req: reqwest::Request) -> anyhow::Result<reqwest::Response> {
        let parts = parse_sftp_url(req.url())?;
        let range_header = req
            .headers()
            .get(RANGE)
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        // Fresh SSH connection + sftp subsystem channel
        let session_handle = ssh_connect(
            &parts.host,
            parts.port,
            &parts.user,
            &parts.pass,
            &parts.fingerprint,
        )
        .await?;
        let channel =
            tokio::time::timeout(SSH_CHANNEL_TIMEOUT, session_handle.channel_open_session())
                .await
                .map_err(|_| anyhow::anyhow!("SFTP: session channel timeout"))??;
        tokio::time::timeout(SSH_CHANNEL_TIMEOUT, channel.request_subsystem(true, "sftp"))
            .await
            .map_err(|_| anyhow::anyhow!("SFTP: subsystem request timeout"))??;

        let mut sftp = SftpClient::new(channel.into_stream());
        sftp.init().await?;

        // stat for total size (needed for Content-Range and open-ended ranges)
        let total_size = sftp.stat_size(&parts.remote_path).await?;

        // Compute range — reject invalid/multi-range with 416
        let (offset, limit_len, status) = if let Some(range_val) = range_header.as_deref() {
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
                    warn!(
                        range = range_val,
                        "SFTP: rejecting unsupported Range header"
                    );
                    return Self::build_416_response(total_size);
                }
            }
        } else {
            (0, total_size, 200)
        };

        let handle = sftp.open_read(&parts.remote_path).await?;

        debug!(offset, limit_len, status, total_size, "SFTP: serving file");

        // Streaming body：流水线 READ + 乱序缓冲 + 短读补发。
        // 捕获 session_handle 保持连接存活；流结束连接整体关闭。
        let body_stream = try_stream! {
            let _session = session_handle;
            let mut sftp = sftp;
            let end = offset + limit_len;
            let mut emit_offset = offset;      // 已按序吐出的位置
            let mut next_req_offset = offset;  // 下一条 READ 的起点
            let mut pending: HashMap<u32, (u64, u32)> = HashMap::new(); // id → (offset, len)
            let mut ready: BTreeMap<u64, Bytes> = BTreeMap::new();      // offset → data

            while emit_offset < end {
                while pending.len() < PIPELINE && next_req_offset < end {
                    let len = (end - next_req_offset).min(READ_CHUNK as u64) as u32;
                    let id = sftp.send_read(&handle, next_req_offset, len).await
                        .map_err(std::io::Error::other)?;
                    pending.insert(id, (next_req_offset, len));
                    next_req_offset += len as u64;
                }

                let (ptype, payload) = sftp.recv_packet().await.map_err(std::io::Error::other)?;
                let mut pos = 0;
                let id = get_u32(&payload, &mut pos).map_err(std::io::Error::other)?;
                let Some((req_off, req_len)) = pending.remove(&id) else {
                    Err(std::io::Error::other(format!("SFTP: response for unknown id {id}")))?;
                    unreachable!();
                };
                match ptype {
                    FXP_DATA => {
                        let data = get_bytes(&payload, &mut pos).map_err(std::io::Error::other)?;
                        if data.is_empty() {
                            Err(std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                "SFTP: empty DATA response",
                            ))?;
                        }
                        // 短读：服务端有权少给，补发缺口
                        if (data.len() as u32) < req_len {
                            let gap_off = req_off + data.len() as u64;
                            let gap_len = req_len - data.len() as u32;
                            let id = sftp.send_read(&handle, gap_off, gap_len).await
                                .map_err(std::io::Error::other)?;
                            pending.insert(id, (gap_off, gap_len));
                        }
                        ready.insert(req_off, Bytes::copy_from_slice(data));
                    }
                    FXP_STATUS => {
                        // 读取范围由 stat 限定，EOF 意味着文件在传输中变短
                        let err = status_to_err(&payload);
                        let mut pos2 = 4;
                        let code = get_u32(&payload, &mut pos2).unwrap_or(u32::MAX);
                        if code == FX_EOF {
                            Err(std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                format!("SFTP: unexpected EOF at offset {req_off} (file shrank?)"),
                            ))?;
                        } else {
                            Err(std::io::Error::other(err.to_string()))?;
                        }
                    }
                    other => {
                        Err(std::io::Error::other(format!("SFTP: unexpected packet type {other}")))?;
                    }
                }

                while let Some(chunk) = ready.remove(&emit_offset) {
                    emit_offset += chunk.len() as u64;
                    yield chunk;
                }
            }
        };

        // Build HTTP response
        let mut builder = http::Response::builder()
            .status(status)
            .header(CONTENT_LENGTH, limit_len)
            .header(ACCEPT_RANGES, "bytes");

        if status == 206 {
            let end = offset + limit_len - 1;
            builder = builder.header(CONTENT_RANGE, format!("bytes {offset}-{end}/{total_size}"));
        }

        let http_resp = builder
            .body(reqwest::Body::wrap_stream(
                body_stream.map_err(|e: std::io::Error| e),
            ))
            .map_err(|e| anyhow::anyhow!("SFTP: failed to build response: {e}"))?;

        Ok(reqwest::Response::from(http_resp))
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
        self.sftp_request(req)
            .await
            .map_err(|e| mw_err_fmt(format_args!("SFTP: {e:#}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_range_parsing() {
        assert_eq!(
            SftpMiddleware::parse_single_range("bytes=0-99"),
            Some((0, Some(99)))
        );
        assert_eq!(
            SftpMiddleware::parse_single_range("bytes=100-"),
            Some((100, None))
        );
        assert_eq!(SftpMiddleware::parse_single_range("bytes=-500"), None);
        assert_eq!(SftpMiddleware::parse_single_range("bytes=0-1,5-9"), None);
        assert_eq!(SftpMiddleware::parse_single_range("items=0-1"), None);
    }

    #[test]
    fn packet_cursor_primitives() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&7u32.to_be_bytes());
        buf.extend_from_slice(&0x1122334455667788u64.to_be_bytes());
        put_str(&mut buf, b"handle");
        let mut pos = 0;
        assert_eq!(get_u32(&buf, &mut pos).unwrap(), 7);
        assert_eq!(get_u64(&buf, &mut pos).unwrap(), 0x1122334455667788);
        assert_eq!(get_bytes(&buf, &mut pos).unwrap(), b"handle");
        assert!(get_u32(&buf, &mut pos).is_err()); // 越界
    }

    /// e2e：需要本地 Go 测试 server（见 tests/sshd/）。
    /// 运行：go run ./tests/sshd -root <dir>，取输出 FINGERPRINT，然后
    /// SSHD_FP=<hex> SSHD_ROOT=<dir> cargo test ssh_e2e -- --ignored --nocapture
    #[test]
    #[ignore = "requires tests/sshd go server (SSHD_FP / SSHD_ROOT env)"]
    fn ssh_e2e() {
        let fp = std::env::var("SSHD_FP").expect("SSHD_FP not set");
        let root = std::env::var("SSHD_ROOT").expect("SSHD_ROOT not set");
        let port: u16 = std::env::var("SSHD_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(18122);

        // 测试文件：3MiB 确定性内容，覆盖多个 READ_CHUNK 与流水线深度
        let pattern: Vec<u8> = (0..3 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(std::path::Path::new(&root).join("e2e.bin"), &pattern).unwrap();

        let cred = format!("user=test&pass=pass123&fingerprint={fp}");
        let sftp_url = format!("sftp://127.0.0.1:{port}/e2e.bin#{cred}");
        let tunnel_url = format!("ssh+http://127.0.0.1:{port}/e2e.bin#{cred}");

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let client = &*crate::DOWNLOAD_CLIENT;

            // sftp 整文件
            let resp = client.get(&sftp_url).send().await.unwrap();
            assert_eq!(resp.status(), 200);
            assert_eq!(resp.bytes().await.unwrap().as_ref(), &pattern[..]);

            // sftp Range（跨 chunk 边界）
            let resp = client
                .get(&sftp_url)
                .header("range", "bytes=100000-299999")
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 206);
            assert_eq!(
                resp.headers()["content-range"],
                format!("bytes 100000-299999/{}", pattern.len())
            );
            assert_eq!(
                resp.bytes().await.unwrap().as_ref(),
                &pattern[100000..300000]
            );

            // sftp 后缀 Range → 416
            let resp = client
                .get(&sftp_url)
                .header("range", "bytes=-500")
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 416);

            // ssh+http 整文件与 Range（Go http.FileServer 原生支持 Range）
            let resp = client.get(&tunnel_url).send().await.unwrap();
            assert_eq!(resp.status(), 200);
            assert_eq!(resp.bytes().await.unwrap().as_ref(), &pattern[..]);
            let resp = client
                .get(&tunnel_url)
                .header("range", "bytes=1048576-2097151")
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 206);
            assert_eq!(
                resp.bytes().await.unwrap().as_ref(),
                &pattern[1048576..2097152]
            );

            // 指纹不匹配 → 拒连
            let bad_fp: String = fp
                .chars()
                .map(|c| if c == 'a' { 'b' } else { 'a' })
                .collect();
            let bad_url = format!(
                "sftp://127.0.0.1:{port}/e2e.bin#user=test&pass=pass123&fingerprint={bad_fp}"
            );
            assert!(client.get(&bad_url).send().await.is_err());

            // 并发突发：8 路并行 Range 下载，各自独立连接
            let tasks: Vec<_> = (0..8u64)
                .map(|i| {
                    let url = sftp_url.clone();
                    let start = i * 300_000;
                    let end = start + 199_999;
                    tokio::spawn(async move {
                        let resp = crate::DOWNLOAD_CLIENT
                            .get(&url)
                            .header("range", format!("bytes={start}-{end}"))
                            .send()
                            .await
                            .unwrap();
                        assert_eq!(resp.status(), 206);
                        (start as usize, resp.bytes().await.unwrap())
                    })
                })
                .collect();
            for t in tasks {
                let (start, bytes) = t.await.unwrap();
                assert_eq!(bytes.as_ref(), &pattern[start..start + 200_000]);
            }
        });
    }
}
