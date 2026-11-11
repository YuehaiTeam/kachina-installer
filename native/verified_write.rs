//! 边写边校验、写完读回的文件写出，安装器与 sfx 共用（sfx 经 `#[path]` 引入）。
//!
//! 内容写入目标的同时进入 hasher，写完核对字节数与 hash：这一步核对的是交给写入端的
//! 数据。文件再从写入用的句柄读回一遍（[`Written::read_back`]），核对文件系统交回的内容
//! 与写入的一致：摘要算出之后数据还要经过写缓冲、页缓存和过滤驱动。需要时先在内存里抹掉
//! 文件开头的布局头，再计算 hash 与写出。只依赖外部 crate 与同为共享模块的 `pack_format`。

use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, Write};
use std::path::Path;
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

/// 摘要算法，名字同 metadata 的 hash 算法名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Md5,
    Xxh,
}

impl Algorithm {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "md5" => Some(Self::Md5),
            "xxh" => Some(Self::Xxh),
            _ => None,
        }
    }
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
    /// 读回目标出错
    ReadBack(io::Error),
    /// 读回的内容与写入的不同。写入的数据已通过 [`Failure::Size`] 与 [`Failure::Hash`] 的
    /// 核对，问题出在写入端之后；两个字段是各自的字节数与摘要。
    Altered {
        written: String,
        read: String,
    },
}

/// 一次写出的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub bytes: u64,
    /// 写出内容的摘要；写入器没有摘要算法时为 `None`。
    digest: Option<(Algorithm, String)>,
}

impl Written {
    /// 写出内容的摘要，格式同 metadata 里的 hash。
    pub fn hash(&self) -> Option<&str> {
        self.digest.as_ref().map(|(_, hash)| hash.as_str())
    }

    fn describe(bytes: u64, hash: Option<&str>) -> String {
        match hash {
            Some(hash) => format!("{bytes} bytes {hash}"),
            None => format!("{bytes} bytes"),
        }
    }

    /// 从 `file` 的开头读到末尾，字节数与摘要须同写入时一致，否则为 [`Failure::Altered`]。
    /// 没有摘要时只核对字节数。读到的是文件系统此刻交回的内容，通常来自页缓存，介质上的
    /// 错误不在核对之内。每读一块前检查 `cancelled`。
    pub fn read_back<F: Read + Seek>(
        &self,
        file: &mut F,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), Failure> {
        file.rewind().map_err(Failure::ReadBack)?;
        let mut digest = self.digest.as_ref().map(|(algo, _)| Digest::new(*algo));
        let mut buf = vec![0u8; 256 * 1024];
        let mut bytes = 0u64;
        loop {
            if cancelled() {
                return Err(Failure::Cancelled);
            }
            let n = match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Failure::ReadBack(e)),
            };
            if let Some(digest) = &mut digest {
                digest.update(&buf[..n]);
            }
            bytes += n as u64;
        }
        let hash = digest.map(Digest::finish);
        if bytes != self.bytes || hash.as_deref() != self.hash() {
            return Err(Failure::Altered {
                written: Self::describe(self.bytes, self.hash()),
                read: Self::describe(bytes, hash.as_deref()),
            });
        }
        Ok(())
    }
}

/// 以读写方式新建文件，已存在则截断：写完后的读回用的是同一个句柄。
pub fn create_file(path: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
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

impl Digest {
    fn new(algorithm: Algorithm) -> Self {
        match algorithm {
            Algorithm::Md5 => Self::Md5(chksum_md5::MD5::new()),
            Algorithm::Xxh => Self::Xxh(Box::new(twox_hash::XxHash3_128::new())),
        }
    }

    fn algorithm(&self) -> Algorithm {
        match self {
            Self::Md5(_) => Algorithm::Md5,
            Self::Xxh(_) => Algorithm::Xxh,
        }
    }

    fn update(&mut self, buf: &[u8]) {
        match self {
            Self::Md5(h) => h.update(buf),
            Self::Xxh(h) => h.write(buf),
        }
    }

    fn finish(self) -> String {
        match self {
            Self::Md5(h) => h.digest().to_hex_lowercase(),
            Self::Xxh(h) => format!("{:x}", h.finish_128()),
        }
    }
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
            Some(Digest::new(Algorithm::Md5))
        } else if expected.xxh.is_some() {
            Some(Digest::new(Algorithm::Xxh))
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

    /// 没有期望 hash 时也按 `algorithm` 计算摘要，供读回核对与调用方取用；有期望 hash 时
    /// 算法随期望值，本调用不改变它。须在写入任何内容之前调用。
    pub fn with_algorithm(mut self, algorithm: Algorithm) -> Self {
        if self.digest.is_none() {
            self.digest = Some(Digest::new(algorithm));
        }
        self
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
        if let Some(digest) = &mut self.digest {
            digest.update(buf);
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

    /// 写出不足 `HEAD_LEN` 的文件开头、刷出缓冲并核对；返回内层写入器与写出结果。
    pub fn finish(mut self) -> Result<(W, Written), Failure> {
        self.flush_head().map_err(Failure::Write)?;
        self.inner.flush().map_err(Failure::Write)?;
        if let Some(progress) = &mut self.progress {
            progress.tick(self.written, true);
        }
        let digest = self
            .digest
            .take()
            .map(|digest| (digest.algorithm(), digest.finish()));
        let expected_hash = self.expected.md5.as_ref().or(self.expected.xxh.as_ref());
        if let (Some(expected), Some((_, actual))) = (expected_hash, &digest) {
            if self.written != self.expected.size {
                return Err(Failure::Size {
                    expected: self.expected.size,
                    actual: self.written,
                });
            }
            if actual != expected {
                return Err(Failure::Hash {
                    expected: expected.clone(),
                    actual: actual.clone(),
                });
            }
        }
        Ok((
            self.inner,
            Written {
                bytes: self.written,
                digest,
            },
        ))
    }
}

impl HashingWriter<'_, BufWriter<File>> {
    /// [`finish`](Self::finish)，再从写入用的句柄读回核对（[`Written::read_back`]）。
    /// 交回的句柄位置在文件末尾，尚未刷盘。
    pub fn finish_verified(self) -> Result<(File, Written), Failure> {
        let cancelled = self.cancelled;
        let (buffered, written) = self.finish()?;
        let mut file = buffered
            .into_inner()
            .map_err(|e| Failure::Write(e.into_error()))?;
        written.read_back(&mut file, cancelled)?;
        Ok((file, written))
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

/// 读回核对的测试共用的输入。
#[cfg(test)]
pub(crate) mod testing {
    use std::io::{self, Read, Write};
    use std::path::{Path, PathBuf};

    /// `len` 字节的伪随机内容。
    pub(crate) fn bytes(len: usize) -> Vec<u8> {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        (0..len)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect()
    }

    /// 交出 `data` 的全部内容，读到末尾时经另一个句柄翻转 `path` 的第一个字节：此时写入方
    /// 已把文件开头写进文件，还没有读回。`data` 须长于写入方的缓冲（256 KiB 的两倍以上），
    /// 否则文件开头还在缓冲里，改动会被随后的写入盖掉。
    pub(crate) struct TamperAtEnd {
        data: io::Cursor<Vec<u8>>,
        path: PathBuf,
        tampered: bool,
    }

    impl TamperAtEnd {
        pub(crate) fn new(data: Vec<u8>, path: &Path) -> Self {
            Self {
                data: io::Cursor::new(data),
                path: path.to_path_buf(),
                tampered: false,
            }
        }
    }

    impl Read for TamperAtEnd {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.data.read(buf)?;
            if n == 0 && !self.tampered {
                self.tampered = true;
                let first = self.data.get_ref()[0];
                let mut file = std::fs::OpenOptions::new().write(true).open(&self.path)?;
                file.write_all(&[first ^ 1])?;
            }
            Ok(n)
        }
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
        writer.finish().map(|(out, written)| (out, written.bytes))
    }

    #[test]
    fn read_back_accepts_what_was_written_and_reports_what_changed() {
        let bytes = image();
        let never = || false;
        let mut writer = HashingWriter::new(
            io::Cursor::new(Vec::new()),
            Expected::default(),
            false,
            &never,
        )
        .with_algorithm(Algorithm::Xxh);
        writer.write_all(&bytes).unwrap();
        let (mut stored, written) = writer.finish().unwrap();
        assert_eq!(written.bytes, bytes.len() as u64);
        assert_eq!(written.hash(), Some(xxh(&bytes).as_str()));
        written.read_back(&mut stored, &never).unwrap();

        // 长度不变、内容变了
        stored.get_mut()[100] ^= 0x40;
        assert!(matches!(
            written.read_back(&mut stored, &never),
            Err(Failure::Altered { .. })
        ));
        // 被截短
        stored.get_mut()[100] ^= 0x40;
        stored.get_mut().truncate(bytes.len() - 1);
        assert!(matches!(
            written.read_back(&mut stored, &never),
            Err(Failure::Altered { .. })
        ));
        let cancelled = || true;
        assert!(matches!(
            written.read_back(&mut stored, &cancelled),
            Err(Failure::Cancelled)
        ));
    }

    /// 写入时的 hash 核对通过，文件在读回之前被另一个句柄改动：`finish_verified` 报
    /// [`Failure::Altered`]；没有改动时交回文件与写入内容的摘要。
    #[test]
    fn finish_verified_fails_a_file_changed_after_the_write() {
        let bytes = testing::bytes(600 * 1024);
        let dir = std::env::temp_dir().join(format!("kachina-verified-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.bin");
        let expected = Expected {
            size: bytes.len() as u64,
            md5: None,
            xxh: Some(xxh(&bytes)),
        };
        let never = || false;
        let write = |input: &mut dyn Read| {
            let file = BufWriter::with_capacity(256 * 1024, create_file(&path).unwrap());
            let mut writer = HashingWriter::new(file, expected.clone(), false, &never);
            copy_into(input, &mut writer)?;
            writer.finish_verified().map(|(_, written)| written)
        };

        let written = write(&mut bytes.as_slice()).unwrap();
        assert_eq!(written.hash(), Some(xxh(&bytes).as_str()));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);

        let mut tampering = testing::TamperAtEnd::new(bytes.clone(), &path);
        assert!(matches!(
            write(&mut tampering),
            Err(Failure::Altered { .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
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
