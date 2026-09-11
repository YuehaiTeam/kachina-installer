# 仓库扁平布局

Status: implemented

## Problem

Web 与 Rust 的源码、清单和 Windows 资源分属不同嵌套目录时，构建入口必须带 `--manifest-path`，CI 与测试要同时跟踪两套 `rust-toolchain`、两套 `target/`，源码路径也无法从仓库根直接读出职责。

## Decision

仓库根是唯一的包清单位置：`package.json`、`tsconfig.json`、`Cargo.toml`、`rust-toolchain.toml`、`build.rs`、`Cargo.lock`。`web/` 与 `native/` 只放源码，不放第二份 `package.json` 或 `Cargo.toml`。

| 路径 | 内容 |
|---|---|
| `web/` | WebView 前端（rsbuild 入口 `web/index.tsx`） |
| `native/` | `kachina-installer` 与 `kachina-builder` 的 Rust 源码（`Cargo.toml` 的 `[[bin]]` 指向 `native/main.rs` 与 `native/builder/main.rs`） |
| `libs/` | `hpatch-sys`、`hdiff-sys` 及其 C 源 |
| `resources/` | Windows 嵌入资源：`app.manifest`、`app.rc`、`icons/` |
| `locales/` | 文案 TSV，由根目录 `build.rs` 合并 |
| `target/` | Cargo 产物（含 `x86_64-win7-windows-msvc/release`） |

`libs/*` 仍是独立 crate，各自保留 `Cargo.toml`，由根包以 path 依赖引用。仓库中不存在 `src-tauri/`。

根目录 `build.rs` 从 `dist/index.html`、`locales/` 和 `resources/` 嵌入前端、文案与 exe 清单/图标。`CARGO_MANIFEST_DIR` 即仓库根，单测与脚本按根相对路径读取 `.cache/`、`locales/`、`tests/`。

## Alternatives considered

- 根目录 Cargo workspace，`native/` / `web/` 各带一份清单：与“清单只在根目录”冲突，且 `web/` 不是 Rust crate。
- 把 `hpatch-sys` / `hdiff-sys` 并进根包、取消 `libs/*/Cargo.toml`：C 绑定仍是独立编译单元，cc 构建脚本与 C 树跟着 crate 走更清楚。
- 保留 `src-tauri/` 只改内部文件夹名：`--manifest-path`、双 toolchain、双 `target/` 都还在。

## Verification

| 判据 | 结果 |
|---|---|
| 根目录有且仅有一份应用 `package.json` / `Cargo.toml` / `rust-toolchain.toml` | 见本决策表；`web/`、`native/` 无第二份清单 |
| `src-tauri/` 不在版本库中 | `git ls-files src-tauri` 为空 |
| 前端与 Rust 单测在新路径下通过 | PASS：`pnpm exec tsc --noEmit` 零错误；`pnpm exec vitest run` 21 passed；`cargo test` 16 passed（kachina-builder）+ 120 passed / 1 ignored（kachina-installer） |

## Consequences

- rust-analyzer 与 `cargo` 以仓库根为 crate 根；本地旧的 `src-tauri/target` 不再被构建使用。
- CI `unit-test` 执行 `cargo test`，产物与测试夹具都从根目录 `target/` 读取。
- `libs/*/Cargo.toml` 仍在，只服务 C 绑定 crate，不是应用包清单。
