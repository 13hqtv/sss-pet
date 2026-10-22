# 精致 UI 重设计 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把桌面宠物的界面从"可用但不精致"整改成精致感——统一设计系统(浅深 token)、柔和分层软阴影、靛蓝强调、系统 CJK 14px、对话 markdown 渲染、机器人重画成几何线条(固定配色),密度舒适留白。

**Architecture:** 新建 `src/panel/theme.css`(token 浅+深 + 排版 + 圆角 + 阴影 + 间距 + 基础元素),组件 `<style>` 的 ad-hoc hex 迁到 `var(--…)`;面板根挂 `data-theme`(light/dark,system 跟 `prefers-color-scheme`),机器人**不挂**(固定配色);`ChatView` 用 `marked`+`DOMPurify` 经 `{@html}` 渲染 agent 回复(markdown 样式放全局 `theme.css`);`Pet.svelte` 重画几何线条机器人(5 态,保留 svg 根/cog 分裂/96×96 足迹)。

**Tech Stack:** Tauri v2、Svelte 5、CSS 变量、`marked`+`dompurify`(npm)。

**Spec:** `docs/superpowers/specs/2026-10-22-refined-ui-design.md`

## Global Constraints

- Windows 11;Tauri v2 + Svelte 5。便携 exe(离线,不联网载字体/资源)。
- 字体:系统栈 `system-ui,"PingFang SC","Microsoft YaHei",sans-serif` + mono `ui-monospace,"SF Mono",monospace`;基础 14px、行高 1.6、小字 12px(≥11px);仅 400/500 字重。
- `.superpowers/` git-ignored;scoped `git add src/ src-tauri/`;在 master 上做;中文 UI。
- 现有接口不动:`Message/MessageKind`、`Conversation`、`list_models`、`chat_send`、`run_turn`、窗口/拖动/穿透/状态机逻辑——纯视觉 + markdown + 主题。
- 机器人**固定配色**(不跟主题);面板主题(`data-theme` + `prefers-color-scheme`)。
- 机器人重画约束:保留 `<svg>` 根(`data-tauri-drag-region`)+ `class="robot {stateClass}"` + `STATE_CLASS` 5 态 + 思考齿轮外层 `<g transform>`/内层 `<g class="cog">` 分裂 + 落 ~96×96 内(中央 80% 可交互矩形)。
- markdown 样式放全局 `theme.css`(Svelte scoped `<style>` 不作用于 `{@html}` 内容)。
- 睡觉态静态无 keyframe(v0.1.0"空闲 CPU≈0"约束保留)。

## Review Focus

1. **主题真切换面板**(主题选择器原本是 no-op)→ Task 1 验证 data-theme 接线 + token 切换。
2. **markdown 真渲染**(不是原始 markdown 文本)→ Task 2/3 验证 `{@html}`+DOMPurify 管线 + 代码块。
3. **机器人重画没弄坏拖动/穿透/5 态**→ Task 5 验证 svg 根/cog 分裂/96×96。
4. **scoped→全局迁移没丢样式**(某组件自定义样式没了)→ Task 3/4 验证 build + 视觉。
5. **纯视觉改动没弄坏 v0.1.0 逻辑**(chat/tray/窗口/状态)→ Task 6 验证 cargo test 全绿 + dev 跑 + exe 运行。

## File Structure

- `src/panel/theme.css`(建:token + 基础元素 + markdown 全局样式)
- `src/panel/app.css`(改:import theme.css)
- `src/panel/main.ts`(改:主题接线 data-theme)
- `src/panel/views/ChatView.svelte`(改:换皮 + markdown 接入)
- `src/panel/views/{TodosView,RemindersView,SettingsView}.svelte`(改:换皮)
- `src/panel/App.svelte`(改:tab 壳换皮)
- `src/pet/Pet.svelte` + `src/pet/states.css`(改:机器人重画 + 固定配色)
- `src-tauri/src/tray.rs`(改:icon 占位)
- `package.json`(加 marked/dompurify)

---

## Task 1: 设计系统 theme.css + 主题接线

**Files:** Create `src/panel/theme.css`;Modify `src/panel/app.css`(import)、`src/panel/main.ts`(data-theme)、`src/panel/views/SettingsView.svelte`(save 时设 data-theme)。

**Interfaces:** Produces: `theme.css` 的 CSS 变量(`--bg/--surface/--ink/--mut/--bd/--acc/--acc-soft/--code-bg/--code-ink/--shadow` 浅+深、`--font/--mono`、`--r-*`、`--space-*`)+ 基础元素(滚动条/focus/`prefers-reduced-motion`);面板根 `data-theme` 由 `Config.theme` 设(system 跟 `prefers-color-scheme`)。

- [ ] **Step 1: 建 `theme.css`** — token(浅/深两套 via `:root[data-theme="light"]` / `[data-theme="dark"]` + `@media (prefers-color-scheme: dark)` for system)+ 排版/圆角/阴影/间距 + 基础元素。值照 spec 的 token 表。
- [ ] **Step 2: `app.css`** `@import "./theme.css";`。
- [ ] **Step 3: 主题接线** — `main.ts` 启动:读 `Config.theme`(`invoke('settings_get')` 或已有)→ `document.documentElement.dataset.theme = theme`;若 `system` → 用 `matchMedia('(prefers-color-scheme: dark)')` 定初值 + 监听变化。`SettingsView` 保存主题时同步设 `document.documentElement.dataset.theme`(同 webview,无需跨窗口事件)。
- [ ] **Step 4: 验证 Review Focus #1** — `pnpm build`+`pnpm check` clean;dev,在设置里切 light/dark/system → **面板**随之翻(机器人不变,下个任务才改)。`cargo check` 过。
- [ ] **Step 5: 提交** `git commit -m "feat: design-system theme.css (light/dark tokens) + panel data-theme wiring"`。

## Task 2: markdown 渲染(marked + DOMPurify)

**Files:** Modify `package.json`(deps)、`src/panel/views/ChatView.svelte`(renderMarkdown + `{@html}`)、`src/panel/theme.css`(markdown 全局样式)。

**Interfaces:** Produces: `renderMarkdown(content: string): string`(`marked.parse` → `DOMPurify.sanitize`);ChatView 助手气泡用它经 `{@html}` 渲染。

- [ ] **Step 1: 加依赖** `pnpm add marked dompurify`(+ `@types/dompurify` 若需)。
- [ ] **Step 2: `ChatView` 加 `renderMarkdown`** — `import marked from 'marked'; import DOMPurify from 'dompurify';` → `renderMarkdown(c){ return DOMPurify.sanitize(marked.parse(c ?? '',{breaks:true})); }`;助手气泡 `{@html renderMarkdown(m.content)}`。
- [ ] **Step 3: `theme.css` 加 markdown 全局样式** — `.bubble p/ul/li/strong/code/pre` 等(段落间距/列表/加粗/行内 code 软底/代码块深底 mono + 可选 `highlight.js`);代码块 `.codeblock` 深底。
- [ ] **Step 4: 验证 Review Focus #2** — `pnpm build`+`pnpm check` clean;dev,发一条含 markdown 的回复(或 dev stub)→ 渲染成段落/列表/代码块,不是原始 `**`/``` 文本;`<script>` 被净化(可代码级断言 DOMPurify 跑过)。
- [ ] **Step 5: 提交** `git commit -m "feat: markdown rendering in chat (marked + DOMPurify)"`。

## Task 3: ChatView 换皮 + markdown 接入助手气泡

**Files:** Modify `src/panel/views/ChatView.svelte`(及其 `<style>`)。

**Interfaces:** Consumes: Task 1 token + Task 2 renderMarkdown。Produces: 精致气泡/composer/chip/会话列表/新对话对话框。

- [ ] **Step 1: `<style>` 迁 token** — 把 ChatView 所有硬编码 hex 换成 `var(--…)`;气泡(用户 accent 底白字 / 助手 surface 底 + markdown)、composer、chip、会话列表项、新对话对话框、思考/工具折叠块全用 token + 软阴影 + 圆角阶梯。
- [ ] **Step 2: 助手气泡接 markdown** — 确认 Task 2 的 `{@html renderMarkdown(m.content)}` 落到助手气泡;用户气泡仍纯文本;思考/工具块(Task 6 of model/cwd)换皮到 token 但折叠逻辑不动。
- [ ] **Step 3: 验证 Review Focus #2/#4** — `pnpm build`+`pnpm check` clean;dev,对话区精致:气泡/chip/composer 得体,markdown 渲染,折叠块仍可展;浅深切换都耐看。
- [ ] **Step 4: 提交** `git commit -m "feat: ChatView refined restyle + markdown bubbles"`。

## Task 4: App.svelte tab 壳 + 其它视图换皮

**Files:** Modify `src/panel/App.svelte`、`src/panel/views/{TodosView,RemindersView,SettingsView}.svelte`。

**Interfaces:** Consumes: Task 1 token。

- [ ] **Step 1: `App.svelte`** — tab 壳:药丸 tab(active = acc-soft 底 + acc 字)、窗口 dots、dragbar → token + 软阴影。
- [ ] **Step 2: `TodosView`/`RemindersView`/`SettingsView`** — 表单/列表/按钮/开关 → token;`SettingsView` 的主题选择器 + 每 agent 默认 model 探测块也换皮。
- [ ] **Step 3: 验证 Review Focus #4** — `pnpm build`+`pnpm check` clean;dev,四个 tab 都精致一致,无某处漏掉旧样式。
- [ ] **Step 4: 提交** `git commit -m "feat: app shell + todos/reminders/settings refined restyle"`。

## Task 5: Pet.svelte 机器人重画(几何线条,固定配色)

**Files:** Modify `src/pet/Pet.svelte`、`src/pet/states.css`、`src/pet/app.css`。

**Interfaces:** Consumes: 固定配色(不接 token/不挂 data-theme)。Produces: 几何线条机器人 5 态。

- [ ] **Step 1: 重画 SVG** — 几何 2px 单线(`stroke-linecap/join:round`):圆角矩形头 + 点眼 + 天线 + 小身,**落 ~96×96 内**;5 态克制变化(待命睁眼微弧嘴 / 思考头顶小齿轮 / 睡眠垂头半透 zzz / 有动静橙眼 ! / 需要你 ?);**保留 `<svg>` 根**(`data-tauri-drag-region`)、`class="robot {stateClass}"`、`STATE_CLASS` 5 态映射、思考齿轮的**外层 `<g transform>` + 内层 `<g class="cog">` 分裂**(不分裂齿轮跑 (0,0))。
- [ ] **Step 2: 固定配色** — 机器人用固定墨色 + 软阴影/微光晕(不接 `data-theme`,不跟主题翻);`states.css` 的 standby/thinking keyframe 改用固定色;睡觉态仍**无 keyframe**(静态)。
- [ ] **Step 3: 验证 Review Focus #3** — `pnpm build`+`pnpm check` clean;dev,机器人 5 态都精致;拖动 + 点击穿透仍工作(点机器人能拖/能开面板,透明区穿透);睡眠静态无动画。
- [ ] **Step 4: 提交** `git commit -m "feat: refined geometric robot (5 states, fixed palette)"`。

## Task 6: tray icon 占位 + 重建 exe + 全验证

**Files:** Modify `src-tauri/src/tray.rs`(icon 占位,临时用现有);重建 exe。

- [ ] **Step 1: tray icon** — 临时用现有 app icon(spec 允许占位);若易做精致睡眠机器人 icon 可换,否则留占位记 follow-up。
- [ ] **Step 2: 重建 exe** — `pnpm tauri build` → `src-tauri/target/release/swd-pet.exe`。
- [ ] **Step 3: 全验证 Review Focus #1/#2/#3/#5** — `cargo check`+`pnpm build`+`pnpm check`(0 错)+`cargo test`(全套绿,不破坏 v0.1.0+model/cwd 逻辑)+ exe 跑:浅/深切换、markdown 对话渲染、机器人 5 态、拖动/穿透/托盘/对话/代办/提醒 都正常。
- [ ] **Step 4: 提交** `git commit -m "build: refined UI — tray icon placeholder + rebuild exe"`。

---

## Self-Review

- **Spec 覆盖**:设计系统 token(T1)、markdown(T2)、ChatView 换皮+markdown(T3)、App 壳+其它视图(T4)、机器人重画固定配色(T5)、tray+打包验证(T6) 均有任务。主题接线(T1,只面板)、机器人不挂主题(T5)、markdown 全局样式(T1/T2)、机器人重画约束(T5) 全落。
- **类型一致**:`theme.css` token 名(T1)被 T2/T3/T4 消费;`renderMarkdown`(T2)被 T3 消费;固定机器人配色(T5)独立。
- **Review Focus 五条**各钉任务(T1/T2+T3/T5/T3+T4/T6)。
- **比例**:以签名+验证为主,CSS 体留给实现者(机械迁 token)。
