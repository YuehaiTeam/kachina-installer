pub mod install_file;
pub mod manager;
pub mod operation;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::dfs::InsightItem;
use crate::fs::commit::{CommitOutcome, RecoverOutcome};
use crate::fs::LocalScan;
use crate::installer::uninstall::{StagedImage, UninstallOutcome};
use crate::thirdparty::mirrorc::MirrorcExtract;
use crate::utils::error::TACommandError;

use install_file::{InstallResult, MultichunkResult};

/// 管道帧：4 字节小端长度 + postcard 编码体。上限只防御帧头错位，正常载荷远小于此。
const MAX_FRAME: usize = 64 * 1024 * 1024;

pub fn encode_frame<T: Serialize>(msg: &T) -> std::io::Result<Vec<u8>> {
    let body = postcard::to_stdvec(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if body.len() > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "ipc frame too large",
        ));
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn decode_frame<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> std::io::Result<T> {
    postcard::from_bytes(bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// 对端关闭时返回 `Ok(None)`；帧头之后的 EOF 视为错误。
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match reader.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "ipc frame too large",
        ));
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await?;
    Ok(Some(body))
}

// `Chunk` 用元组：u32/u64 不同型，位置写反编译不过；`BytesOf`/`CountOf`/`Extract`
// 的同型字段保留具名，写反不会被编译器发现。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    Bytes(u64),
    Chunk(u32, u64),
    BytesOf { done: u64, total: u64 },
    CountOf { done: u64, total: u64 },
    Extract { file: String, done: u64, total: u64 },
    Delete(String),
}

pub type ProgressNotify = Arc<dyn Fn(Progress) + Send + Sync>;

pub fn progress_notify(f: impl Fn(Progress) + Send + Sync + 'static) -> ProgressNotify {
    Arc::new(f)
}

pub fn progress_noop() -> ProgressNotify {
    progress_notify(|_| {})
}

/// 跨提权管道的错误。`code` 为空表示未挂码，此时 `message` 是整条链的 `{:#}`；
/// 有码时 `message` 是 `Coded.detail`。`Coded.code` 是 `&'static str`，父进程用
/// `code_from_str` 映射回常量，未知码按未挂码处理。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct IpcError {
    pub message: String,
    pub code: Option<String>,
    pub subject: Option<String>,
    pub sid: Option<String>,
    pub cancelled: bool,
    pub insight: Option<InsightItem>,
}

impl IpcError {
    pub fn from_ta(err: &TACommandError) -> Self {
        use crate::utils::code::{extract, Extracted};
        let (message, code, subject, sid, cancelled) = match extract(&err.error) {
            Extracted::Coded(c) => (
                c.detail.clone().unwrap_or_default(),
                Some(c.code.to_string()),
                c.subject.clone(),
                c.sid.clone(),
                false,
            ),
            Extracted::Cancelled => (String::new(), None, None, None, true),
            Extracted::Uncoded { detail } => (detail, None, None, None, false),
        };
        Self {
            message,
            code,
            subject,
            sid,
            cancelled,
            insight: err.insight.clone(),
        }
    }

    pub fn into_ta(self) -> TACommandError {
        use crate::utils::code::{code_from_str, Cancelled, Coded};
        let error = if self.cancelled {
            anyhow::Error::new(Cancelled)
        } else if let Some(code) = self.code.as_deref().and_then(code_from_str) {
            let mut coded = Coded::bare(code);
            coded.detail = Some(self.message).filter(|s| !s.is_empty());
            coded.subject = self.subject;
            coded.sid = self.sid;
            anyhow::Error::new(coded)
        } else {
            anyhow::anyhow!(self.message)
        };
        TACommandError {
            error,
            insight: self.insight,
        }
    }
}

/// Staging directory opened for a session (see `fs::staging::Staging::open`).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StagingOpened {
    pub root: String,
    /// Journal text left by an interrupted commit.
    pub journal: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum IpcResult {
    Ping,
    InstallFile(InstallResult),
    InstallMultichunkStream(MultichunkResult),
    CreateLnk,
    WriteRegistry,
    StageSelfImage(Vec<StagedImage>),
    RunUninstall(UninstallOutcome),
    FindProcessByName(Vec<(u32, String)>),
    KillProcess,
    InstallRuntime(String),
    CheckLocalFiles(LocalScan),
    ProbeWritable(Vec<String>),
    RunMirrorcDownload,
    /// 归档内 `.metadata.json` 原文与解出的文件清单。`RepoMetadata` 带
    /// `skip_serializing_if`，不能位置编码，原文交回会话侧解析。
    RunMirrorcInstall(MirrorcExtract),
    OpenStaging(StagingOpened),
    Commit(CommitOutcome),
    Recover(RecoverOutcome),
    DiscardStaging,
}

impl IpcResult {
    pub fn insight(&self) -> Option<InsightItem> {
        match self {
            IpcResult::InstallFile(r) => r.insight.clone(),
            IpcResult::InstallMultichunkStream(r) => Some(r.insight.clone()),
            _ => None,
        }
    }
}

// `Envelope` 与 `Breadcrumb` 都是 JSON 文本：前者主进程不解析直接转发，后者是
// `serde_json::Value`，而 postcard 不支持 `deserialize_any`。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum PipeMsg {
    Progress(String, Progress),
    Ok(String, IpcResult),
    Err(String, IpcError),
    Envelope(String),
    Breadcrumb(String),
    Disconnect(String),
}

#[cfg(test)]
mod tests {
    use super::install_file::{InstallFileArgs, InstallFileMode, InstallFileSource};
    use super::operation::IpcOperation;
    use super::*;

    fn insight(err: Option<&str>) -> InsightItem {
        InsightItem {
            url: "https://x.example/f".into(),
            ttfb: 12,
            time: 34,
            size: 56,
            error: err.map(str::to_string),
            range: vec![(0, 99), (200, 299)],
            mode: Some("direct".into()),
        }
    }

    fn roundtrip<T: Serialize + for<'a> Deserialize<'a>>(msg: &T) -> T {
        let frame = encode_frame(msg).unwrap();
        assert_eq!(
            u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize,
            frame.len() - 4
        );
        decode_frame(&frame[4..]).unwrap()
    }

    #[test]
    fn pipe_msg_shapes_roundtrip() {
        let multi = roundtrip(&PipeMsg::Ok(
            "id".into(),
            IpcResult::InstallMultichunkStream(MultichunkResult {
                results: vec![
                    Ok(10),
                    Err(IpcError {
                        message: "boom".into(),
                        code: None,
                        subject: None,
                        sid: None,
                        cancelled: false,
                        insight: Some(insight(Some("HASH_MISMATCH_ERR"))),
                    }),
                ],
                insight: insight(None),
            }),
        ));
        let PipeMsg::Ok(id, IpcResult::InstallMultichunkStream(r)) = multi else {
            panic!("variant lost");
        };
        assert_eq!(id, "id");
        assert!(matches!(r.results[0], Ok(10)));
        assert_eq!(
            r.results[1]
                .as_ref()
                .unwrap_err()
                .insight
                .as_ref()
                .unwrap()
                .error
                .as_deref(),
            Some("HASH_MISMATCH_ERR")
        );
        assert_eq!(r.insight.range, vec![(0, 99), (200, 299)]);

        let progress = roundtrip(&PipeMsg::Progress("p".into(), Progress::Chunk(3, 4096)));
        assert!(matches!(
            progress,
            PipeMsg::Progress(_, Progress::Chunk(3, 4096))
        ));

        let meta = roundtrip(&PipeMsg::Ok(
            "m".into(),
            IpcResult::RunMirrorcInstall(MirrorcExtract {
                metadata: Some("{\"tag_name\":\"1\"}".into()),
                files: vec![("a/b.dll".into(), "h".into())],
                deletes: vec!["gone".into()],
            }),
        ));
        assert!(matches!(
            meta,
            PipeMsg::Ok(_, IpcResult::RunMirrorcInstall(m))
                if m.metadata.as_deref().is_some_and(|s| s.contains("tag_name"))
                    && m.files.len() == 1 && m.deletes == vec!["gone".to_string()]
        ));
    }

    #[test]
    fn coded_error_survives_pipe() {
        use crate::utils::code::{extract, Attach, Coded, Extracted, DOWNLOAD_TIMEOUT, PERMISSION_DENIED};

        let err = TACommandError::new(
            Coded::bare_with(DOWNLOAD_TIMEOUT, "cdn.example")
                .with_sid("sid-1")
                .wrap(anyhow::anyhow!("timed out (https://cdn.example/a)")),
        );
        let back = roundtrip(&PipeMsg::Err("e".into(), IpcError::from_ta(&err)));
        let PipeMsg::Err(_, ipc) = back else {
            panic!("variant lost");
        };
        let ta = ipc.into_ta();
        let Extracted::Coded(c) = extract(&ta.error) else {
            panic!("code lost across pipe");
        };
        assert_eq!(c.code, DOWNLOAD_TIMEOUT);
        assert_eq!(c.subject.as_deref(), Some("cdn.example"));
        assert_eq!(c.sid.as_deref(), Some("sid-1"));
        let detail = c.detail.as_deref().unwrap();
        assert!(detail.contains("timed out") && !detail.contains("https://"));

        let bare = TACommandError::new(anyhow::anyhow!("os error 5").attach(PERMISSION_DENIED));
        let ta = IpcError::from_ta(&bare).into_ta();
        assert!(matches!(extract(&ta.error), Extracted::Coded(c) if c.code == PERMISSION_DENIED));

        let plain = TACommandError::new(anyhow::anyhow!("ipc shape"));
        let ta = IpcError::from_ta(&plain).into_ta();
        assert!(matches!(extract(&ta.error), Extracted::Uncoded { detail } if detail.contains("ipc shape")));
    }

    #[test]
    fn operation_with_default_fields_roundtrip() {
        let op = IpcOperation::InstallFile(InstallFileArgs {
            mode: InstallFileMode::HybridPatch {
                diff: InstallFileSource::Url {
                    url: "https://x.example/d".into(),
                    offset: 1,
                    size: 2,
                    skip_decompress: false,
                    request_range: None,
                },
                source: InstallFileSource::Local {
                    offset: 3,
                    size: 4,
                    skip_decompress: true,
                },
            },
            target: "C:\\staged\\new\\a.dll".into(),
            old: Some("C:\\app\\a.dll".into()),
            md5: None,
            xxh: Some("ff".into()),
            clear_installer_index_mark: Some(false),
        });
        let back = roundtrip(&op);
        let IpcOperation::InstallFile(args) = back else {
            panic!("variant lost");
        };
        assert_eq!(args.target, "C:\\staged\\new\\a.dll");
        assert_eq!(args.old.as_deref(), Some("C:\\app\\a.dll"));
        assert_eq!(args.xxh.as_deref(), Some("ff"));
        let InstallFileMode::HybridPatch { diff, source } = args.mode else {
            panic!("mode lost");
        };
        assert!(matches!(
            diff,
            InstallFileSource::Url {
                offset: 1,
                size: 2,
                skip_decompress: false,
                ..
            }
        ));
        assert!(matches!(
            source,
            InstallFileSource::Local {
                offset: 3,
                size: 4,
                skip_decompress: true
            }
        ));
    }

    #[tokio::test]
    async fn read_frame_splits_stream_and_reports_eof() {
        let mut stream = encode_frame(&PipeMsg::Disconnect("a".into())).unwrap();
        stream.extend(encode_frame(&PipeMsg::Envelope("b".into())).unwrap());
        let mut reader = &stream[..];
        let first: PipeMsg =
            decode_frame(&read_frame(&mut reader).await.unwrap().unwrap()).unwrap();
        let second: PipeMsg =
            decode_frame(&read_frame(&mut reader).await.unwrap().unwrap()).unwrap();
        assert!(matches!(first, PipeMsg::Disconnect(s) if s == "a"));
        assert!(matches!(second, PipeMsg::Envelope(s) if s == "b"));
        assert!(read_frame(&mut reader).await.unwrap().is_none());

        let mut truncated = &stream[..6];
        assert!(read_frame(&mut truncated).await.is_err());

        let mut oversized = &[0xffu8, 0xff, 0xff, 0x7f, 0][..];
        assert!(read_frame(&mut oversized).await.is_err());
    }
}
