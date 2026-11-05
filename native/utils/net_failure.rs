//! Network failure classification by error type.
//!
//! A download failure yields two values: a fine-grained tag for
//! `InsightItem.error`, which the telemetry server clusters on, and the coarse
//! user-facing download code. Both are read from the typed causes in the error
//! chain, never from error text; the one exception is the OS code inside a
//! native-tls error (see [`tls_tag`]).

use std::error::Error;
use std::io;

use h3::error::{Code, ConnectionError as H3Connection, LocalError, StreamError as H3Stream};
use h3::quic::ConnectionErrorIncoming;
use h3_msquic_async::msquic::{Status, StatusCode};
use h3_msquic_async::msquic_async::{ConnectionError as QuicConnection, ConnectionStartError};

use crate::capabilities::{sftp::SftpStatus, ssh::SshAuthRejected};
use crate::utils::code::{
    DOWNLOAD_FAILED, DOWNLOAD_REFUSED, DOWNLOAD_STALLED, DOWNLOAD_TIMEOUT, SERVER_HTTP_ERROR,
};

/// Where in the request a failure happened.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    /// Connecting, sending the request, waiting for the response head.
    Send,
    /// Reading the response body.
    Body,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NetFailure {
    /// `InsightItem.error`. Low-cardinality: a name, optionally with a status,
    /// protocol or OS code.
    pub tag: String,
    /// User-facing download code.
    pub code: &'static str,
}

/// What picks the user-facing code.
#[derive(Clone, Copy)]
enum Class {
    /// The peer could not be reached, or refused the connection.
    Connect,
    Timeout,
    /// The body arrives, but too slowly.
    Slow,
    Status,
    Other,
}

impl NetFailure {
    fn new(tag: impl Into<String>, class: Class, phase: Phase) -> Self {
        let code = match (class, phase) {
            (Class::Status, _) => SERVER_HTTP_ERROR,
            (Class::Slow, _) | (Class::Timeout, Phase::Body) => DOWNLOAD_STALLED,
            (Class::Timeout, Phase::Send) => DOWNLOAD_TIMEOUT,
            (Class::Connect, Phase::Send) => DOWNLOAD_REFUSED,
            (Class::Connect, Phase::Body) | (Class::Other, _) => DOWNLOAD_FAILED,
        };
        Self {
            tag: tag.into(),
            code,
        }
    }

    pub fn stalled() -> Self {
        Self::new("NET_STALLED", Class::Slow, Phase::Body)
    }

    pub fn too_slow() -> Self {
        Self::new("NET_TOO_SLOW", Class::Slow, Phase::Body)
    }
}

/// A failure the elevated helper classified, rebuilt by the launcher from the
/// pipe (`IpcError::into_ta`). Only the text of the original chain crosses, so
/// it displays that text and has no source.
#[derive(Debug)]
pub struct CarriedFailure {
    pub failure: NetFailure,
    pub text: String,
}

impl std::fmt::Display for CarriedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl Error for CarriedFailure {}

/// Classifies an error known to come off the network, such as a failed read
/// of a download body. `NET_OTHER` when no cause is recognized.
pub fn classify(err: &(dyn Error + 'static), phase: Phase) -> NetFailure {
    let mut walk = Walk::new(phase, true);
    walk.run(err).unwrap_or_else(|| walk.fallback())
}

/// The network failure behind an error of any origin, as a send-phase failure
/// unless a body read already classified it. `None` when nothing in the chain
/// comes from the network (a local file error, a malformed API answer).
pub fn find(err: &(dyn Error + 'static)) -> Option<NetFailure> {
    let mut walk = Walk::new(Phase::Send, false);
    walk.run(err)
        .or_else(|| walk.in_net.then(|| walk.fallback()))
}

struct Walk {
    phase: Phase,
    /// A network-layer error has been passed, so io errors below it are
    /// network errors rather than local ones.
    in_net: bool,
    /// reqwest reported a connect failure (DNS, TCP or TLS).
    connect: bool,
}

enum Step<'a> {
    Done(NetFailure),
    Next(&'a (dyn Error + 'static)),
    End,
}

impl Walk {
    fn new(phase: Phase, in_net: bool) -> Self {
        Self {
            phase,
            in_net,
            connect: false,
        }
    }

    fn run(&mut self, mut err: &(dyn Error + 'static)) -> Option<NetFailure> {
        loop {
            match self.step(err) {
                Step::Done(failure) => return Some(failure),
                Step::Next(next) => err = next,
                Step::End => return None,
            }
        }
    }

    fn failure(&self, tag: impl Into<String>, class: Class) -> NetFailure {
        // A failure inside reqwest's connect phase (DNS, TCP, TLS) is a connect
        // failure whatever its cause.
        let class = match class {
            Class::Other if self.connect => Class::Connect,
            class => class,
        };
        NetFailure::new(tag, class, self.phase)
    }

    fn timeout(&self) -> NetFailure {
        let tag = match (self.connect, self.phase) {
            (true, _) => "NET_CONNECT_TIMEOUT",
            (false, Phase::Send) => "NET_TIMEOUT",
            (false, Phase::Body) => "NET_READ_TIMEOUT",
        };
        self.failure(tag, Class::Timeout)
    }

    fn fallback(&self) -> NetFailure {
        let tag = if self.connect {
            "NET_CONNECT"
        } else {
            "NET_OTHER"
        };
        self.failure(tag, Class::Other)
    }

    fn status(&self, status: u16) -> NetFailure {
        self.failure(format!("NET_HTTP_{status}"), Class::Status)
    }

    fn step<'a>(&mut self, e: &'a (dyn Error + 'static)) -> Step<'a> {
        if let Some(c) = e.downcast_ref::<crate::fs::ClassifiedNetworkError>() {
            return Step::Done(c.failure.clone());
        }
        if let Some(c) = e.downcast_ref::<CarriedFailure>() {
            return Step::Done(c.failure.clone());
        }
        if let Some(s) = e.downcast_ref::<crate::dfs::HttpStatus>() {
            return Step::Done(self.status(s.status));
        }
        if let Some(io) = e.downcast_ref::<io::Error>() {
            return self.io(io);
        }
        if let Some(m) = e.downcast_ref::<reqwest_middleware::Error>() {
            self.in_net = true;
            return match m {
                reqwest_middleware::Error::Reqwest(r) => self.reqwest(r),
                // `#[error(transparent)]`: its source() skips the anyhow root.
                reqwest_middleware::Error::Middleware(a) => Step::Next(a.as_ref()),
            };
        }
        if let Some(r) = e.downcast_ref::<reqwest::Error>() {
            self.in_net = true;
            return self.reqwest(r);
        }
        if let Some(h) = e.downcast_ref::<hyper::Error>() {
            self.in_net = true;
            return self.hyper(h);
        }
        if let Some(t) = e.downcast_ref::<native_tls::Error>() {
            return Step::Done(self.failure(tls_tag(t), Class::Connect));
        }
        if let Some(s) = e.downcast_ref::<H3Stream>() {
            self.in_net = true;
            return self.h3_stream(s);
        }
        if let Some(c) = e.downcast_ref::<H3Connection>() {
            self.in_net = true;
            return self.h3_connection(c);
        }
        if let Some(q) = e.downcast_ref::<QuicConnection>() {
            return Step::Done(self.quic(q));
        }
        if let Some(ConnectionStartError::OtherError(status)) = e.downcast_ref() {
            return Step::Done(self.quic_status(status));
        }
        if let Some(r) = e.downcast_ref::<russh::Error>() {
            self.in_net = true;
            return self.ssh(r);
        }
        if e.is::<SshAuthRejected>() {
            return Step::Done(self.failure("SSH_AUTH", Class::Connect));
        }
        if let Some(s) = e.downcast_ref::<SftpStatus>() {
            return Step::Done(self.failure(format!("SFTP_STATUS:{}", s.code), Class::Other));
        }
        e.source().map_or(Step::End, Step::Next)
    }

    /// `io::Error::source()` skips the payload, so the payload is walked here;
    /// the kind counts only when the payload names nothing.
    fn io<'a>(&mut self, io: &'a io::Error) -> Step<'a> {
        if let Some(payload) = io.get_ref() {
            if let Some(failure) = self.run(payload) {
                return Step::Done(failure);
            }
        }
        if !self.in_net {
            return Step::End;
        }
        use io::ErrorKind as K;
        // WSAHOST_NOT_FOUND, WSATRY_AGAIN, WSANO_RECOVERY, WSANO_DATA
        if matches!(io.raw_os_error(), Some(11001..=11004)) {
            return Step::Done(self.failure("NET_DNS", Class::Connect));
        }
        let (tag, class): (String, Class) = match io.kind() {
            K::TimedOut => return Step::Done(self.timeout()),
            K::ConnectionRefused => ("NET_CONNECT_REFUSED".into(), Class::Connect),
            K::NetworkUnreachable | K::HostUnreachable | K::NetworkDown => {
                ("NET_UNREACHABLE".into(), Class::Connect)
            }
            K::ConnectionReset | K::ConnectionAborted => ("NET_RESET".into(), Class::Other),
            K::UnexpectedEof => ("NET_EOF".into(), Class::Other),
            K::BrokenPipe | K::NotConnected => ("NET_CLOSED".into(), Class::Other),
            K::InvalidData | K::InvalidInput => ("NET_PROTOCOL".into(), Class::Other),
            _ => match io.raw_os_error() {
                Some(code) => (format!("NET_OS:{code}"), Class::Other),
                None => return Step::End,
            },
        };
        Step::Done(self.failure(tag, class))
    }

    fn reqwest<'a>(&mut self, r: &'a reqwest::Error) -> Step<'a> {
        if let Some(status) = r.status() {
            return Step::Done(self.status(status.as_u16()));
        }
        self.connect |= r.is_connect();
        if r.is_timeout() {
            return Step::Done(self.timeout());
        }
        r.source().map_or(Step::End, Step::Next)
    }

    fn hyper<'a>(&self, h: &'a hyper::Error) -> Step<'a> {
        if h.is_timeout() {
            return Step::Done(self.timeout());
        }
        if h.is_parse() {
            return Step::Done(self.failure("NET_PROTOCOL", Class::Other));
        }
        if h.is_incomplete_message() || h.is_closed() || h.is_canceled() {
            return Step::Done(self.failure("NET_CLOSED", Class::Other));
        }
        h.source().map_or(Step::End, Step::Next)
    }

    fn h3_stream<'a>(&mut self, e: &'a H3Stream) -> Step<'a> {
        let tag = match e {
            H3Stream::StreamError { code, .. } => format!("H3_STREAM_ERROR:{code:?}"),
            H3Stream::RemoteTerminate { code, .. } => format!("H3_STREAM_RESET:{code:?}"),
            H3Stream::ConnectionError { 0: c, .. } => return self.h3_connection(c),
            H3Stream::HeaderTooBig { .. } => "H3_HEADER_TOO_BIG".into(),
            H3Stream::RemoteClosing { .. } => "H3_GOAWAY".into(),
            H3Stream::Undefined { 0: inner, .. } => return Step::Next(&**inner),
            _ => "H3_OTHER".into(),
        };
        Step::Done(self.failure(tag, Class::Other))
    }

    fn h3_connection<'a>(&mut self, e: &'a H3Connection) -> Step<'a> {
        let (tag, class): (String, Class) = match e {
            H3Connection::Local { error, .. } => match error {
                LocalError::Application { code, .. } => {
                    (format!("H3_LOCAL_CLOSE:{code:?}"), Class::Other)
                }
                _ => ("H3_LOCAL_CLOSE".into(), Class::Other),
            },
            H3Connection::Remote { 0: incoming, .. } => match incoming {
                ConnectionErrorIncoming::ApplicationClose { error_code } => (
                    format!("H3_CLOSED:{:?}", Code::from(*error_code)),
                    Class::Other,
                ),
                ConnectionErrorIncoming::Timeout => ("QUIC_TIMEOUT".into(), Class::Timeout),
                ConnectionErrorIncoming::InternalError(_) => ("H3_INTERNAL".into(), Class::Other),
                ConnectionErrorIncoming::Undefined(inner) => return Step::Next(&**inner),
            },
            H3Connection::Timeout { .. } => ("QUIC_TIMEOUT".into(), Class::Timeout),
            _ => ("H3_OTHER".into(), Class::Other),
        };
        Step::Done(self.failure(tag, class))
    }

    fn quic(&self, e: &QuicConnection) -> NetFailure {
        match e {
            QuicConnection::ShutdownByTransport(status, _) | QuicConnection::OtherError(status) => {
                self.quic_status(status)
            }
            QuicConnection::ShutdownByPeer(code) => {
                self.failure(format!("H3_CLOSED:{:?}", Code::from(*code)), Class::Other)
            }
            QuicConnection::ShutdownByLocal => self.failure("QUIC_LOCAL_SHUTDOWN", Class::Other),
            QuicConnection::ConnectionClosed => self.failure("QUIC_CLOSED", Class::Other),
            QuicConnection::SslKeyLogFileAlreadySet => self.failure("QUIC_OTHER", Class::Other),
        }
    }

    fn quic_status(&self, status: &Status) -> NetFailure {
        match status.try_as_status_code() {
            Ok(
                StatusCode::QUIC_STATUS_CONNECTION_TIMEOUT
                | StatusCode::QUIC_STATUS_CONNECTION_IDLE,
            ) => self.failure("QUIC_TIMEOUT", Class::Timeout),
            Ok(code) => {
                let name = code.to_string();
                let name = name.trim_start_matches("QUIC_STATUS_");
                self.failure(format!("QUIC_TRANSPORT:{name}"), Class::Connect)
            }
            Err(_) => self.failure(
                format!("QUIC_TRANSPORT:{:#x}", status.0 as u32),
                Class::Connect,
            ),
        }
    }

    fn ssh<'a>(&self, e: &'a russh::Error) -> Step<'a> {
        use russh::Error as E;
        let (tag, class): (String, Class) = match e {
            // `#[error(transparent)]`: its source() skips the io::Error.
            E::IO(io) => return Step::Next(io),
            E::ConnectionTimeout | E::KeepaliveTimeout | E::InactivityTimeout | E::Elapsed(_) => {
                ("SSH_TIMEOUT".into(), Class::Timeout)
            }
            E::KexInit
            | E::Kex
            | E::UnknownAlgo
            | E::NoCommonAlgo { .. }
            | E::Version
            | E::StrictKeyExchangeViolation { .. } => ("SSH_KEX".into(), Class::Connect),
            E::UnknownKey | E::WrongServerSig | E::KeyChanged { .. } => {
                ("SSH_HOST_KEY".into(), Class::Connect)
            }
            E::NotAuthenticated | E::NoAuthMethod | E::UnsupportedAuthMethod => {
                ("SSH_AUTH".into(), Class::Connect)
            }
            E::ChannelOpenFailure(reason) => (format!("SSH_CHANNEL:{reason:?}"), Class::Connect),
            E::WrongChannel => ("SSH_CHANNEL".into(), Class::Connect),
            E::Disconnect | E::HUP | E::RecvError | E::SendError => {
                ("SSH_DISCONNECT".into(), Class::Other)
            }
            _ => ("SSH_OTHER".into(), Class::Other),
        };
        Step::Done(self.failure(tag, class))
    }
}

/// native-tls hides the schannel `io::Error` (its `source()` is that error's
/// source), so the HRESULT survives only in the text: "… (os error -2146762495)".
fn tls_tag(e: &native_tls::Error) -> String {
    let text = e.to_string();
    let code = text
        .rsplit_once("(os error ")
        .and_then(|(_, rest)| rest.strip_suffix(')'))
        .and_then(|n| n.parse::<i32>().ok());
    match code {
        Some(code) => format!("NET_TLS:{:#x}", code as u32),
        None => "NET_TLS".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::code::{attach_download, extract, Extracted};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Answers one request with `response`, then closes the connection, or
    /// resets it once the client has had time to read the response head.
    async fn serve(response: &'static [u8], reset: bool) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/f", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(socket.read_u8().await.unwrap());
            }
            socket.write_all(response).await.unwrap();
            if reset {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                // A zero linger makes the drop an abortive close (RST), which
                // does not block.
                #[allow(deprecated)]
                socket.set_linger(Some(std::time::Duration::ZERO)).unwrap();
            }
        });
        url
    }

    fn code_of(err: anyhow::Error) -> &'static str {
        match extract(&attach_download(err, None, None)) {
            Extracted::Coded(c) => c.code,
            other => panic!("not coded: {other:?}"),
        }
    }

    /// Downloads `url` the way an install does; returns the phase that failed,
    /// the insight tag and the user-facing code.
    async fn download(url: &str) -> (Phase, String, &'static str) {
        match crate::fs::create_http_stream(url, 0, 0, None).await {
            Ok((mut reader, _, insight)) => {
                let err = reader.read_to_end(&mut Vec::new()).await.unwrap_err();
                let tag = insight.lock().unwrap().error.clone().unwrap();
                (Phase::Body, tag, code_of(crate::fs::read_failure(err)))
            }
            Err(err) => {
                let tag = err.insight.unwrap().error.unwrap();
                (Phase::Send, tag, code_of(err.error))
            }
        }
    }

    #[tokio::test]
    async fn send_phase_failures() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let closed = format!("http://{}/f", listener.local_addr().unwrap());
        drop(listener);
        assert_eq!(
            download(&closed).await,
            (Phase::Send, "NET_CONNECT_REFUSED".into(), DOWNLOAD_REFUSED)
        );

        let url = serve(b"HTTP/1.1 503 Busy\r\nContent-Length: 0\r\n\r\n", false).await;
        assert_eq!(
            download(&url).await,
            (Phase::Send, "NET_HTTP_503".into(), SERVER_HTTP_ERROR)
        );
    }

    async fn refused(url: &str) -> crate::utils::error::TACommandError {
        match crate::fs::create_http_stream(url, 0, 0, None).await {
            Ok(_) => panic!("connected to a closed port"),
            Err(err) => err,
        }
    }

    fn coded(err: anyhow::Error) -> (&'static str, String) {
        match extract(&attach_download(err, None, None)) {
            Extracted::Coded(c) => (c.code, c.detail.unwrap()),
            other => panic!("not coded: {other:?}"),
        }
    }

    /// A send failure's detail starts with its tag. The elevated helper's
    /// uncoded failure crosses the pipe as text, and the launcher hangs the
    /// code and detail it would have hung for the same failure itself.
    #[tokio::test]
    async fn code_and_detail_survive_the_elevation_pipe() {
        use crate::ipc::{decode_frame, encode_frame, IpcError, PipeMsg};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let closed = format!("http://{}/f", listener.local_addr().unwrap());
        drop(listener);

        let local = coded(refused(&closed).await.error);
        assert_eq!(local.0, DOWNLOAD_REFUSED);
        assert!(local.1.starts_with("NET_CONNECT_REFUSED: "), "{}", local.1);

        let msg = PipeMsg::Err(
            "id".into(),
            IpcError::from_ta(&refused(&closed).await),
            Default::default(),
        );
        let frame = encode_frame(&msg).unwrap();
        let PipeMsg::Err(_, piped, _) = decode_frame(&frame[4..]).unwrap() else {
            panic!("variant lost");
        };
        assert_eq!(coded(piped.into_ta().error), local);
    }

    #[tokio::test]
    async fn body_phase_failures() {
        let short = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nabc", false).await;
        let reset = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nabc", true).await;
        let chunk = serve(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
            false,
        )
        .await;
        assert_eq!(
            download(&short).await,
            (Phase::Body, "NET_EOF".into(), DOWNLOAD_FAILED)
        );
        assert_eq!(
            download(&reset).await,
            (Phase::Body, "NET_RESET".into(), DOWNLOAD_FAILED)
        );
        assert_eq!(
            download(&chunk).await,
            (Phase::Body, "NET_PROTOCOL".into(), DOWNLOAD_FAILED)
        );
    }

    #[tokio::test]
    async fn middleware_causes_survive_reqwest() {
        // A middleware body error, through reqwest's wrapped body stream.
        let body = futures::stream::iter([Err::<bytes::Bytes, _>(io::Error::other(SftpStatus {
            code: 4,
            message: "failure".into(),
        }))]);
        let response: reqwest::Response =
            http::Response::new(reqwest::Body::wrap_stream(body)).into();
        let err = response.bytes().await.unwrap_err();
        assert_eq!(
            classify(&io::Error::other(err), Phase::Body),
            NetFailure::new("SFTP_STATUS:4", Class::Other, Phase::Body)
        );

        // Middleware send errors. `Middleware(anyhow)` is transparent, so its
        // source() skips the anyhow root when there is no context on top.
        let send = |root: anyhow::Error| {
            anyhow::Error::new(reqwest_middleware::Error::Middleware(root))
                .context("create_http_stream")
        };
        let timeout =
            send(anyhow::Error::new(io::Error::from(io::ErrorKind::TimedOut)).context("ssh"));
        assert_eq!(code_of(timeout), DOWNLOAD_TIMEOUT);
        let rejected = send(anyhow::Error::new(SshAuthRejected));
        assert_eq!(
            find(rejected.as_ref()).map(|f| (f.tag, f.code)),
            Some(("SSH_AUTH".into(), DOWNLOAD_REFUSED))
        );
    }

    #[test]
    fn local_io_is_not_a_network_failure() {
        let disk = anyhow::Error::new(io::Error::from(io::ErrorKind::StorageFull)).context("write");
        assert_eq!(find(disk.as_ref()), None);
        assert_eq!(code_of(disk), crate::utils::code::DISK_FULL);
    }
}
