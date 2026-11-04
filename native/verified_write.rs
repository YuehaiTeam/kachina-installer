//! 边写边校验的文件写出，安装器与 sfx 共用（sfx 经 `#[path]` 引入）。
//!
//! 内容写入目标的同时进入 hasher，写完核对字节数与 hash，不重新读一遍文件；需要时先在
//! 内存里抹掉文件开头的布局头，再计算 hash 与写出。只依赖外部 crate 与同为共享模块的
//! `pack_format`。

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use crate::pack_format::clear_layout;

/// 写出内容应有的大小与 hash。`md5` 优先于 `xxh`（与数据块命名一致）；两者都没有时
/// 大小与 hash 都不核对。
#[derive(Debug, Clone, Default)]
pub struct Expected {
    pub size: u64,
    pub md5: Option<String>,
    pub xxh: Option<String>,
}

#[derive(Debug)]
pub enum Failure {
    /// 读输入出错，包括解压
    Read(io::Error),
    /// 写目标出错
    Write(io::Error),
    Cancelled,
    Size {
        expected: u64,
        actual: u64,
    },
    Hash {
        expected: String,
        actual: String,
    },
}

/// 写入端检查到取消时返回的错误。经 hpatch 等中间层原样带回后由 [`is_cancel`] 识别。
#[derive(Debug)]
struct CancelMarker;

impl std::fmt::Display for CancelMarker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for CancelMarker {}

pub fn is_cancel(err: &io::Error) -> bool {
    err.get_ref().is_some_and(|e| e.is::<CancelMarker>())
}

const PROGRESS_FRAME: Duration = Duration::from_millis(20);

/// 字节进度回调，每 20ms 至多一次。
struct Throttle<'a> {
    report: Box<dyn FnMut(u64) + 'a>,
    last: Instant,
}

impl Throttle<'_> {
    fn tick(&mut self, done: u64, last_chunk: bool) {
        if last_chunk || self.last.elapsed() >= PROGRESS_FRAME {
            self.last = Instant::now();
            (self.report)(done);
        }
    }
}

enum Digest {
    Md5(chksum_md5::MD5),
    // 装进 Box 避免两个变体的体积差过大
    Xxh(Box<twox_hash::XxHash3_128>),
}

/// 抹布局头时先攒下的文件开头长度；布局头在 PE 的 DOS stub 里。
const HEAD_LEN: usize = 512;

/// 写入 `inner` 的同时计算 hash、累计字节数；`write` 开头检查取消。
pub struct HashingWriter<'a, W: Write> {
    inner: W,
    expected: Expected,
    digest: Option<Digest>,
    written: u64,
    /// 需要抹布局头时，文件开头攒满 `HEAD_LEN` 之前的内容
    head: Option<Vec<u8>>,
    cancelled: &'a dyn Fn() -> bool,
    progress: Option<Throttle<'a>>,
}

impl<'a, W: Write> HashingWriter<'a, W> {
    pub fn new(
        inner: W,
        expected: Expected,
        clear_layout_header: bool,
        cancelled: &'a dyn Fn() -> bool,
    ) -> Self {
        let digest = if expected.md5.is_some() {
            Some(Digest::Md5(chksum_md5::MD5::new()))
        } else if expected.xxh.is_some() {
            Some(Digest::Xxh(Box::new(twox_hash::XxHash3_128::new())))
        } else {
            None
        };
        Self {
            inner,
            expected,
            digest,
            written: 0,
            head: clear_layout_header.then(|| Vec::with_capacity(HEAD_LEN)),
            cancelled,
            progress: None,
        }
    }

    /// 按已写出字节数回报进度（20ms 节流，`finish` 时补报一次）。
    pub fn with_progress(mut self, report: impl FnMut(u64) + 'a) -> Self {
        self.progress = Some(Throttle {
            report: Box::new(report),
            last: Instant::now(),
        });
        self
    }

    fn emit(&mut self, buf: &[u8]) -> io::Result<()> {
        match &mut self.digest {
            Some(Digest::Md5(h)) => h.update(buf),
            Some(Digest::Xxh(h)) => h.write(buf),
            None => {}
        }
        self.inner.write_all(buf)?;
        self.written += buf.len() as u64;
        if let Some(progress) = &mut self.progress {
            progress.tick(self.written, false);
        }
        Ok(())
    }

    fn flush_head(&mut self) -> io::Result<()> {
        if let Some(mut head) = self.head.take() {
            clear_layout(&mut head);
            self.emit(&head)?;
        }
        Ok(())
    }

    /// 写出不足 `HEAD_LEN` 的文件开头、刷出缓冲并核对；返回内层写入器与写出字节数。
    pub fn finish(mut self) -> Result<(W, u64), Failure> {
        self.flush_head().map_err(Failure::Write)?;
        self.inner.flush().map_err(Failure::Write)?;
        if let Some(progress) = &mut self.progress {
            progress.tick(self.written, true);
        }
        let expected_hash = self.expected.md5.as_ref().or(self.expected.xxh.as_ref());
        if let (Some(expected), Some(digest)) = (expected_hash, self.digest) {
            if self.written != self.expected.size {
                return Err(Failure::Size {
                    expected: self.expected.size,
                    actual: self.written,
                });
            }
            let actual = match digest {
                Digest::Md5(h) => h.digest().to_hex_lowercase(),
                Digest::Xxh(h) => format!("{:x}", h.finish_128()),
            };
            if &actual != expected {
                return Err(Failure::Hash {
                    expected: expected.clone(),
                    actual,
                });
            }
        }
        Ok((self.inner, self.written))
    }
}

impl<W: Write> Write for HashingWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if (self.cancelled)() {
            return Err(io::Error::other(CancelMarker));
        }
        let Some(head) = &mut self.head else {
            self.emit(buf)?;
            return Ok(buf.len());
        };
        let take = (HEAD_LEN - head.len()).min(buf.len());
        head.extend_from_slice(&buf[..take]);
        if head.len() == HEAD_LEN {
            self.flush_head()?;
        }
        Ok(take)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// 把 `input` 全部写进 `writer`；读错与写错分开报告。
pub fn copy_into<W: Write>(
    input: &mut dyn Read,
    writer: &mut HashingWriter<'_, W>,
) -> Result<(), Failure> {
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Failure::Read(e)),
        };
        writer.write_all(&buf[..n]).map_err(|e| {
            if is_cancel(&e) {
                Failure::Cancelled
            } else {
                Failure::Write(e)
            }
        })?;
    }
}

/// 按已读字节数回报进度（20ms 节流，读满 `total` 时补报）。补丁按 diff 读量计进度。
pub struct ProgressReader<'a, R> {
    inner: R,
    read: u64,
    total: u64,
    progress: Throttle<'a>,
}

impl<'a, R: Read> ProgressReader<'a, R> {
    pub fn new(inner: R, total: u64, report: impl FnMut(u64) + 'a) -> Self {
        Self {
            inner,
            read: 0,
            total,
            progress: Throttle {
                report: Box::new(report),
                last: Instant::now(),
            },
        }
    }
}

impl<R: Read> Read for ProgressReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        self.progress.tick(self.read, self.read >= self.total);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARKER_AT: usize = 0x4E;

    fn image() -> Vec<u8> {
        let mut bytes: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        bytes[MARKER_AT..MARKER_AT + 18].copy_from_slice(crate::pack_format::LAYOUT_MARKER);
        bytes[MARKER_AT + 18..MARKER_AT + 38].fill(0xAB);
        bytes
    }

    fn xxh(bytes: &[u8]) -> String {
        let mut h = twox_hash::XxHash3_128::new();
        h.write(bytes);
        format!("{:x}", h.finish_128())
    }

    fn write_in_chunks(
        bytes: &[u8],
        expected: Expected,
        clear: bool,
        chunk: usize,
    ) -> Result<(Vec<u8>, u64), Failure> {
        let never = || false;
        let mut writer = HashingWriter::new(Vec::new(), expected, clear, &never);
        for part in bytes.chunks(chunk) {
            writer.write_all(part).map_err(Failure::Write)?;
        }
        writer.finish()
    }

    #[test]
    fn layout_header_is_cleared_before_hashing_across_split_writes() {
        let original = image();
        let mut cleared = original.clone();
        cleared[MARKER_AT + 18..MARKER_AT + 38].fill(0);
        let expected = Expected {
            size: cleared.len() as u64,
            md5: None,
            xxh: Some(xxh(&cleared)),
        };
        // 7 字节一块：布局头跨多次 write
        let (out, written) = write_in_chunks(&original, expected, true, 7).unwrap();
        assert_eq!(out, cleared);
        assert_eq!(written, cleared.len() as u64);
    }

    #[test]
    fn short_file_with_clearing_is_still_written() {
        let bytes = b"tiny".to_vec();
        let expected = Expected {
            size: 4,
            md5: Some(chksum_md5::hash(&bytes).to_hex_lowercase()),
            xxh: None,
        };
        let (out, _) = write_in_chunks(&bytes, expected, true, 64).unwrap();
        assert_eq!(out, bytes);
    }

    #[test]
    fn size_and_hash_mismatches_are_reported() {
        let bytes = image();
        let good = xxh(&bytes);
        let wrong_size = Expected {
            size: bytes.len() as u64 + 1,
            md5: None,
            xxh: Some(good.clone()),
        };
        assert!(matches!(
            write_in_chunks(&bytes, wrong_size, false, 1000),
            Err(Failure::Size { .. })
        ));
        let wrong_hash = Expected {
            size: bytes.len() as u64,
            md5: None,
            xxh: Some("0".into()),
        };
        assert!(matches!(
            write_in_chunks(&bytes, wrong_hash, false, 1000),
            Err(Failure::Hash { .. })
        ));
        // 没有期望 hash 时不核对
        assert!(write_in_chunks(&bytes, Expected::default(), false, 1000).is_ok());
    }

    #[test]
    fn cancellation_stops_the_copy() {
        let cancelled = || true;
        let mut writer = HashingWriter::new(Vec::new(), Expected::default(), false, &cancelled);
        let mut input: &[u8] = b"data";
        assert!(matches!(
            copy_into(&mut input, &mut writer),
            Err(Failure::Cancelled)
        ));
    }
}
