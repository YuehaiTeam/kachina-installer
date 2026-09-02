# 编译配置级体积裁剪：日志级别编译期截断、IDNA 数据表移除、opt-level z

Status: implemented

## Problem

依赖裁剪与 IPC 类型化之后，`cargo bloat` 函数榜单已经很平：前两名 `run_dfs_install` 66.6KiB、`run_opr` 61.9KiB，第三名跌到 29KiB，58% 的 `.text` 由 6147 个小函数构成。但按 PE 节而不是按 crate 看，还有三块与代码结构无关、只由编译配置决定的体积：

- 依赖中大量 `tracing::debug!`/`trace!` 与 `log::debug!` 调用点。运行时由 `utils/log.rs` 的 `LogSubscriber` 在 `enabled()` 一律丢弃，但调用点的 `Metadata` 静态结构（.rdata）与格式化参数构造代码（.text）仍被链入。
- `url` → `idna` → ICU4X 的 Unicode 规范化数据表全部位于 .rdata，crate 榜单的 .text 视角看不到它。
- `opt-level = "s"` 保留了循环向量化与较多内联；`"z"` 在本项目的性能约束下是免费的。

测量条件（全文同）：本机 x86_64-pc-windows-msvc、release profile（LTO + codegen-units=1）、非 build-std，PE 节 raw size。基线 3,716,096 字节（.text 2,900,864 / .rdata 634,816）。

## Decision

1. `tracing` 与 `log` 开 `release_max_level_info`。DEBUG 及以下调用点在 release 构建中不生成代码，与 `LogSubscriber` 的 INFO 过滤语义等价。`log` 作为直接依赖只为给传递依赖（schannel/mio/want/zip）设置该 feature，仓库代码不使用它。
2. `idna_adapter` 在 `Cargo.lock` 锁定为 1.0.0（`cargo update -p idna_adapter --precise 1.0.0`）。这是 idna 官方提供的无 Unicode 后端桩：`Url::parse` 拒绝非 ASCII 域名，`xn--` Punycode 标签作为 ASCII 直接通过但不再做 UTS 46 校验。安装源、Mirror酱 与下载节点域名均为 ASCII，配置中如需国际化域名以 Punycode 形式填写。
3. `[profile.release] opt-level = "z"`。C 依赖（zstd/hpatch/hdiff/msquic）经 cc-rs 编译，MSVC 下 `s` 与 `z` 同映射为 `/O1`，不受影响。
4. 哈希 crate（`twox-hash`、`chksum-md5`、`chksum-hash-md5`）以 `[profile.release.package.*]` 单独保持 `opt-level = 3`。

## Alternatives considered

- 只对 `tracing` 设 `release_max_level_info`、不加 `log`：`log` 侧调用点分散在 TLS/IO 依赖里，单独一行直接依赖即可覆盖，无理由留下。
- `idna_adapter` 1.1.0（unicode-rs 后端）：比 ICU4X 更大，方向相反。
- 保留 IDNA、改用 `url` 之外的轻量解析：`reqwest` 自身依赖 `url`，替换不可行；桩后端是唯一能把数据表拿掉的选项。
- `opt-level = "z"` 全局但不为哈希 crate 单独覆盖：独立 bench 显示 xxh3 吞吐下降约 15%，虽仍远高于磁盘带宽，覆盖是零体积成本的保险。
- 用 nightly `#[optimize(speed)]` 标注 `hash_file` 代替 per-package 覆盖：LTO 下哈希循环若被内联进 `minsize` 调用者，per-package 属性会随之失效，函数级标注更稳；本轮先取配置级方案，出现回归再换。

## Verification

| 判据 | 结果 |
|---|---|
| `release_max_level_info` 后体积下降 | PASS：3,716,096 → 3,647,488（−68,608；.text −35KiB、.rdata −27KiB） |
| `idna_adapter` 1.0.0 后体积下降 | PASS：3,647,488 → 3,496,960（−150,528；.rdata −133KiB、.text −17KiB） |
| `opt-level = "z"` 后体积下降 | PASS：3,496,960 → 3,103,744（−393,216；.text 2,848,256 → 2,436,608） |
| 哈希 crate 单独 O3 不回吐体积 | PASS：3,103,744 → 3,102,208 |
| `opt-level = "z"` 的哈希吞吐 | PASS：独立 bench（512MiB 内存缓冲、1MiB 分块、LTO + codegen-units=1、nightly msvc）MD5 在 s/z 下均约 778 MB/s；xxh3 22.2 GB/s → 19.0 GB/s |
| 两个 bin 均可构建 | PASS：`cargo build --release` 产出 kachina-installer 3,102,208 字节、kachina-builder 2,870,784 字节 |
| 单测 | PASS：`cargo test` 13 passed（kachina-builder）+ 59 passed, 1 ignored（kachina-installer） |

合计 3,716,096 → 3,102,208 字节（−613,888，−16.5%），未改动任何仓库代码。

## Consequences

- `Url::parse` 对非 ASCII 域名返回错误，沿既有错误路径（如 `META_FAILED`）呈现；配置中的国际化域名须以 Punycode 填写。
- DEBUG/TRACE 事件在 release 构建中彻底不存在，调试依赖内部行为只能用 debug 构建；此前 release 下它们也从未被输出。
- `opt-level = "z"` 下所有 Rust 代码带 `minsize` 属性，后续新增 CPU 密集路径需自行评估是否加 per-package 覆盖或 `#[optimize(speed)]`。
- CI 构建（build-std + optimize_for_size）下 std 同样受 `opt-level = "z"` 影响，产物降幅预期大于本机数字；`cargo bloat` 的函数榜单在新配置下会整体缩小，后续代码级裁剪应以新基线重测。
