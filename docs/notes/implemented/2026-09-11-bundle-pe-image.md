# Bundle 安装器映像按 PE 签名定位

Status: implemented

## Problem

`kachina-builder-bundle.exe` 是 builder 字节后直接接 installer 字节。pack 要从这个文件里切出 installer 映像，交给 rcedit 写版本信息。原先扫描全部 `MZ\x90\x00` 并取最后一处：MSVC DOS stub 正好是这四个字节，但 `.rdata`、zstd 资产和 ico 里也会出现同样的序列。安装器变大后，最后一处往往落在映像体内，rcedit `Load` 得到 `ExecutableLoadFailed`，`test:prepare` 在打 v1 updater 时退出。

## Decision

`native/builder/local.rs` 的 `pe_image_starts` 只收同时满足下列条件的偏移：`MZ`、`e_lfanew` 在 `0x40..=0x1000`、该处为 `PE\0\0`。`get_reader_for_bundle` 取最后一个这样的偏移，即拼接在 builder 后面的 installer。pack 把映像写到带进程 id 与 UUID 的临时文件并 `sync_all` 后再调用 rcedit。

## Alternatives considered

- 继续扫 `MZ\x90\x00` 但改取第二个命中：builder 体内只要再出现一处魔数，第二个就不是 installer。
- 在 merge 脚本写入显式偏移：要改 bundle 格式，现有 CI 产物和本地拼接文件都对不上。
- 解析第一个 PE 的 SizeOfImage 推算第二个起点：overlay 和节对齐会偏，签名扫描更直接。

## Verification

| 判据 | 结果 |
|---|---|
| 体内假 `MZ\x90\x00` 不改变 installer 起点 | PASS：`bundle_uses_last_real_pe_not_last_mz90` |
| 无 `PE\0\0` 的 DOS 魔数不是映像 | PASS：`pe_at_requires_pe_signature` |
| builder 单测 | PASS：`cargo test --bin kachina-builder` 18 passed |
| 失败 CI 产物上的真 bundle 能 `test:prepare` | PASS：`.cache/ci-fail/artifact/kachina-builder-bundle.exe`（5,729,792 字节，PE 起点 `0,2769408`）放到 `target/x86_64-win7-windows-msvc/release/` 后 `npm run test:prepare` 通过 |
| 用同一次产物的 `kachina-builder.exe` 覆盖 bundle 复现 CI | PASS：2,769,408 字节、仅 PE `[0]`，pack 打印 `Failed to find packed exe` |
| 失败产物走新测试 job 拷贝逻辑 | PASS：bundle 仍是 5,729,792，cargo builder 仍是 2,769,408 |
| 本地 CI 矩阵 e2e | PASS：`test:prepare` 后 offline/online install+update、dfs2、updater-survival、already-latest、uninstall、userdata-ignore、occupied-process、builder-extract-replace、plugin-stub |

## Consequences

- 只拼了一个 PE 的文件（未 merge 的 builder）会在 pack 时明确报找不到第二个映像并以退出码 1 结束，而不是把体内魔数交给 rcedit。
- `e_lfanew` 超过 4KiB 的非典型 PE 不会被当成映像起点。
- CI 上传的 `kachina-builder.exe` 必须是 bundle（builder+installer），不能是 cargo 的单文件 builder。测试 job 只在产物里还没有更大的 `kachina-builder-bundle.exe` 时，才用 `kachina-builder.exe` 补这个别名。
