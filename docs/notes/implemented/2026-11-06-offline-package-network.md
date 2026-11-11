# 离线包何时联网

Status: implemented

## Problem

离线包带着包内 metadata 与全部文件，用户没有加 `-O` 时应当不依赖来源服务器：服务器出错或不可用时照常装上包内版本，用户察觉不到。

会话开始时安装器向来源请求在线 metadata，用来发现更新的版本，请求失败时用包内 metadata。服务器接受连接后不回应时，安装要等客户端 30 秒的读取超时才开始。packed 来源读远端包头时不看状态码、不限读取长度，服务器忽略 Range 时整个包被读进内存。

内嵌文件的一次尝试失败后（数据块损坏或写盘出错），第二、三次尝试从网络取完整文件，离线安装因此依赖服务器。DFS2 来源下内嵌文件不申报范围，全部文件都内嵌时也不建会话，这次重试取不到 URL：用户看到 `NO_DOWNLOAD_NODE`，本地的错误（块损坏的 `HASH_MISMATCH`、写盘失败的磁盘错误）被盖掉。

格式允许 metadata 列出包里没有内嵌的文件。这样的包在在线版本与包内版本相同时按包内 metadata 安装，来源索引被换成包内索引，没内嵌的文件在远端找不到（由代码得出，没有复现）。

## Decision

### 离线包

包内 metadata 的每个文件都有内嵌数据块（按 metadata 的 hash 算法比对块名）时，安装器是离线包（[`carries_every_file`](../../../native/session/run.rs)）。判定在选用 metadata 之前按整个包做，不看本次要装哪些文件。

只内嵌一部分文件的包是在线安装器，内嵌块按 hash 复用：必须取到在线 metadata，取不到时安装失败；不弹版本提示，`-S` / `-I` 也装在线版本。自动应答时用包内版本的规则（[重构审阅问题](./2026-10-31-refactor-review-findings.md)的 C2）只对离线包成立。

界面与静默安装启动时都核对 `\0INDEX` 与扫描到的数据块（[`index_matches_blocks`](../../../native/installer/config.rs)），索引里的块找不到或偏移不符时报 `PKG_BROKEN`：被截断的包不算部分内嵌。

### 检查更新

离线包在交互会话开始时请求在线 metadata。`-S` / `-I` 安装包内版本，不发这次请求（[`asks_source`](../../../native/session/run.rs)）；加 `-O` 时照常请求。没有加 `-O` 时这次请求整体限时 5 秒（`UPDATE_CHECK_LIMIT`）。连接被拒、不可达、超时、HTTP 错误状态、响应不是安装包、metadata 解析失败，都按没有在线版本处理：使用包内 metadata，只记一条 `online metadata failed` 日志，不提示用户。检查成功且在线版本更新时，询问是否改用在线版本（更新时包内没有索引则直接改用）。

packed 来源读包头的 [`get_http_with_range`](../../../native/dfs.rs) 在状态码不是成功、或非零偏移的响应不是 206 时报错，响应体最多读到所请求的长度。

5 秒按包头段的大小取。packed 来源要取回 `\0CONFIG`、`\0IMAGE`、`\0INDEX`、`\0META` 首尾相接的一段；本机三个生产包甲旧版、甲新版、乙的这一段约 280 KB、300 KB、955 KB，乙含 922.6 KB 的 `\0IMAGE`（条目大小取自 `kachina-builder extract --list`，x86_64-win7-windows-msvc release）。

### 内嵌文件的重试

会话装包内 metadata 时（[`restore_local_package`](../../../native/session/source.rs)），内嵌文件的三次尝试都读同一个内嵌块，最后报本地的错误。装在线 metadata 时（在线安装器、带 `-O`、或在提示中选了更新），第二、三次以一个整文件请求从网络取；DFS2 下这段范围何时申报见[下载规划器](./2026-09-21-download-planner.md)的 DFS2 申报一节；DFS2 来源没有会话（计划里只有内嵌文件）时仍读包内数据块。

## Alternatives considered

- 离线包只在加 `-O` 时检查更新：交互安装时发现新版本的提示随之消失，检查更新仍然需要。
- 只限制建立连接与收到响应头的时间：要在每种来源的请求上分别加，服务器回了响应头再停住时仍要等 30 秒。
- 时限取 10 秒或 30 秒：慢连接上更容易取完包头，服务器不回应时用户等得更久。
- 按本次要装的文件判定离线：选用哪份 metadata 要在扫描安装目录之前定下，要装哪些文件却取决于扫描与计划，二者循环依赖。
- 部分内嵌的包报 `PKG_BROKEN`，或在 `-S` / `-I` 下只在在线版本与包内版本相同时继续：前者拒绝格式合法的包；这种包缺文件，装不出包内版本，只能装在线版本。
- 装包内版本时内嵌文件仍退到网络重试：离线安装依赖服务器，服务器不可用时还盖住本地错误；写盘失败时换来源也写不进去。
- 装在线版本时总是申报内嵌文件的完整范围：全部文件都能从包里读的安装也要建 DFS2 会话，建会话失败时整个安装失败。

## Verification

e2e 为 x86_64-win7-windows-msvc release（`pnpm build`），测试 server 在本机 8080 端口，Windows 11 Insider 26H1 build 28020。

| 判据 | 结果 |
|---|---|
| `-S` / `-I` 不请求来源服务器 | PASS：e2e `test:offline-update-check` 以 `-S` 安装 v1 离线包，来源地址上的测试 server 接受连接、不回应，安装装上 v1，server 没有收到任何请求；单测 `only_an_unattended_offline_install_skips_its_source` |
| 交互会话检查更新失败时装包内版本，不提示，除包头外不请求服务器 | 未自动化：e2e 没有交互会话。包头读取的规则见下面的包头读取一行 |
| 交互会话里服务器不回应时最多多等 5 秒 | 未自动化：e2e 没有交互会话 |
| 包头读取核对状态码、Range 与长度 | PASS：`ranged_read_checks_status_and_length` |
| 离线包判定 | PASS：`offline_package_embeds_every_listed_file`；e2e 的 v1、v2 离线包按离线安装：`offline-install`、`offline-update`、`already-latest` 不启动测试 server，按在线安装器处理时会因取不到 metadata 失败 |
| 装包内版本时内嵌文件重试读包内数据块且不申报；装在线版本时整文件重试且范围已申报，DFS2 没有会话时读包内数据块；全部文件都内嵌时不申报 | PASS：`embedded_retry_leaves_the_package_only_online`、`merge.rs` 的 `skip_ranges_for_embedded_files` |
| 被截断的包在静默安装时报 `PKG_BROKEN` | 未测 |
| 部分内嵌的包按在线安装器安装 | 未测：没有构造这种包的 e2e |
| 内嵌块损坏或写盘失败时报本地错误码 | 未测 |

## Consequences

- 慢于约 190 KB/s 的连接 5 秒内取不完带图片的包头，检查按失败处理，用户看不到新版本提示。
- `-S` / `-I` 不发这次请求：无人值守的安装不依赖来源服务器，也不知道来源有没有更新的版本。
- 部分内嵌的包离不开服务器，`-S` / `-I` 下装的是在线版本。
- 部分内嵌的包使用 hashed 存储、metadata 接口不可用而文件仍可下载时，安装失败。
