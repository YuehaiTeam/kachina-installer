# 前端重写为 Preact 渲染器

Status: proposed

## Problem

WebView 前端（`src/`，Vue 3 + rsbuild，产物为单个内联脚本与样式的 `dist/index.html`，经 `src-tauri/build.rs` zstd 压缩嵌入 exe）的体积与结构都和它承担的工作不匹配。

体积构成（rsbuild 1.5.10 production 构建，`dist/index.html` 原始 161,281 字节、zstd level 22 后 68,026 字节；模块占比取自 webpack-bundle-analyzer 在关闭内联后的 gzip 值，与 zstd 比例接近）：

| 模块 | 压缩后 | 占比 |
|---|---|---|
| Vue 运行时 | 31.2 KB | 45% |
| `src/left.webp` 默认左图（base64 data URI 内联，zstd 差值实测） | 22.7 KB | 33% |
| DOMPurify（`src/utils/svgSanitizer.ts` 用于源图标 SVG） | 4.9 KB | 7% |
| 应用代码全部（`App.vue` 2.4 KB、`plugins/*` 1.0 KB、其余约 4.5 KB） | 约 8 KB | 12% |
| CSS（`src/index.css` 经 purgecss 后 14 KB 原始） | 3.2 KB | 5% |

界面本身只有六个屏幕（就绪、进行中、安装完成、已是最新、卸载中、卸载完成）、两个面板（切换源、Mirror酱 CDK）和十来个交互，运行时框架占了近一半；默认图片占三分之一，而打包方通常会以 `\0IMAGE` 槽位提供自己的图片，默认图成为死重；DOMPurify 消毒的是打包方写进 exe 的 `embedded_config` 里的源图标，能改这段 SVG 的人同样能替换整个 HTML，消毒没有安全收益。

结构上，`src/bootstrap.ts`（260 行）与 `App.vue` 的十九个 `ref` 承担着会话状态推导与业务判断，`src/mirrorc-errors.ts` 维护 Mirror酱 错误映射，`src/session.ts` 粘合五种会话事件——这些在 [UI 契约](./2026-09-02-ui-contract.md) 落地后全部由 Rust 承担，前端剩下的职责只是"收到 `UiState` 就渲染、用户操作就发 `Intent`"。

`package.json` 的 `dependencies` 里 `async`、`compare-versions`、`uuid` 在 `src/` 中没有任何 import，bundle 里也不存在；`@sentry/cli` 是构建期工具却位于 `dependencies`。

## Proposal

依赖 [UI 契约](./2026-09-02-ui-contract.md) 的 `ui-state` 事件、`intent` 命令、`error_dialog` / `task_dialog` 命令、`i18n.tsv` 资产与插件宿主协议；本 note 只定义渲染器怎么实现，契约形状不在此复述。与契约 note 是同一连续任务：不写 Vue 适配层，bridge 切到 `ui-state` / `intent` 时第一方前端同时换成 Preact。

### 框架与构建

- Preact + TSX，`@preact/signals` 持有唯一的 `state = signal<UiState>()`。不用 SFC，不引入路由、状态库。
- rsbuild：`@rsbuild/plugin-vue` 换 `@rsbuild/plugin-preact`；`inlineScripts` / `inlineStyles` / `all-in-one` 分块 / purgecss / `dataUriLimit` 等既有配置保持。
- 删除依赖：`vue`、`@rsbuild/plugin-vue`、`dompurify`、`async`、`compare-versions`、`uuid`；`@sentry/cli` 移到 `devDependencies`。
- 删除文件：`src/left.webp`、`src/utils/svgSanitizer.ts`、`src/components/SafeIcon.vue`、`src/mirrorc-errors.ts`、`src/bootstrap.ts`、`src/session.ts`、全部 `.vue` 文件。`src/index.css` 的 Fluent 变量与组件样式保留，按新组件的类名调整。

### 目录

```
src/
  index.tsx          入口：?pluginHost=1 走插件宿主，否则挂载 <App/>
  host.ts            chrome.webview 收发：invoke / listen（现 src/tauri.ts 的内容，去掉 getCurrentWindow 包装）
  state.ts           signal<UiState>、订阅 ui-state、intent(…) 发送
  i18n.ts            拉取 /i18n.tsv、按 lang 选列、t(key, params)
  App.tsx            按 state.phase / mode 选择屏幕；面板开关等视图状态用 useState
  screens/           Ready.tsx  Running.tsx  Done.tsx  Failed.tsx
  panels/            SourcePanel.tsx  CdkPanel.tsx
  ui/                Checkbox.tsx  Input.tsx  Dialog.tsx  Spinner.tsx  icons.tsx
  plugin-host.ts     监听 session-plugin，转 plugins/bridge，answer_session_plugin
  plugins/           不变（index.ts、registry.ts、bridge.ts、types.ts、github/、stub/）
```

### 渲染规则

- 屏幕由 `state.phase` 决定：`Ready` → `Ready.tsx`；`Running` → `Running.tsx`；`Done` → `Done.tsx`（按 `SessionResult` 的 `is_uninstall` / `already_latest` / `is_update` 选文案键 `done.uninstall` / `done.latest` / `done.update` / `done.install`）；`Failed` → `Failed.tsx`。首次 `ui-state` 到达前显示 spinner。
- `Running.tsx` 用 `t("progress." + stage, { subject, done: formatSize(done), total: formatSize(total) })` 组当前状态行，`formatSize` 在前端实现；步骤列表按当前源是否为 `mirrorc://` 选 `step.default.*` / `step.mirrorc.*`。哪个 `stage` 用单行省略样式由渲染器按键决定。
- `Failed.tsx` 默认行为是调用 `invoke("error_dialog", coded)`，对话框关闭后发 `Intent::Dismiss`；`MIRRORC_CDK_*` 码在 `Dismiss` 之后打开 `CdkPanel`。`ui-notice` 事件同样调 `error_dialog`，不改 `phase`。
- `state.pending` 非空时渲染确认模态：文案键 `prompt.<kind>.title` / `prompt.<kind>.message`，`items` 以列表展示，按钮发 `Intent::Answer`。
- `Ready.tsx` 的每个控件对应一个 `Intent`：路径链接 → `invoke("pick_path")` 后发 `SetPath`；源列表 → `SetSource`；两个复选框 → `SetCreateLnk` / `SetDeleteUserData`；主按钮 → `Start`；`needs_elevate` 决定盾牌图标。`CdkPanel` 的输入框失焦或确定时发 `SetCdk`，按 `state.cdk` 显示校验中 / 无效（无效时 `state.cdk` 里的 `Coded` 走 `error_dialog`）。
- 源图标：`sources[i].icon` 为 SVG 文本时以 `dangerouslySetInnerHTML` 直接内联，不消毒；来源是打包方的嵌入配置，与替换整个 HTML 处于同一信任边界。
- 主题：`state.theme` 为 `Image` 时 `<img src="/theme.webp">`，为 `Css` 时插入 `<link rel="stylesheet" href="/theme.css">`，为 `None` 时不渲染图片区域、右侧内容占满宽度。前端 bundle 不携带任何默认图片。
- 窗口控制（无边框时的最小化 / 关闭）继续用 `window_minimize` / `window_close` 命令。

### 文案

`i18n.ts` 在启动时请求 `/i18n.tsv`（与 `index.html` 同一资产路径，`host/assets.rs::lookup` 负责解码），按首行表头找到 `state.project.lang` 对应列（无匹配取第一列），`t(key, params)` 做 `{name}` 直接替换，缺键返回键名。表在 `ui-state` 首次到达前并行加载完成。

### 插件宿主

`index.tsx` 判断 `location.search` 含 `pluginHost=1` 时不挂载任何界面，只执行 `plugin-host.ts`：监听 `session-plugin`、交给 `plugins/bridge.ts` 的 `handleSessionPlugin`、以 `answer_session_plugin` 应答、最后调用 `plugin_host_ready`。插件宿主与主界面是同一份 HTML，因此自定义 HTML 替换 `index.html` 后同时接管插件宿主：要引入新插件只需在自定义 HTML 中注册，不必改 Rust。

自定义 HTML 的最小实现（与第一方前端共用同一契约，不依赖 Preact）：

```html
<script>
const wv = chrome.webview;
wv.addEventListener('message', ({ data }) => {
  if (data.kind === 'event' && data.event === 'ui-state') render(data.payload);
  if (data.kind === 'event' && data.event === 'session-plugin') handlePlugin(data.payload);
});
const intent = (kind, extra) => wv.postMessage({ id: Date.now(), kind: 'invoke', cmd: 'intent', args: { kind, ...extra } });
function render(state) { /* 按 state.phase 更新 DOM；失败时可 postMessage cmd:'error_dialog' 或自行呈现 */ }
function handlePlugin(req) { /* 无插件时应答 { id: req.id, ok: false, error: 'not found' } */ }
if (location.search.includes('pluginHost=1')) wv.postMessage({ id: 1, kind: 'invoke', cmd: 'plugin_host_ready', args: {} });
</script>
```

### 测试

Vitest + jsdom + `@testing-library/preact`。夹具是若干 `UiState` JSON（就绪 / 更新态就绪 / 卸载态就绪 / 进行中 / 各类完成 / 失败 / 带 `pending`），断言关键元素出现与文案键解析，模拟点击后断言 `invoke("intent", …)` 的参数；`host.ts` 用一个内存版 `chrome.webview` 替身。一个测试读取 `locales/zh-CN.tsv`，断言 `screens/` 与 `panels/` 中出现的全部 `t("…")` 键都有文案。

## Alternatives considered

- Svelte 5：编译产物在同一量级（估 2–4 KB 之差），自带 scoped 样式与现有 `.vue` 写法最接近；但 rsbuild 侧是社区插件，多一层编译器依赖，测试工具链一并更换。
- 零框架手写 `render(state)`：运行时 0 KB，对自定义 HTML 是最好的示范；但没有 diff，进度高频推送要手动只更新进度节点，转义要自行保证，第一方维护成本最高。已在上节以最小实现的形式作为自定义 HTML 的示例保留。
- 保留 Vue 只做裁剪：Vue 运行时是 45% 的固定成本，`App.vue` 自身只有 2.4 KB，裁剪空间不在应用代码。
- 先做 Vue 的 `UiState` 适配层、模板暂不改：否。适配层本身就是一份渲染器，还要再扔掉；与契约 note 一起换到 Preact。
- 默认图片重新编码而不移除：180px 宽的位置在 2x 密度下 360px 即够，webp 可压到 10 KB 以内；但打包方几乎总会提供自己的图，默认图在成品中是死重，移除后由 `theme == None` 的布局承接。
- 源图标改用 `<img src="data:image/svg+xml,…">` 以免消毒：`<img>` 中的 SVG 不执行脚本，但 `.card svg { fill }` 这类 CSS 着色失效；信任边界已包含替换整个 HTML，直接内联即可。

## Acceptance criteria

- `dist/index.html` zstd level 22 后 ≤ 30,000 字节（rsbuild production 构建，与 Problem 中 68,026 同一测量方法）；预估约 20,000。
- `pnpm exec rsbuild build` 产物中不含 `vue`、`dompurify` 的模块，`left.webp` 不存在于仓库与产物。
- 每个 `UiState.phase` 变体与每个 `Prompt.kind` 至少一个组件测试；每个 `Intent` 变体至少一个"点击后发出该意图"的断言。
- 文案完整性测试通过：`screens/`、`panels/` 使用的键全部存在于 `locales/zh-CN.tsv`。
- 以 `?pluginHost=1` 加载时不渲染界面元素、不请求 `/i18n.tsv`、调用一次 `plugin_host_ready`；现有 e2e `plugin-stub` 保持通过。
- 现有 e2e 十项（`test:all`）保持全绿。
- `package.json` 的 `dependencies` 只剩 `preact` 与 `@preact/signals`。

## Risks

- 契约与渲染器同一批改动，形状仍可能两边手写不一致：渲染器以 `UiState` 的 TS 类型为唯一输入，该类型从 Rust 侧 `serde` 派生的 JSON 形状手写一份放在 `src/state.ts`，字段变更两边同时改；夹具 JSON 同步更新。
- `dangerouslySetInnerHTML` 内联源图标：信任边界已述；若日后配置可从远端加载，此处需要重新评估。
- 第一方 Fluent 样式依赖 purgecss 的 safelist 正则（`/^(?!h[1-6]).*$/`）保留绝大部分类名，换框架后类名生成方式变化，需要复核 purge 结果没有误删。
- WebView2 的 `chrome.webview.postMessage` 在 Preact 渲染前就可能收到首个 `ui-state`：`state.ts` 在模块加载即订阅并缓存最新值，`App` 挂载后读取 signal 当前值，不依赖事件顺序。
- 自定义 HTML 若只实现 `ui-state` / `intent` 而不实现插件宿主协议，以插件源打包的安装器会在 `session-plugin` 上等待超时：文档在契约 note 中，最小实现示例在本 note 中，两处互链。
