# 桌面宠物助手 — 精致 UI 重设计 设计增补

- 日期：2026-10-22
- 状态：草稿（待 builder 审查，再交用户确认）
- 扩展：`docs/superpowers/specs/2026-10-21-desktop-pet-design.md`（v0.1.0 + model/cwd/streaming 增补已实现）

## 意图

把现有可用但不精致的界面，整改成"精致感"——干净、克制、有品质感的细节：好字体字号、好配色、克制的软阴影、得体的留白；对话内容按 markdown 渲染（代码块/列表/标题/行内 code）；机器人重画成克制的几何线条小家伙。每一处都经得起细看，不是花哨卖萌、也不是冷冰冰的工具。

## 目标

- **统一设计系统**：一套设计 token（颜色浅+深、字体字号、圆角、阴影、间距），所有组件消费，取代现在各组件各写各的 ad-hoc `<style>`。
- **柔和分层**：软阴影（`0 6px 24px rgba(...)` 级）+ 不透明白/近黑分层表面，不是纯无影 Flat；圆角阶梯（面板 14px → 气泡 12px → chip 999px → 代码块 8px）。
- **配色**：暖白 `#FAFAF7`/近黑 `#0F0F11` 背景；白 `#FFFFFF`/`#1A1A1D` 表面；墨 `#1C1C1E`/`#E8E8EA` 文字；**靛蓝强调** `#4F46E5`(浅)/`#818CF8`(深)；靛蓝软底 `#EEF2FF`/`#1E1B3A`；边框 `#ECECE8`/`#2A2A2E`；muted 文字 `#6B6B70`/`#8A8A90`。
- **字体**：系统栈 `system-ui, "PingFang SC", "Microsoft YaHei", sans-serif`（便携 exe 不联网载 web 字体，离线即可精致）；代码用 `ui-monospace, "SF Mono", monospace`。基础 14px、行高 1.6、小字 12px（≥11px）；仅 400/500 字重。
- **markdown 对话**：agent 回复按 markdown 渲染——段落/列表/**加粗**/`行内 code`/```代码块```（深底 mono，可选语法高亮）。用 `marked` 解析 + `DOMPurify` 净化（agent 输出虽半可信，仍防 XSS）。
- **机器人重画**：几何线条（2px 单线、圆角矩形头、点眼、天线、小身），5 态（待命/思考/睡眠/有动静/需要你）用克制的变化区分（思考=头顶小齿轮、睡眠=垂头+zzz、有动静=橙眼+!、需要你=?）。
- **浅深双色（仅面板）**：`Config.theme`（system/light/dark，Task 16 已有设置项）真正接到**面板**根 `data-theme`，token 按之切换（system 跟 `prefers-color-scheme`）。**机器人不跟主题切换**——固定精致配色（在任何桌面背景上看得清），不做跨窗口主题同步。
- **密度**：舒适留白（8px 节奏 4/8/12/16/24），不挤不空。

## 非目标

- 不重构布局（保留 tab 壳 + 对话流 + 当前的窗口/拖动/穿透逻辑）。
- 不加新功能（纯视觉 + markdown；代办/提醒/设置只换皮）。
- 不联网载字体（便携离线；用系统栈 + 好 CJK fallback）。
- 不做完整语法高亮引擎（代码块先深底 mono 可读；`highlight.js` 可选，builder 判断）。

## 约束

- Tauri v2 + Svelte 5；现有组件结构（`App.svelte`/`ChatView.svelte`/`Pet.svelte`/`SettingsView`/`TodosView`/`RemindersView`/`tray.rs`）。
- 便携 exe：不依赖运行时联网字体/资源；新增前端依赖（`marked` + `dompurify`）打进 bundle。
- 中文 UI 文案。
- 现有接口不动：`Message/MessageKind`、`Conversation`、`list_models`、`chat_send` 等——纯渲染层 + CSS 改造。

## 设计系统 token

写到 `src/panel/theme.css`（新），组件 `@import` 或在 `app.css` 引入；用 CSS 变量 + `:root[data-theme="light|dark"]` 切换（system 跟 `prefers-color-scheme`）。

```
浅: --bg #FAFAF7 --surface #FFFFFF --ink #1C1C1E --mut #6B6B70 --bd #ECECE8 --acc #4F46E5 --acc-soft #EEF2FF --code-bg #1E1E2E --code-ink #E4E4E7 --shadow 0 6px 24px rgba(20,20,30,.07),0 1px 3px rgba(20,20,30,.04)
深: --bg #0F0F11 --surface #1A1A1D --ink #E8E8EA --mut #8A8A90 --bd #2A2A2E --acc #818CF8 --acc-soft #1E1B3A --code-bg #0B0B0D --code-ink #D4D4D8 --shadow 0 6px 24px rgba(0,0,0,.45),0 1px 3px rgba(0,0,0,.3)
--font: system-ui,"PingFang SC","Microsoft YaHei",sans-serif; --mono: ui-monospace,"SF Mono",monospace
--r-panel 14px --r-bubble 12px --r-chip 999px --r-code 8px
--space: 4/8/12/16/24
```

## 机器人（几何线条，固定配色）

重画 `Pet.svelte` 的 SVG：2px 单线、`stroke-linecap/join: round`、圆角矩形头 + 点眼 + 天线 + 小身。5 态用克制变化：
- 待命：睁眼、微弧嘴、身稳。
- 思考：头顶小齿轮（或 `...`）。
- 睡眠：垂头、半透明、`zzz`。
- 有动静：橙眼 + `!`。
- 需要你：`?` + 看你。
睡觉态仍**静态无 keyframe**（v0.1.0 的"空闲 CPU≈0"约束保留）。
**固定配色（不跟主题切换）**：机器人用一套在任何桌面背景上看得清的墨色 + 软阴影/微光晕兜底，不接 `data-theme`、不做跨窗口主题同步。
**重画约束（builder 查证）**：保留 `<svg>` 根（`data-tauri-drag-region` 在根上，拖动靠它）；保留 `class="robot {stateClass}"` + `STATE_CLASS` 5 态映射（状态切换靠它）；若留思考齿轮，保留外层 `<g transform="translate(…)">` + 内层 `<g class="cog">` 的分裂（CSS `transform` 会覆盖 SVG `transform` 属性，不分裂齿轮会跑到 (0,0)）；新造型要落在 **~96×96** 内（`window.rs` 把机器人可交互矩形算作 120×120 窗的中央 80%，超出部分点击穿透会失效）。

## markdown 对话渲染

- `ChatView.svelte`：agent 气泡内容用 `marked.parse(content)` → `DOMPurify.sanitize(html)` → 渲染（Svelte `{@html}` 注入已净化 HTML；Svelte 5 `{@html}` 在表达式变化时重渲，流式可用）。
- **markdown 样式放全局 `theme.css`**：组件 scoped `<style>` 不作用于 `{@html}` 注入的内容，所以 markdown 元素（`p/ul/li/code/pre` 等）的样式必须在全局 `theme.css`，不是组件 `<style>`。
- 代码块：`marked` 默认 `<pre><code>`；CSS 深底 mono + 可选 `highlight.js`（builder 判断；不做也行，深底 mono 已可读）。`marked`/`dompurify` 是普通 npm 依赖，Vite 打进 bundle（离线便携满足；`tauri.conf.json` 的 `csp:null` 无 CSP 要配）。
- 用户气泡仍纯文本（accent 底白字）。
- 工具调用/结果/思考块（Task 6 的折叠）保持，但换皮到新 token。

## 组件换皮范围

- `theme.css`（新）：token + 基础元素样式（滚动条、focus、`prefers-reduced-motion`）。
- `App.svelte`：tab 壳换皮（药丸 tab、窗口 dots、拖动条）。
- `ChatView.svelte`：气泡（markdown）、composer、chip、会话列表项、新对话对话框——全换 token；markdown 渲染接入。
- `Pet.svelte`：机器人 SVG 重画（5 态）+ 状态 class 用 token 色。
- `TodosView`/`RemindersView`/`SettingsView`：换 token（表单/列表/按钮）。
- `tray.rs` 图标：换精致睡眠机器人（icon 资源，临时用现有，正式 icon 后续）。
- 主题接线（仅面板）：`Config.theme` → 启动时 + 设置改时设**面板**根 `data-theme`（`src/panel/main.ts` 里设 `document.documentElement.dataset.theme`；system 模式加 `prefers-color-scheme` `matchMedia` 监听）。**机器人窗口不接**（固定配色）。

## builder 审查后的修订/澄清（已并入上文，列此备查）

- **机器人固定配色**（用户定）：不跟主题切换，任何桌面背景上看得清，不做跨窗口主题同步。
- **主题接线是新工作**：现状 `data-theme`/`prefers-color-scheme` 在 `src/` 全无（SettingsView 的主题选择器目前是视觉 no-op）；两个 webview（panel 的 `index.html`、pet 的 `pet.html`）都有 `<html>` 可挂 `data-theme`——但只面板挂。
- **`{@html}`+DOMPurify 可行**：`csp:null`、Svelte 5 `{@html}` 流式重渲 OK；markdown 样式必须全局（scoped 不作用于 `{@html}`）。
- **SVG 重画约束**：留 svg 根（drag-region）+ class 绑定（5 态）+ cog 外内层分裂 + 落 96×96 内（中央 80% 可交互矩形）。
- **tray icon 占位**：临时用现有 app icon，正式 icon 后续（文档已记，非缺口）。

## 留给实现者

- `highlight.js` 是否引入（代码块语法色）——builder 判断（不引入也达标）。
- 滚动条/focus/选中态的细节美化。
- tray 正式 icon 资源（v0.1.x 可用占位）。
- 各组件 `<style>` 迁到 `theme.css` token 的具体改写（机械活）。

## 非目标（重申）

- 不动布局/窗口/拖动/穿透/agent/chat 逻辑——纯视觉 + markdown + 主题。
- 不联网字体/资源。
