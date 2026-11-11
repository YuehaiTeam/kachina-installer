//! 运行管线：还原目录树 → 拉起应用 → 等待退出 → 清理临时目录。

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use fmmap::tokio::{AsyncMmapFile, AsyncMmapFileExt};

use crate::metadata::{FileMeta, RepoMetadata};
use crate::pack_format::{
    preferred_file_hash, read_json, relative_under_root, scan_embedded, Embedded,
};
use crate::verified_write::{copy_into, Expected, Failure, HashingWriter};
use crate::{dialog, process, reaper, shell_link, uac};

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

/// 提权重启自身时插在用户参数之前的标记；带标记的实例不再提权，也不把标记传给应用。
pub const ELEVATED_ARG: &str = "--kachina-sfx-elevated";

/// 程序入口：返回进程退出码（正常路径透传应用退出码）。`relaunched` 表示本进程是带
/// [`ELEVATED_ARG`] 的提权重启实例。
pub async fn run(console: bool, relaunched: bool) -> i32 {
    // 先清理历史残留，再做正事；失败不阻塞本次运行。
    reaper::sweep();

    let (mmap, embedded, config) = match load().await {
        Ok(loaded) => loaded,
        Err(err) => {
            report_error(console, "kachina-sfx", &format!("{err:#}"));
            return 1;
        }
    };
    match run_package(console, relaunched, &mmap, &embedded, &config).await {
        Ok(code) => code,
        // 临时目录已清理，用户主动取消不弹错误
        Err(err) if err.downcast_ref::<Cancelled>().is_some() => 1,
        Err(err) => {
            report_error(console, &config.title(), &format!("{err:#}"));
            1
        }
    }
}

impl SfxConfig {
    /// 对话框标题：`windowTitle`，其次 `appName`。
    fn title(&self) -> String {
        self.window_title
            .clone()
            .or_else(|| self.app_name.clone())
            .unwrap_or_else(|| "kachina-sfx".to_string())
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

/// 校验、按需提权重启、解压、运行、清理。创建临时目录之后的任何失败都先删掉临时目录再返回。
async fn run_package(
    console: bool,
    relaunched: bool,
    mmap: &AsyncMmapFile,
    embedded: &[Embedded],
    config: &SfxConfig,
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

    let args: Vec<OsString> = std::env::args_os().skip(1 + relaunched as usize).collect();
    let self_elevated = uac::check_elevated().unwrap_or(false);
    if wants_elevation(relaunched, self_elevated, &config.uac_strategy) {
        match relaunch_elevated(&args).await {
            Ok(code) => return Ok(code as i32),
            Err(e) if is_uac_declined(&e) && config.uac_strategy == "force" => {
                bail!("用户取消了管理员授权，应用未运行")
            }
            // prefer-admin：用户取消则在本进程内以普通权限继续
            Err(e) if is_uac_declined(&e) => {}
            Err(e) => return Err(e).context("以管理员身份启动失败"),
        }
    }

    let temp_dir = create_temp_dir()?;
    let result = run_staged(console, mmap, &plan, &temp_dir, &exe, config, &args).await;
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

/// 解压计划：meta 的每个文件配上它的数据块，按解出大小降序排列，大小相同的保持 meta
/// 顺序。解压线程按这个顺序领取，最大的文件最先开始，收尾时不会只剩一两个线程在解大文件。
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
    plan.sort_by_key(|item| std::cmp::Reverse(item.output_size));
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
    args: &[OsString],
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
        dialog::Progress::show(&config.title(), "正在解压…", total).ok()
    };
    // 运行时是单线程的，这里没有别的任务；解压线程在 extract_all 里自己起
    extract_all(mmap.as_slice(), plan, temp_dir, progress.as_ref())?;
    if let Some(p) = progress {
        if p.cancelled() {
            return Err(Cancelled.into());
        }
        p.close().await;
    }

    let code = launch_and_wait(temp_dir, exe, config, args).await?;
    cleanup_temp(temp_dir).await;
    Ok(code as i32)
}

/// 解压线程数上限，与安装器的本地解压并发一致。
const MAX_WORKERS: usize = 16;

/// 先建好全部目录，再由每个 CPU 一个线程（至多 [`MAX_WORKERS`]）按计划顺序领取文件
/// 写出。任一文件失败或用户取消后，其余线程在下一次写入时停下；返回最先记下的错误。
fn extract_all(
    package: &[u8],
    plan: &[ExtractItem],
    temp_dir: &Path,
    progress: Option<&dialog::Progress>,
) -> Result<()> {
    let dirs: HashSet<PathBuf> = plan
        .iter()
        .filter_map(|item| temp_dir.join(&item.rel).parent().map(Path::to_path_buf))
        .collect();
    for dir in &dirs {
        std::fs::create_dir_all(dir).with_context(|| format!("创建目录失败 {}", dir.display()))?;
    }

    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let first_error: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    let workers = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .min(MAX_WORKERS)
        .min(plan.len());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                while !stop.load(Ordering::SeqCst) {
                    let Some(item) = plan.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };
                    if let Err(err) = extract_one(package, item, temp_dir, progress, &stop) {
                        // 先记错误再置停止位：被停止位打断的线程报的 Cancelled 不会抢先
                        first_error
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .get_or_insert(err);
                        stop.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            });
        }
    });
    match first_error.into_inner().unwrap_or_else(|e| e.into_inner()) {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// 解码一个数据块到临时目录，边写边算 hash，写完从同一个句柄读回核对；失败时删掉半成品
/// 文件。临时目录是本进程刚建的空目录，`rel` 只含普通组件，路径不会落到目录之外。
fn extract_one(
    package: &[u8],
    item: &ExtractItem,
    temp_dir: &Path,
    progress: Option<&dialog::Progress>,
    stop: &AtomicBool,
) -> Result<()> {
    let out_path = temp_dir.join(&item.rel);
    let result = decode_verify_write(package, item, &out_path, progress, stop);
    if result.is_err() {
        let _ = std::fs::remove_file(&out_path);
    }
    result
}

fn decode_verify_write(
    package: &[u8],
    item: &ExtractItem,
    out_path: &Path,
    progress: Option<&dialog::Progress>,
    stop: &AtomicBool,
) -> Result<()> {
    let block = package
        .get(item.block.offset..item.block.offset + item.block.size)
        .with_context(|| format!("{} 的数据块越过包尾", item.file_name))?;
    let mut decoder = zstd::stream::read::Decoder::with_buffer(std::io::Cursor::new(block))
        .context("初始化解压失败")?
        .single_frame();
    let file = crate::verified_write::create_file(out_path)
        .with_context(|| format!("创建文件失败 {}", out_path.display()))?;

    let cancelled =
        || stop.load(Ordering::SeqCst) || progress.is_some_and(dialog::Progress::cancelled);
    let expected = Expected {
        size: item.output_size,
        md5: item.md5.clone(),
        xxh: item.xxh.clone(),
    };
    // 进度框按字节累加，写出侧回报的是本文件累计值
    let mut reported = 0;
    let mut writer = HashingWriter::new(
        std::io::BufWriter::with_capacity(256 * 1024, file),
        expected,
        false,
        &cancelled,
    )
    .with_progress(|written| {
        if let Some(p) = progress {
            p.add_bytes(written - reported);
        }
        reported = written;
    });
    let failed = |failure: Failure| -> anyhow::Error {
        match failure {
            Failure::Cancelled => Cancelled.into(),
            Failure::Read(e) => {
                anyhow::Error::new(e).context(format!("{} 解压失败", item.file_name))
            }
            Failure::Write(e) => {
                anyhow::Error::new(e).context(format!("写入失败 {}", out_path.display()))
            }
            Failure::Size { expected, actual } => anyhow::anyhow!(
                "{} 大小校验失败：期望 {expected} 字节，实际 {actual} 字节",
                item.file_name
            ),
            Failure::Hash { expected, actual } => anyhow::anyhow!(
                "{} 校验失败：期望 {expected}，实际 {actual}",
                item.file_name
            ),
            Failure::ReadBack(e) => {
                anyhow::Error::new(e).context(format!("读回失败 {}", out_path.display()))
            }
            Failure::Altered { written, read } => anyhow::anyhow!(
                "{} 读回校验失败：写入 {written}，读回 {read}",
                item.file_name
            ),
        }
    };
    copy_into(&mut decoder, &mut writer).map_err(failed)?;
    writer.finish_verified().map_err(failed)?;
    Ok(())
}

/// 本进程是否要先提权重启自身再解压。未知取值与 prefer-admin 同义，与安装器
/// elevate_from_state 的默认分支一致；提权重启的实例即使没拿到管理员权限（如 UAC
/// 关闭的标准用户）也不再重启，避免循环。
fn wants_elevation(relaunched: bool, self_elevated: bool, strategy: &str) -> bool {
    !relaunched && !self_elevated && strategy != "prefer-user"
}

/// 以 `runas` 重启自身并等待退出，返回它的退出码。cwd 由 `ShellExecuteExW` 继承；
/// 调用者的环境变量不跨越提权，与直接经 UAC 启动一个程序一致。
async fn relaunch_elevated(args: &[OsString]) -> io::Result<u32> {
    let exe = std::env::current_exe()?;
    let mut relaunch_args = Vec::with_capacity(args.len() + 1);
    relaunch_args.push(OsString::from(ELEVATED_ARG));
    relaunch_args.extend_from_slice(args);
    let handle = uac::run_elevated(&exe, process::join_args(&relaunch_args))?;
    // SAFETY: SEE_MASK_NOCLOSEPROCESS 返回的进程句柄归调用方所有，交给 Child 关闭
    unsafe { process::Child::from_raw(handle.0) }.wait().await
}

/// 以本进程的权限拉起应用主体并等待退出，透传退出码。
///
/// 应用的环境额外带 electron-builder portable 目标的三个变量，指向 sfx 本体而不是临时目录；
/// cwd 继承 sfx 的调用者。应用以临时目录里的 `.lnk`（目标是 sfx 本体、不带参数）作为
/// 来源快捷方式启动，固定到任务栏的是 sfx 本体；快捷方式建不出来时照常启动。
async fn launch_and_wait(
    temp_dir: &Path,
    exe: &Path,
    config: &SfxConfig,
    args: &[OsString],
) -> Result<u32> {
    let sfx = std::env::current_exe().context("CURRENT_EXE_ERR")?;
    let app_filename = config
        .app_name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| {
            exe.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    let env = portable_env(&sfx, &app_filename);
    let link = create_source_link(temp_dir, &sfx, &app_filename);
    let child = process::spawn_from_link(
        temp_dir.join(exe),
        args,
        &env,
        link.as_deref().map(Path::as_os_str),
    )
    .context("启动应用失败")?;
    child.wait().await.context("等待应用退出失败")
}

fn portable_env(sfx: &Path, app_filename: &str) -> Vec<(&'static str, OsString)> {
    let dir = sfx.parent().unwrap_or(sfx);
    vec![
        ("PORTABLE_EXECUTABLE_DIR", dir.as_os_str().to_owned()),
        ("PORTABLE_EXECUTABLE_FILE", sfx.as_os_str().to_owned()),
        ("PORTABLE_EXECUTABLE_APP_FILENAME", app_filename.into()),
    ]
}

/// 解压完成后在临时目录根部写 `<名称>.lnk`，目标是 sfx 本体，起始位置是 sfx 所在目录；
/// 名称就是任务栏上显示的名称。同名文件已存在（包内文件，按文件系统自己的大小写规则判定）、
/// 名称清洗后为空或写入失败时返回 `None`，不覆盖任何已解出的文件。
fn create_source_link(temp_dir: &Path, sfx: &Path, name: &str) -> Option<PathBuf> {
    let path = temp_dir.join(format!("{}.lnk", link_file_stem(name)?));
    if path.symlink_metadata().is_ok() {
        return None;
    }
    shell_link::create(sfx, &path, sfx.parent()?).ok()?;
    Some(path)
}

/// 把名称变成可用的文件名主干：替换文件名非法字符，去掉 Windows 会剥掉的尾部点与空格。
fn link_file_stem(name: &str) -> Option<String> {
    let stem: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let stem = stem.trim().trim_end_matches(['.', ' ']);
    (!stem.is_empty()).then(|| stem.to_string())
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

    #[test]
    fn plan_puts_the_largest_files_first() {
        let embedded = vec![Embedded {
            name: "h".into(),
            offset: 0,
            raw_offset: 0,
            size: 1,
        }];
        let hashed: Vec<FileMeta> = [("a", 5), ("b", 900), ("c", 5), ("d", 40)]
            .into_iter()
            .map(|(name, size)| FileMeta {
                file_name: name.into(),
                size,
                md5: None,
                xxh: Some("h".into()),
                installer: None,
            })
            .collect();
        let plan = plan_extraction(&embedded, &hashed).unwrap();
        let names: Vec<&str> = plan.iter().map(|item| item.file_name.as_str()).collect();
        assert_eq!(names, ["b", "d", "a", "c"]);
    }

    #[test]
    fn only_an_unelevated_first_instance_asks_for_elevation() {
        assert!(wants_elevation(false, false, "prefer-admin"));
        assert!(wants_elevation(false, false, "force"));
        assert!(wants_elevation(false, false, "unknown"));
        assert!(!wants_elevation(false, false, "prefer-user"));
        assert!(!wants_elevation(false, true, "force"));
        assert!(!wants_elevation(true, false, "force"));
    }

    #[test]
    fn link_name_is_a_usable_file_name() {
        assert_eq!(link_file_stem("My App").as_deref(), Some("My App"));
        assert_eq!(link_file_stem("A/B: C?").as_deref(), Some("A_B_ C_"));
        assert_eq!(link_file_stem("App. . ").as_deref(), Some("App"));
        assert_eq!(link_file_stem(" .. "), None);
    }

    #[test]
    fn source_link_is_named_after_the_app_and_never_replaces_a_packaged_file() {
        let sfx_dir = temp_dir();
        let sfx = sfx_dir.join("Portable.exe");
        std::fs::write(&sfx, b"MZ").unwrap();

        let fresh = temp_dir();
        let link = create_source_link(&fresh, &sfx, "My App").unwrap();
        assert_eq!(link, fresh.join("My App.lnk"));
        assert!(link.is_file());

        // 包内同名文件只差非 ASCII 字母的大小写，文件系统视为同一个
        let packaged = temp_dir();
        std::fs::write(packaged.join("Ä.lnk"), b"packaged").unwrap();
        assert!(create_source_link(&packaged, &sfx, "ä").is_none());
        assert_eq!(std::fs::read(packaged.join("Ä.lnk")).unwrap(), b"packaged");

        for dir in [sfx_dir, fresh, packaged] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// 把每段内容压成一个 zstd 帧依次拼成"包"，返回包字节与对应的解压计划。
    fn packed(files: &[(&str, Vec<u8>)]) -> (Vec<u8>, Vec<ExtractItem>) {
        let mut package = Vec::new();
        let mut plan = Vec::new();
        for (name, data) in files {
            let frame = zstd::encode_all(&data[..], 3).unwrap();
            let mut xxh = twox_hash::XxHash3_128::new();
            xxh.write(data);
            plan.push(ExtractItem {
                file_name: name.to_string(),
                rel: relative_under_root(Path::new(""), name).unwrap(),
                output_size: data.len() as u64,
                md5: None,
                xxh: Some(format!("{:x}", xxh.finish_128())),
                block: Embedded {
                    name: name.to_string(),
                    offset: package.len(),
                    raw_offset: 0,
                    size: frame.len(),
                },
            });
            package.extend_from_slice(&frame);
        }
        (package, plan)
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kachina-sfx-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn extract_all_writes_every_file_under_its_directory() {
        let files: Vec<(String, Vec<u8>)> = (0..24u32)
            .map(|i| {
                let data = (0..(i * 997 + 1)).map(|b| (b % 251) as u8).collect();
                (format!("d{}/sub/f{i}.bin", i % 4), data)
            })
            .collect();
        let refs: Vec<(&str, Vec<u8>)> =
            files.iter().map(|(n, d)| (n.as_str(), d.clone())).collect();
        let (package, plan) = packed(&refs);
        let dir = temp_dir();
        extract_all(&package, &plan, &dir, None).unwrap();
        for (name, data) in &files {
            assert_eq!(&std::fs::read(dir.join(name)).unwrap(), data, "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_reports_size_and_hash_mismatch_and_removes_the_file() {
        let data: Vec<u8> = (0..200000u32).map(|i| (i % 251) as u8).collect();
        let (package, plan) = packed(&[("a/b.bin", data)]);
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("a")).unwrap();
        let stop = AtomicBool::new(false);

        // 解出的字节数与 meta 声明不符必须失败
        let longer = ExtractItem {
            output_size: plan[0].output_size + 1,
            ..plan[0].clone()
        };
        let err = extract_one(&package, &longer, &dir, None, &stop).unwrap_err();
        assert!(format!("{err:#}").contains("大小校验失败"));
        assert!(!dir.join("a/b.bin").exists());

        // 篡改期望 hash 必须失败，并让 extract_all 报出同一错误
        let bad = vec![ExtractItem {
            xxh: Some("0".into()),
            ..plan[0].clone()
        }];
        let err = extract_all(&package, &bad, &dir, None).unwrap_err();
        assert!(format!("{err:#}").contains("校验失败"));
        assert!(!dir.join("a/b.bin").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
