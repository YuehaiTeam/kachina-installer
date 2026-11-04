//! 运行管线：还原目录树 → 拉起应用 → 等待退出 → 清理临时目录。

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use async_compression::tokio::bufread::ZstdDecoder;
use fmmap::tokio::{AsyncMmapFile, AsyncMmapFileExt};

use crate::metadata::{FileMeta, RepoMetadata};
use crate::pack_format::{
    preferred_file_hash, read_json, relative_under_root, scan_embedded, Embedded,
};
use crate::{dialog, process, reaper, uac};

/// \0CONFIG 中 sfx 关心的子集；serde 忽略其余字段，与安装器配置双向兼容。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SfxConfig {
    exe_name: String,
    #[serde(default)]
    app_name: Option<String>,
    #[serde(default)]
    window_title: Option<String>,
    #[serde(default = "default_uac")]
    uac_strategy: String,
}

fn default_uac() -> String {
    "prefer-admin".to_string()
}

/// 用户在进度框点了取消或关掉了它。
#[derive(Debug)]
struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// 程序入口：返回进程退出码（正常路径透传应用退出码）。
pub async fn run(console: bool) -> i32 {
    // 先清理历史残留，再做正事；失败不阻塞本次运行。
    reaper::sweep();

    let (mmap, embedded, config) = match load().await {
        Ok(loaded) => loaded,
        Err(err) => {
            report_error(console, "kachina-sfx", &format!("{err:#}"));
            return 1;
        }
    };
    let title = config
        .window_title
        .clone()
        .or_else(|| config.app_name.clone())
        .unwrap_or_else(|| "kachina-sfx".to_string());
    match run_package(console, &mmap, &embedded, &config, &title).await {
        Ok(code) => code,
        // 临时目录已清理，用户主动取消不弹错误
        Err(err) if err.downcast_ref::<Cancelled>().is_some() => 1,
        Err(err) => {
            report_error(console, &title, &format!("{err:#}"));
            1
        }
    }
}

fn report_error(console: bool, title: &str, message: &str) {
    if console {
        eprintln!("kachina-sfx: {message}");
    } else {
        dialog::show_error(title, message);
    }
}

async fn load() -> Result<(AsyncMmapFile, Vec<Embedded>, SfxConfig)> {
    let mmap = open_self().await?;
    let embedded = scan_embedded(mmap.as_slice());
    if embedded.is_empty() {
        bail!("此程序不含自解压数据");
    }
    // 错误文本里的块名写成可见的 `\0`：真正的 NUL 会截断对话框里的宽字符串。
    let config = read_json(mmap.as_slice(), &embedded, "\0CONFIG")
        .context("LOCAL_CONFIG_ERR: malformed \\0CONFIG")?
        .context("包内缺少 \\0CONFIG")?;
    Ok((mmap, embedded, config))
}

async fn open_self() -> Result<AsyncMmapFile> {
    let exe = std::env::current_exe().context("CURRENT_EXE_ERR")?;
    // 与安装器一致的调试约定：debug 构建读同目录的 .bin 副本（pack 产物）。
    let path = if cfg!(debug_assertions) {
        let bin = exe.with_extension("bin");
        if bin.exists() {
            bin
        } else {
            exe
        }
    } else {
        exe
    };
    AsyncMmapFile::open(path).await.context("MMAP_ERR")
}

/// 解压、运行、清理。创建临时目录之后的任何失败都先删掉临时目录再返回。
async fn run_package(
    console: bool,
    mmap: &AsyncMmapFile,
    embedded: &[Embedded],
    config: &SfxConfig,
    title: &str,
) -> Result<i32> {
    let meta: RepoMetadata = read_json(mmap.as_slice(), embedded, "\0META")
        .context("LOCAL_CONFIG_ERR: malformed \\0META")?
        .context("此包不含离线数据，不能作为自解压程序运行")?;
    if meta.hashed.is_empty() {
        bail!("此包不含离线数据，不能作为自解压程序运行");
    }

    // 全部路径与主程序先校验再落盘：任何一条不合法即整体失败，不留下半次提取。
    let plan = plan_extraction(embedded, &meta.hashed)?;
    let exe = exe_in_plan(&plan, &config.exe_name)?;

    let temp_dir = create_temp_dir()?;
    let result = run_staged(console, mmap, &plan, &temp_dir, &exe, config, title).await;
    if result.is_err() {
        cleanup_temp(&temp_dir).await;
    }
    result
}

#[derive(Debug, Clone)]
struct ExtractItem {
    /// meta 中的相对路径，用于报错
    file_name: String,
    /// 校验过的相对路径，只含普通组件
    rel: PathBuf,
    output_size: u64,
    md5: Option<String>,
    xxh: Option<String>,
    block: Embedded,
}

fn plan_extraction(embedded: &[Embedded], hashed: &[FileMeta]) -> Result<Vec<ExtractItem>> {
    let mut plan = Vec::with_capacity(hashed.len());
    for file in hashed {
        let rel = relative_under_root(Path::new(""), &file.file_name)
            .with_context(|| format!("包内路径不安全: {:?}", file.file_name))?;
        let hash = preferred_file_hash(&file.md5, &file.xxh)
            .with_context(|| format!("{} 缺少校验 hash", file.file_name))?;
        let block = embedded
            .iter()
            .find(|e| &e.name == hash)
            .with_context(|| format!("包内缺少数据块 {hash}（{}）", file.file_name))?;
        plan.push(ExtractItem {
            file_name: file.file_name.clone(),
            rel,
            output_size: file.size,
            md5: file.md5.clone(),
            xxh: file.xxh.clone(),
            block: block.clone(),
        });
    }
    Ok(plan)
}

/// `exeName` 与 meta 路径同属包内输入：按同一规则校验，并须是本次解压的文件之一
/// （不区分大小写）。
fn exe_in_plan(plan: &[ExtractItem], exe_name: &str) -> Result<PathBuf> {
    let rel = relative_under_root(Path::new(""), exe_name)
        .with_context(|| format!("exeName 路径不安全: {exe_name:?}"))?;
    let key = rel.to_string_lossy().to_lowercase();
    if !plan
        .iter()
        .any(|item| item.rel.to_string_lossy().to_lowercase() == key)
    {
        bail!("包内没有应用主程序 {exe_name}");
    }
    Ok(rel)
}

fn create_temp_dir() -> Result<PathBuf> {
    let base = std::env::temp_dir();
    for _ in 0..16 {
        let dir = base.join(format!(
            "{}{}",
            reaper::TEMP_PREFIX,
            uuid::Uuid::new_v4().simple()
        ));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).context("创建临时目录失败"),
        }
    }
    bail!("无法分配不重名的临时目录")
}

/// 解压、拉起并等待应用、清理。任何失败由调用方清理临时目录。
async fn run_staged(
    console: bool,
    mmap: &AsyncMmapFile,
    plan: &[ExtractItem],
    temp_dir: &Path,
    exe: &Path,
    config: &SfxConfig,
    title: &str,
) -> Result<i32> {
    let total: u64 = plan.iter().map(|i| i.output_size).sum();
    if let Some(free) = free_space(temp_dir) {
        if free < total + total / 10 {
            bail!("临时目录所在卷空间不足：需要约 {total} 字节，可用 {free} 字节");
        }
    }

    let progress = if console {
        None
    } else {
        dialog::Progress::show(title, "正在解压…", total).ok()
    };
    for item in plan {
        extract_one(mmap, item, temp_dir, progress.as_ref()).await?;
    }
    if let Some(p) = progress {
        if p.cancelled() {
            return Err(Cancelled.into());
        }
        p.close().await;
    }

    let code = launch_and_wait(&temp_dir.join(exe), &config.uac_strategy).await?;
    cleanup_temp(temp_dir).await;
    Ok(code as i32)
}

/// 解码一个数据块到临时目录，边写边算 hash；失败时删掉半成品文件。
/// 临时目录是本进程刚建的空目录，`rel` 只含普通组件，路径不会落到目录之外。
async fn extract_one(
    mmap: &AsyncMmapFile,
    item: &ExtractItem,
    temp_dir: &Path,
    progress: Option<&dialog::Progress>,
) -> Result<()> {
    let out_path = temp_dir.join(&item.rel);
    if let Some(parent) = out_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("创建目录失败 {}", parent.display()))?;
    }

    let result = decode_verify_write(mmap, item, &out_path, progress).await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&out_path).await;
    }
    result
}

async fn decode_verify_write(
    mmap: &AsyncMmapFile,
    item: &ExtractItem,
    out_path: &Path,
    progress: Option<&dialog::Progress>,
) -> Result<()> {
    let reader = mmap.range_reader(item.block.offset, item.block.size)?;
    let mut decoder = ZstdDecoder::new(reader);
    let file = tokio::fs::File::create(out_path)
        .await
        .with_context(|| format!("创建文件失败 {}", out_path.display()))?;
    let mut writer = tokio::io::BufWriter::new(file);

    // 与安装器 verify_hash 同一语义：md5 优先于 xxh；xxh 为 XxHash3_128 的
    // finish_128 十六进制。
    enum Hasher {
        Md5(chksum_md5::MD5),
        // 装进 Box 避免两个变体的体积差过大
        Xxh(Box<twox_hash::XxHash3_128>),
    }
    let mut hasher = if item.md5.is_some() {
        Hasher::Md5(chksum_md5::MD5::new())
    } else {
        Hasher::Xxh(Box::new(twox_hash::XxHash3_128::new()))
    };

    let mut buf = [0u8; 65536];
    let mut written: u64 = 0;
    loop {
        if progress.is_some_and(dialog::Progress::cancelled) {
            return Err(Cancelled.into());
        }
        let n = decoder.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).await?;
        match &mut hasher {
            Hasher::Md5(h) => h.update(&buf[..n]),
            Hasher::Xxh(h) => h.write(&buf[..n]),
        }
        written += n as u64;
        if let Some(p) = progress {
            p.add_bytes(n as u64);
        }
    }
    writer.flush().await?;

    if written != item.output_size {
        bail!(
            "{} 大小校验失败：期望 {} 字节，实际 {} 字节",
            item.file_name,
            item.output_size,
            written
        );
    }
    let expected = item.md5.as_ref().or(item.xxh.as_ref());
    if let Some(expected) = expected {
        let actual = match hasher {
            Hasher::Md5(h) => h.digest().to_hex_lowercase(),
            Hasher::Xxh(h) => format!("{:x}", h.finish_128()),
        };
        if &actual != expected {
            bail!(
                "{} 校验失败：期望 {expected}，实际 {actual}",
                item.file_name
            );
        }
    }
    Ok(())
}

/// 按 uacStrategy 拉起应用主体并等待退出，透传退出码。
/// sfx 自身恒为 asInvoker；提权只发生在这一次启动上。
async fn launch_and_wait(exe: &Path, strategy: &str) -> Result<u32> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();

    let self_elevated = uac::check_elevated().unwrap_or(false);
    // 未知取值与 prefer-admin 同义，与安装器 elevate_from_state 的默认分支一致
    let want_elevated = !self_elevated && strategy != "prefer-user";

    if !want_elevated {
        return spawn_and_wait(exe, &args).await;
    }

    match uac::run_elevated(exe, process::join_args(&args)) {
        // SAFETY: SEE_MASK_NOCLOSEPROCESS 返回的进程句柄归调用方所有，交给 Child 关闭
        Ok(handle) => unsafe { process::Child::from_raw(handle.0) }
            .wait()
            .await
            .context("等待应用退出失败"),
        Err(e) if is_uac_declined(&e) && strategy == "force" => {
            bail!("用户取消了管理员授权，应用未运行")
        }
        // prefer-admin：用户取消则回退普通启动
        Err(e) if is_uac_declined(&e) => spawn_and_wait(exe, &args).await,
        Err(e) => Err(e).context("启动应用失败"),
    }
}

async fn spawn_and_wait(exe: &Path, args: &[OsString]) -> Result<u32> {
    let child = process::spawn(exe, args, false).context("启动应用失败")?;
    child.wait().await.context("等待应用退出失败")
}

fn is_uac_declined(err: &io::Error) -> bool {
    // ShellExecuteExW 在用户拒绝 UAC 时报 ERROR_CANCELLED(1223)
    err.raw_os_error() == Some(1223)
}

/// 尽力删除临时目录：失败退避重试，仍失败则留给 reaper。
async fn cleanup_temp(dir: &Path) {
    for _ in 0..20 {
        if tokio::fs::remove_dir_all(dir).await.is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

fn free_space(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let existing = path.ancestors().find(|p| p.exists())?;
    let wide: Vec<u16> = existing
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free = 0u64;
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), None, None, Some(&mut free)) }.ok()?;
    Some(free)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(file_name: &str) -> ExtractItem {
        ExtractItem {
            file_name: file_name.into(),
            rel: relative_under_root(Path::new(""), file_name).unwrap(),
            output_size: 1,
            md5: None,
            xxh: Some("h".into()),
            block: Embedded {
                name: "h".into(),
                offset: 0,
                raw_offset: 0,
                size: 1,
            },
        }
    }

    #[test]
    fn plan_extraction_reports_missing_block() {
        let embedded = vec![Embedded {
            name: "abc".into(),
            offset: 0,
            raw_offset: 0,
            size: 10,
        }];
        let hashed = vec![FileMeta {
            file_name: "app.exe".into(),
            size: 5,
            md5: None,
            xxh: Some("missing".into()),
            installer: None,
        }];
        let err = plan_extraction(&embedded, &hashed).unwrap_err();
        assert!(format!("{err:#}").contains("缺少数据块"));
    }

    #[test]
    fn plan_extraction_rejects_unsafe_path_before_writing() {
        let embedded = vec![Embedded {
            name: "h".into(),
            offset: 0,
            raw_offset: 0,
            size: 1,
        }];
        let hashed = vec![FileMeta {
            file_name: "../evil.exe".into(),
            size: 1,
            md5: None,
            xxh: Some("h".into()),
            installer: None,
        }];
        let err = plan_extraction(&embedded, &hashed).unwrap_err();
        assert!(format!("{err:#}").contains("路径不安全"));
    }

    #[test]
    fn exe_must_be_a_safe_path_in_the_plan() {
        let plan = vec![item("App.exe"), item("lib/x.dll")];
        assert_eq!(
            exe_in_plan(&plan, "app.EXE").unwrap(),
            PathBuf::from("app.EXE")
        );
        assert!(exe_in_plan(&plan, "lib\\x.dll").is_ok());
        assert!(
            format!("{:#}", exe_in_plan(&plan, "missing.exe").unwrap_err())
                .contains("没有应用主程序")
        );
        assert!(
            format!("{:#}", exe_in_plan(&plan, "../App.exe").unwrap_err()).contains("路径不安全")
        );
    }

    #[tokio::test]
    async fn decode_verify_write_roundtrip() {
        let data: Vec<u8> = (0..200000u32).map(|i| (i % 251) as u8).collect();
        // 压缩成 zstd 帧落盘，再经 mmap + 解码路径还原
        let dir = std::env::temp_dir().join(format!("kachina-sfx-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let packed = dir.join("block");
        {
            let mut encoder = async_compression::tokio::write::ZstdEncoder::new(Vec::new());
            encoder.write_all(&data).await.unwrap();
            encoder.shutdown().await.unwrap();
            std::fs::write(&packed, encoder.into_inner()).unwrap();
        }
        let packed_len = std::fs::metadata(&packed).unwrap().len() as usize;
        let mmap = AsyncMmapFile::open(&packed).await.unwrap();

        let mut xxh = twox_hash::XxHash3_128::new();
        xxh.write(&data);
        let xxh_hex = format!("{:x}", xxh.finish_128());

        let item = ExtractItem {
            file_name: "a/b.bin".to_string(),
            rel: PathBuf::from(r"a\b.bin"),
            output_size: data.len() as u64,
            md5: None,
            xxh: Some(xxh_hex),
            block: Embedded {
                name: "x".into(),
                offset: 0,
                raw_offset: 0,
                size: packed_len,
            },
        };
        let out = dir.join("out/b.bin");
        tokio::fs::create_dir_all(out.parent().unwrap())
            .await
            .unwrap();
        decode_verify_write(&mmap, &item, &out, None).await.unwrap();
        assert_eq!(tokio::fs::read(&out).await.unwrap(), data);

        // 解出的字节数与 meta 声明不符必须失败
        let longer = ExtractItem {
            output_size: data.len() as u64 + 1,
            ..item.clone()
        };
        let err = decode_verify_write(&mmap, &longer, &out, None)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("大小校验失败"));

        // 篡改期望 hash 必须失败
        let bad = ExtractItem {
            xxh: Some("0".into()),
            ..item
        };
        let err = decode_verify_write(&mmap, &bad, &out, None)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("校验失败"));

        drop(mmap);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
