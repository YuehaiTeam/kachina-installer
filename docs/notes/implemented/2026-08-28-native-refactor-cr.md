# Native refactor CR leftovers

Status: implemented

## Problem

`951f8cb..HEAD` 把安装器从 Tauri+Vue 换成原生 Win32+WebView2，安装会话下沉到 Rust `session/`。对照 `main` 与当时未提交的 insight 改动审查：`session/plan.rs`、`session/merge.rs`、`builder/replace_bin.rs` 测试覆盖扎实（含旧 JS dump 回归），能力面没有丢；但 Native 路径在 Mirror酱 CDK 过期自救、插件 RPC 超时与隐藏窗口类、插件 IPC 命令面、hashed insight range、WebView2 启动看门狗、`.metadata.json` 声明序、主题跟随、本地扫描前缀、invoke 关闭顺序和提权订阅时序上，与 `main` 有行为差和未完成的窗口/IPC 边角。

## Decision

落地在 `refactor/native` 的 `536ab57`。versionRegex 已换 `regex-lite`（该提交之前的分支上，`capture_version` 覆盖自定义 pattern）。本批现状：

- **CDK 过期**：Native TaskDialog 在 7001/7002/7004/7005 后强制重填 CDK，预填旧值，不自动删 wincred；空确认才删。WebView `session-reopen-source` 不动。
- **插件 hang**：`PluginHub::wait` 每 RPC 60s 超时，超时摘 pending 并失败安装；`PromptHub` 人类确认不超时。
- **隐藏插件窗口**：`PLUGIN_CLASS` + `plugin_wndproc`；`WM_CLOSE` 只 DestroyWindow，不 `delete_self_on_exit`；去掉 attach 后的 `set_visible(false)`。
- **插件 IPC**：`plugin_runtime` 默认拒绝；白名单 `plugin_host_ready` / `answer_session_plugin` / `http_get_request` / `log`/`warn`/`error`，以及仅 http(s) 的 `launch`。
- **Native 源列表**：TaskDialog 源选择隐藏 `needs_js_plugin` 的源；GitHub 仍列出。当前选中若被藏，切到第一个可见源。
- **insight 错误格式**：未改。`fail_with_insight` 仍用 `err.to_string()`，`verify_hash_keep_insight` 仍用 `{e:#}`。
- **hashed insight range**：`insight_range_vec` 在 `size==0` 时报空数组，对齐 `main` 全文件 GET；HTTP 仍不带 Range。`FileLocation` 仍可存 `request_range: "0-"`。`parse_range_string("0-")` 仍为 `(0, u32::MAX)`。
- **WebView2 看门狗**：未收到 `APP_BOOT_SIGNAL` 的等待由 5s 放到 30s。
- **`.metadata.json` 字段序**：`RepoMetadata` 恢复 `repo_name, tag_name, hashed, patches, installer, deletes, packing_info`。pack 嵌入仍走 `json!()` + `sort_all_objects()`。
- **主题**：`WM_SETTINGCHANGE` / `ImmersiveColorSet` 不再吞掉；`apply_mica` + DefWindowProc；WebView `SetBackground` 保留 mica 透明。
- **scan_local**：`strip_install_prefix` 忽略大小写和斜杠，避免 IPC 全路径剥前缀失败导致全量重下。
- **invoke 关闭**：`window_close` / `launch_and_exit` 先 `Reply` 再 `Close`（dispatch 成功才关）。
- **`GuiUi::alert`**：rfd MessageBox 放到 `spawn_blocking`。
- **提权 IPC**：`ManagedElevate::run` 先 `broadcast_tx.subscribe()` 再 `mpsc_tx.send`。

## Alternatives considered

- CDK 过期自动删 wincred：否，对齐 `main`（源界面重开，用户改 CDK；空确认才删）。
- 插件命令继续黑名单：否，改为默认拒绝白名单；`launch` 只放行 http(s)。
- hashed insight 保留 `(0, u32::MAX)`：否，按 `main` 的 `size==0` → `range: []`。
- insight 错误格式一并改成 `{e:#}`：否，并入后续错误码/i18n。

## Verification

| 判据 | 结果 |
|---|---|
| versionRegex / `capture_version` | PASS：regex-lite 0.1.9，5 个单测（`536ab57` 之前） |
| `cargo check`（src-tauri） | PASS（仅既有 hpatch/hdiff memcmp、builder dead_code 警告） |
| `parse_range_keeps_open_end` | PASS：`size==0` 空数组；`parse_range_string("0-")` 仍 `(0, u32::MAX)` |
| `strip_install_prefix_ignores_case_and_slashes` | PASS |
| `pack_embed_matches_legacy`（kachina-builder） | PASS：2 tests |
| Native CDK / 插件窗口 / 主题 / invoke 顺序 | 代码审阅落地；未做手工 GUI 回归 |

## Consequences

- 收益：Native 路径的 CDK 过期自救、插件超时与窗口隔离、插件 IPC 白名单、hashed 全文件 insight range、看门狗、元数据声明序、主题跟随、本地扫描前缀和关闭/提权时序与 `main` 对齐。
- 代价：insight 错误字符串仍不统一（`fail_with_insight` 的 `err.to_string()` vs `{e:#}`），含 `ERR_NETWORK_OTHER` 之类匹配风险，留给错误码 / i18n 治理。
- 崩溃 minidump 仍是独立 proposed：[崩溃采集](../proposed/2026-08-28-crash-capture.md)。
- 原针对 Tauri 路径的 GitHub issue 已被 native 重构替换，按原 PR 不能落地。
