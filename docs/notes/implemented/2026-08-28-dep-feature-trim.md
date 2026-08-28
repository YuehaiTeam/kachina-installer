# 依赖面裁剪：移除 tracing-subscriber 与 serde_json/zip 冗余 feature

Status: implemented

## Problem

installer 二进制中存在三块"付了钱没用上"的依赖代码：

- `tracing-subscriber` 的 registry + fmt 栈（含 sharded-slab、thread-local、nu-ansi-term、tracing-log）合计约 46KiB `.text`，而仓库内没有任何 span 使用（`#[instrument]`/`span!` 全库零命中），registry 的 span 存储与 fmt 的通用格式化管线都是为不存在的需求付费。
- `serde_json` 的 `preserve_order` feature 把 `Map<String,Value>` 换成 IndexMap，仅为 builder 侧产物确定性而开；但 builder 实际依赖 `sort_all_objects()` 排序canonicalize（`src/builder/pack.rs`），排序后插入序无关紧要。同一 package 的两个 `[[bin]]` 共享 feature，无法只为 builder 开。
- `zip` crate 开着 `deflate64` 与 `zstd` 解码器，而全仓库唯一的 zip 消费点是 Mirror酱 归档解压（`src/thirdparty/mirrorc.rs`）。

测量条件（全文同）：本机 x86_64-pc-windows-msvc、opt-level=s + LTO、非 build-std，`cargo bloat` / 文件字节数。

## Decision

**tracing-subscriber 整体移除**，以自写的最小 `tracing::Subscriber`（`src/utils/log.rs`，`LogSubscriber`）替代。单个全局 Subscriber 同时完成：`enabled()` 全局 INFO 过滤、控制台输出（ANSI 着色）、`%TEMP%\KachinaInstaller.log` 文件输出（无色）、Sentry 面包屑写入（原 `BreadcrumbLayer` 逻辑并入）。span 相关方法为空实现，只发号不记录。时间戳复用 `utils::sentry::rfc3339()` 加毫秒。原 `sentry.rs` 中的 `InfoFilter`/`BreadcrumbLayer`/`MessageVisitor` 删除。

**serde_json 去掉 `preserve_order`**。`Map<String,Value>` 回到默认 BTreeMap（迭代即有序）；builder 的确定性由既有的 `sort_all_objects()` 保证，该方法不依赖此 feature。类型化结构体的序列化字段序本就与 feature 无关。

**zip 只保留 `deflate` feature**。两条已知归档来路均只产 Deflate（method 8）：Mirror酱 服务端为 Go 标准库 `archive/zip`（源码 MirrorChyan/resource-backend `internal/pkg/archiver/archiver.go`@d79a7dd，仅 Deflate）；人工打包路径 `7z a -tzip -mx=5` 的 zip 容器默认方法也是 Deflate，Deflate64 仅在显式 `-mm=Deflate64` 时产生。Store（`-mx=0`）与 zip64 结构不受 feature 影响。

## Alternatives considered

- 只替换 fmt 层、保留 registry + 自写 Layer：被本方案取代。registry（sharded-slab/thread-local）的存在意义是 span 存储，仓库无 span，`cargo tree -i tracing-subscriber` 确认无其他依赖方，可整体移除。
- 为 builder 单独保留 `preserve_order`：不可行，同 package 的 `[[bin]]` 共享 `[dependencies]`，feature 无法按 bin 拆分。
- 保留 `deflate64`/`zstd` 以防上游换压缩方法：拒绝。两条来路已核实为 Deflate-only，11.5KiB 换一个从未走到的路径不值；真换了方法会在解压时以 unsupported-method 明确报错，不是静默错误。

## Verification

| 判据 | 结果 |
|---|---|
| `preserve_order` 移除后 builder 产物字节等同 | PASS：排序后输出与移除前一致（`sort_all_objects` 不依赖该 feature，`sort_keys` 对 BTreeMap 为 no-op） |
| zip 仅 `deflate` 下 Mirror酱 归档可解压 | PASS：服务端源码核实仅写 Deflate；`7z -tzip` 默认 Deflate |
| tracing-subscriber 及其传递依赖出图 | PASS：cargo bloat 榜单中 tracing_subscriber、sharded_slab、nu_ansi_term、thread_local、tracing_log 全部消失 |
| 单测通过 | PASS：58 passed, 1 ignored |
| 体积下降 | PASS：4,090,880 → 4,056,064（preserve_order，−34.0KiB）→ 4,044,288（zip，−11.5KiB）→ 3,937,280（tracing，−104.5KiB），合计 −150.0KiB |

## Consequences

- DEBUG 及以下事件在 `enabled()` 全局挡掉，事件不再构造。此前也无任何消费者（fmt 层被 InfoFilter 过滤、BreadcrumbLayer 内部丢弃），语义等价但省了运行时开销；`sentry.rs` 内部的 `tracing::debug!` 自此为纯 no-op。
- 日志行格式从 tracing-subscriber fmt 默认样式变为固定的 `<RFC3339毫秒>Z <LEVEL> <target>: <message>`，字段以 `key=value` 追加；信息量等价，样式不再可配。
- 未来若引入 span，`LogSubscriber` 会静默忽略（发号但不记录），需扩展它或重新评估 registry。
- Mirror酱 若未来改用非 Deflate 压缩方法，解压在运行时以 unsupported-method 报错，需回开对应 zip feature。
- 未类型化 JSON 经 `Map<String,Value>` round-trip 后键序变为字典序（BTreeMap 固有行为），builder 的 canonicalize 正依赖这一点。
