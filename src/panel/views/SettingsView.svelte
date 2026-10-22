<!-- Task 16 + Task 5: the 设置 (settings) tab. Default-provider select,
     follow_activity_min (number, min 900, ≥15min), 勿扰 toggle +
     optional time window (start/end), theme (system/light/dark),
     工作目录 (working dir for agent cwd — a path field). Save →
     settings_set_with_focus → config::save AND applies Signal::FocusMode
     to the brain immediately. Chinese.

     Task 5 addition: per-agent default model. For each of
     claude / codex / opencode: a "探测模型" button → listModels(agent) →
     a dropdown to pick a default model; save to Config.providers[agent]
     .default_model. The new-conv dialog pre-fills the model from this
     default. Falls back to a manual text input when the probe is
     empty (design: 兜底). -->
<script lang="ts">
  import {
    getSettings,
    saveSettings,
    getAutostart,
    setAutostart,
    type Settings,
    type Provider,
    type ProviderCfg,
  } from "../lib/settings";
  import { listModels } from "../lib/api";
  // Task 1: apply the theme to <html data-theme> on save so the panel
  // flips immediately (same webview, no cross-window event needed).
  import { applyTheme } from "../lib/theme";

  const PROVIDERS: Provider[] = ["claude", "codex", "opencode"];
  const PROVIDER_LABEL: Record<Provider, string> = {
    claude: "Claude",
    codex: "Codex",
    opencode: "OpenCode",
  };

  let settings: Settings | null = $state(null);
  let errorMsg: string | null = $state(null);
  let savedMsg: string | null = $state(null);
  let savedTimer: ReturnType<typeof setTimeout> | null = null;

  // Local-edit copies (so the user can edit before clicking 保存).
  let followMin: number = $state(900);
  let theme: string = $state("system");
  let focusEnabled: boolean = $state(false);
  let focusStart: string = $state("");
  let focusEnd: string = $state("");
  let useFocusWindow: boolean = $state(false);
  let defaultProvider: Provider = $state("claude");
  let workingDir: string = $state("");
  let autostart: boolean = $state(false);
  // Responsible persons roster for the 代办 tab — comma-separated text.
  let responsiblePersons: string = $state("");
  // Task 5: per-agent default model local-edit copies. The user probes
  // + picks; the global 保存 button persists all three to
  // Config.providers.<agent>.default_model.
  let providerModels: Record<Provider, string | null> = $state({
    claude: null,
    codex: null,
    opencode: null,
  });

  // Per-agent probe state: list (the probed model IDs), a "probing"
  // flag, an error message, + a manual-entry toggle (the design's
  // 兜底 — when the probe returns empty, the user types a model name).
  interface ProbeState {
    list: string[];
    probing: boolean;
    error: string | null;
    manual: boolean;
  }
  let probeState: Record<Provider, ProbeState> = $state({
    claude: { list: [], probing: false, error: null, manual: false },
    codex: { list: [], probing: false, error: null, manual: false },
    opencode: { list: [], probing: false, error: null, manual: false },
  });

  async function refresh() {
    try {
      const s = await getSettings();
      settings = s;
      followMin = s.follow_activity_min_secs;
      theme = s.theme;
      focusEnabled = s.focus.enabled;
      if (s.focus.window) {
        focusStart = s.focus.window[0];
        focusEnd = s.focus.window[1];
        useFocusWindow = true;
      } else {
        focusStart = "22:00";
        focusEnd = "07:00";
        useFocusWindow = false;
      }
      defaultProvider = (s.default_provider as Provider) || "claude";
      workingDir = s.working_dir ?? "";
      responsiblePersons = (s.responsible_persons ?? []).join(", ");
      providerModels = {
        claude: s.providers.claude.default_model ?? null,
        codex: s.providers.codex.default_model ?? null,
        opencode: s.providers.opencode.default_model ?? null,
      };
      // Query the OS-level autostart state (registry is source of truth).
      try {
        autostart = await getAutostart();
      } catch {
        autostart = false;
      }
      errorMsg = null;
    } catch (e) {
      errorMsg = `加载设置失败: ${e}`;
    }
  }

  /** Probe models for one agent via `listModels(agent)`. On empty →
   * switch the agent's row to manual text entry. On non-empty →
   * populate the dropdown; if the current default isn't in the list,
   * keep it but switch to manual entry so the user can edit/keep
   * rather than silently dropping it. */
  async function probeModels(agent: Provider) {
    const st = probeState[agent];
    st.probing = true;
    st.error = null;
    st.manual = false;
    st.list = [];
    try {
      const list = await listModels(agent);
      st.list = list;
      const current = providerModels[agent];
      if (list.length === 0) {
        // Probe failed / empty → manual text entry. Keep the current
        // default (if any) so the user sees what was configured + can
        // edit it.
        st.manual = true;
      } else if (current && !list.includes(current)) {
        // The current default isn't in the probed list → manual entry
        // so the user can edit/keep it rather than silently dropping.
        st.manual = true;
      } else if (!current) {
        // No current default + non-empty list → pick the first as a
        // convenience default (the user can change it before saving).
        providerModels[agent] = list[0];
      }
    } catch (e) {
      st.error = `探测失败: ${e}`;
      st.manual = true;
    } finally {
      st.probing = false;
    }
  }

  /** Toggle manual entry for one agent (lets the user type a model name
   * even when the probe returned a non-empty list — e.g. for a model
   * not yet advertised by the CLI). */
  function toggleManual(agent: Provider) {
    probeState[agent].manual = !probeState[agent].manual;
  }

  /** Toggle autostart immediately (no save button needed — the OS registry
   * is the source of truth, written right now). */
  async function toggleAutostart() {
    const next = !autostart;
    autostart = next;
    try {
      await setAutostart(next);
    } catch (e) {
      // Revert on failure + surface the error
      autostart = !next;
      errorMsg = `设置开机自启失败: ${e}`;
    }
  }

  async function save() {
    // Enforce the 15min floor on follow_activity_min (the design spec
    // + the brain's FOLLOW_ACTIVITY_MIN constant both treat 900s as
    // the floor).
    if (followMin < 900) {
      errorMsg = "代办间隔最少 900 秒 (15 分钟)";
      return;
    }
    // Task 5: build the ProvidersCfg with the per-agent default models
    // (trimmed; empty string → null = "use the agent's own default").
    const buildCfg = (p: Provider): ProviderCfg => {
      const m = (providerModels[p] ?? "").trim();
      return { default_model: m.length > 0 ? m : null };
    };
    const newSettings: Settings = {
      follow_activity_min_secs: Math.floor(followMin),
      theme,
      focus: {
        enabled: focusEnabled,
        window: useFocusWindow && focusStart && focusEnd
          ? [focusStart, focusEnd]
          : null,
      },
      default_provider: defaultProvider,
      working_dir: workingDir.trim() || null,
      providers: {
        claude: buildCfg("claude"),
        codex: buildCfg("codex"),
        opencode: buildCfg("opencode"),
      },
      responsible_persons: responsiblePersons
        .split(",")
        .map((s) => s.trim())
        .filter((s) => s.length > 0),
    };
    try {
      await saveSettings(newSettings);
      settings = newSettings;
      // Task 1: apply the theme to <html data-theme> now so the panel
      // flips immediately (no panel reload). "system" re-resolves via
      // prefers-color-scheme and re-registers the OS-change listener;
      // explicit light/dark are sticky. The pet window does NOT theme.
      applyTheme(theme);
      errorMsg = null;
      savedMsg = "已保存";
      if (savedTimer) clearTimeout(savedTimer);
      savedTimer = setTimeout(() => {
        savedMsg = null;
        savedTimer = null;
      }, 2000);
    } catch (e) {
      errorMsg = `保存失败: ${e}`;
    }
  }

  $effect(() => {
    void refresh();
    return () => {
      if (savedTimer) clearTimeout(savedTimer);
    };
  });
</script>

<section class="settings">
  {#if settings === null}
    <div class="loading">加载中…</div>
  {:else}
    <div class="form">
      <div class="field">
        <label for="default-provider">默认 Provider</label>
        <select
          id="default-provider"
          bind:value={defaultProvider}
        >
          {#each PROVIDERS as p (p)}
            <option value={p}>{PROVIDER_LABEL[p]}</option>
          {/each}
        </select>
      </div>

      <div class="field">
        <label for="follow-min">代办间隔 (秒, ≥900)</label>
        <input
          id="follow-min"
          type="number"
          min="900"
          step="60"
          bind:value={followMin}
        />
      </div>

      <div class="field">
        <label for="theme">主题</label>
        <select id="theme" bind:value={theme}>
          <option value="system">跟随系统</option>
          <option value="light">浅色</option>
          <option value="dark">深色</option>
        </select>
      </div>

      <div class="field">
        <label>
          <input
            type="checkbox"
            bind:checked={focusEnabled}
          />
          勿扰模式 (睡眠 + 静音 ambient)
        </label>
      </div>

      <div class="field">
        <label>
          <input
            type="checkbox"
            checked={autostart}
            onchange={toggleAutostart}
          />
          开机自启动
        </label>
      </div>

      <div class="field">
        <label>
          <input
            type="checkbox"
            bind:checked={useFocusWindow}
            disabled={!focusEnabled}
          />
          限定时段 (留空=一直勿扰)
        </label>
        <div class="row" class:disabled={!focusEnabled || !useFocusWindow}>
          <input
            type="time"
            bind:value={focusStart}
            disabled={!focusEnabled || !useFocusWindow}
            aria-label="勿扰开始"
          />
          <span>至</span>
          <input
            type="time"
            bind:value={focusEnd}
            disabled={!focusEnabled || !useFocusWindow}
            aria-label="勿扰结束"
          />
        </div>
      </div>

      <div class="field">
        <label for="cwd">工作目录 (agent CLI 运行目录)</label>
        <input
          id="cwd"
          type="text"
          bind:value={workingDir}
          placeholder="留空 = 使用进程当前目录"
        />
        <small>对话发送时 agent 子进程的 cwd。新建对话时作为默认预填值；留空则使用宠物进程的当前目录。</small>
      </div>

      <div class="field">
        <label for="resp-persons">代办责任人</label>
        <input
          id="resp-persons"
          type="text"
          bind:value={responsiblePersons}
          placeholder="用英文逗号分隔，如：张三,李四,王五"
        />
        <small>代办页的责任人下拉列表从这里的配置读取。</small>
      </div>

      <!-- Task 5: per-agent default model. Each agent has a row with a
           探测模型 button (kicks off listModels), a dropdown OR manual
           text input (depending on the probe result + the user's
           toggle), + a clear button. The selected default_model is
           persisted on the global 保存 button. -->
      <fieldset class="agent-models">
        <legend>Agent 默认模型</legend>
        <small class="hint">每个 agent 探测并选择一个默认模型，新建对话时预填。探测失败可手动输入。</small>
        {#each PROVIDERS as p (p)}
          <div class="agent-row">
            <span class="agent-name">{PROVIDER_LABEL[p]}</span>
            <button
              class="probe-btn"
              onclick={() => probeModels(p)}
              disabled={probeState[p].probing}
            >
              {probeState[p].probing ? "探测中…" : "探测模型"}
            </button>
            <div class="model-pick">
              {#if probeState[p].probing}
                <span class="probing-label">探测中…</span>
              {:else if probeState[p].manual || probeState[p].list.length === 0}
                <input
                  type="text"
                  bind:value={providerModels[p]}
                  placeholder="手动输入模型名 (留空 = 默认)"
                  aria-label={`${PROVIDER_LABEL[p]} 默认模型`}
                />
                {#if probeState[p].list.length === 0}
                  <small class="muted">未探测到模型</small>
                {:else}
                  <small class="muted">已切换手动输入</small>
                {/if}
                {#if probeState[p].list.length > 0}
                  <button class="link-btn" onclick={() => toggleManual(p)}>
                    改用下拉选择
                  </button>
                {/if}
              {:else}
                <select bind:value={providerModels[p]}>
                  {#if providerModels[p] === null || providerModels[p] === ""}
                    <option value={null}>(默认模型)</option>
                  {/if}
                  {#each probeState[p].list as m (m)}
                    <option value={m}>{m}</option>
                  {/each}
                </select>
                <button class="link-btn" onclick={() => toggleManual(p)}>
                  手动输入
                </button>
              {/if}
            </div>
            {#if probeState[p].error}
              <small class="error-msg">{probeState[p].error}</small>
            {/if}
          </div>
        {/each}
      </fieldset>

      <div class="actions">
        <button onclick={save}>保存</button>
        {#if savedMsg}<span class="saved">{savedMsg}</span>{/if}
      </div>
    </div>
  {/if}
  {#if errorMsg}<div class="error">{errorMsg}</div>{/if}
</section>

<style>
  /* Task 4: refined restyle — hex → theme.css tokens. Matches the
   * ChatView look. Per the brief: section headings (legend) → ink 500;
   * labels → mut; inputs → bd border + ink text + acc focus; checkboxes
   * → acc when on (via accent-color, the modern native-control theming);
   * buttons → acc + #fff + pill. The "已保存" indicator drops green
   * (Task 3's克制 approach — no green token exists) → acc instead. The
   * per-agent probe blocks share the same input/button treatment. Radius:
   * inputs → --r-code, buttons → --r-chip, fieldset → --r-code. Spacing:
   * 8px rhythm. No soft shadow (flat tab content, scrollable form). */

  .settings {
    flex: 1;
    overflow-y: auto;
    background: var(--bg);
    padding: var(--space-3);
  }

  .loading {
    padding: var(--space-5);
    text-align: center;
    color: var(--mut);
    font-size: 0.88rem;
  }

  .form {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }

  /* Labels are mut (per brief); the fieldset legend is the section
   * heading → ink 500 (see .agent-models legend below). */
  .field label {
    font-size: 0.82rem;
    color: var(--mut);
    font-weight: 500;
    display: flex;
    align-items: center;
    gap: var(--space-1);
  }

  .field input[type="text"],
  .field input[type="number"],
  .field input[type="time"],
  .field select {
    font-size: 0.85rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--surface);
    color: var(--ink);
    transition: border-color var(--t-fast);
  }

  .field input[type="text"]:focus,
  .field input[type="number"]:focus,
  .field input[type="time"]:focus,
  .field select:focus {
    border-color: var(--acc);
    outline: none;
  }

  /* Checkboxes: accent-color themes the native checked state to the
   * accent indigo — "switch var(--acc) when on" per the brief, no
   * custom switch markup needed. Works in both light + dark. */
  .field input[type="checkbox"] {
    width: 16px;
    height: 16px;
    cursor: pointer;
    accent-color: var(--acc);
  }

  .field small {
    font-size: 0.7rem;
    color: var(--mut);
  }

  .field .row {
    display: flex;
    align-items: center;
    gap: var(--space-1);
  }

  .field .row.disabled {
    opacity: 0.5;
  }

  .field .row span {
    font-size: 0.8rem;
    color: var(--mut);
  }

  /* -- per-agent default model section (Task 5) --
   * The fieldset is a section card: surface fill + bd border + code
   * radius. The legend is the section heading → ink 500. */
  .agent-models {
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    padding: var(--space-3);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    background: var(--surface);
  }

  .agent-models legend {
    font-size: 0.82rem;
    color: var(--ink);
    font-weight: 500;
    padding: 0 var(--space-1);
  }

  .agent-models .hint {
    font-size: 0.7rem;
    color: var(--mut);
  }

  .agent-row {
    display: grid;
    grid-template-columns: 80px auto 1fr;
    grid-template-rows: auto auto;
    align-items: center;
    gap: var(--space-1) var(--space-2);
    padding: var(--space-1) var(--space-1);
    border-bottom: 1px solid var(--bd);
  }

  .agent-row:last-child {
    border-bottom: none;
  }

  .agent-name {
    font-size: 0.82rem;
    color: var(--ink);
    font-weight: 500;
    grid-row: 1;
    grid-column: 1;
  }

  .probe-btn {
    grid-row: 1;
    grid-column: 2;
    font-size: 0.78rem;
    padding: 3px var(--space-2);
    cursor: pointer;
    background: var(--acc);
    color: #fff;
    border: none;
    border-radius: var(--r-chip);
    transition: opacity var(--t-fast);
  }

  .probe-btn:hover {
    opacity: 0.9;
  }

  .probe-btn:disabled {
    background: var(--mut);
    cursor: not-allowed;
    opacity: 1;
  }

  .model-pick {
    grid-row: 1;
    grid-column: 3;
    display: flex;
    align-items: center;
    gap: var(--space-1);
    flex-wrap: wrap;
  }

  .model-pick select,
  .model-pick input[type="text"] {
    font-size: 0.82rem;
    padding: 3px var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--surface);
    color: var(--ink);
    min-width: 180px;
    flex: 1;
    transition: border-color var(--t-fast);
  }

  .model-pick select:focus,
  .model-pick input[type="text"]:focus {
    border-color: var(--acc);
    outline: none;
  }

  .probing-label {
    font-size: 0.78rem;
    color: var(--acc);
    font-style: italic;
  }

  .model-pick small.muted {
    font-size: 0.68rem;
    color: var(--mut);
  }

  .link-btn {
    background: transparent;
    border: none;
    color: var(--acc);
    font-size: 0.72rem;
    cursor: pointer;
    padding: 0;
    text-decoration: underline;
  }

  .link-btn:disabled {
    color: var(--mut);
    cursor: not-allowed;
  }

  .error-msg {
    grid-row: 2;
    grid-column: 2 / span 2;
    font-size: 0.72rem;
    color: var(--err);
  }

  .actions {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    margin-top: var(--space-1);
  }

  .actions button {
    padding: var(--space-1) var(--space-4);
    font-size: 0.88rem;
    cursor: pointer;
    background: var(--acc);
    color: #fff;
    border: none;
    border-radius: var(--r-chip);
    transition: opacity var(--t-fast);
  }

  .actions button:hover {
    opacity: 0.9;
  }

  /* "已保存" indicator: was green (#16a34a). No green token exists in
   * the design system; Task 3's克制 approach removes green. Use acc
   * (accent indigo) — a positive confirmation within the accent palette. */
  .actions .saved {
    font-size: 0.8rem;
    color: var(--acc);
  }

  .error {
    padding: var(--space-1) var(--space-3);
    background: var(--err-soft);
    color: var(--err);
    font-size: 0.8rem;
    border-top: 1px solid var(--err);
    margin-top: var(--space-2);
  }
</style>
