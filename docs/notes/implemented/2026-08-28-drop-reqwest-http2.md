# 移除 reqwest 的 http2 feature

Status: implemented

## Problem

`h2` 及其在 hyper/hyper-util 中的代码路径占发布 stub 约 200KiB `.text`（`cargo bloat --release --crates`，本机 x86_64-pc-windows-msvc、opt-level=s + LTO、非 build-std），而安装器的下载负载（并发 range GET 拉大块数据）从 HTTP/2 得不到收益：单流吞吐受 h2 流控窗口限制（客户端未启用 `http2_adaptive_window`），高带宽时延积链路上反而低于 HTTP/1.1；多路复用节省的握手对一次传输数百 MB 的负载可忽略。代码中无任何 http2 专属 API 引用。

## Decision

reqwest 依赖不启用 `http2` feature，客户端 ALPN 只协商 http/1.1。`h2` 的唯一进图路径是本 crate 的 reqwest feature 列表（sentry 只启用 blocking/json/native-tls，reqwest-middleware 只启用 json，直接依赖的 hyper 只启用 client/http1），删除后无 feature unification 回流。h3://、h3wt:// 传输走 msquic，与 reqwest 无关。

## Alternatives considered

- 保留 h2：无实测收益，且带流控窗口的吞吐上限。
- 拆分客户端、仅下载客户端禁用 h2：sentry 与元数据客户端本就未独立启用 http2，拆分徒增 feature 分裂，无额外收益。

## Verification

| 判据 | 结果 |
|---|---|
| `cargo tree -i h2` 无匹配 | PASS：`package ID specification 'h2' did not match any packages` |
| `.text` 体积下降 | PASS：4.1MiB → 3.9MiB（约 -205KiB；`cargo bloat --release --crates`，本机 x86_64-pc-windows-msvc、opt-level=s + LTO、非 build-std）；`h2`、`hyper` 退出 crate 榜前 15 |
| 代码无 http2 API 引用 | PASS：`rg "http2|prior_knowledge|adaptive_window" src/` 无匹配 |
| release 完整构建通过 | PASS：`cargo bloat` 前置构建成功 |

## Consequences

依赖图减少 `h2`、`atomic-waker`、`fnv`、`slab` 四个 crate。全部 reqwest 流量走 HTTP/1.1：并发 range 请求由单连接多路复用改为连接池多连接，首轮并发多数次 TLS 握手；单流下载吞吐不再受 h2 流控窗口限制。依赖调度器下发的 CDN 节点对 HTTP/1.1 多连接无额外限速（运维侧确认无此类节点）。
