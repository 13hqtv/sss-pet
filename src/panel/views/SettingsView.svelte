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
      providerModels = {
        claude: s.providers.claude.default_model ?? null,
        codex: s.providers.codex.default_model ?? null,
        opencode: s.providers.opencode.default_model ?? null,
      };
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
  .settings {
    flex: 1;
    overflow-y: auto;
    background: #f8fafc;
    padding: 10px;
  }

  .loading {
    padding: 1.5rem;
    text-align: center;
    color: #64748b;
    font-size: 0.88rem;
  }

  .form {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .field label {
    font-size: 0.82rem;
    color: #334155;
    font-weight: 500;
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .field input[type="text"],
  .field input[type="number"],
  .field input[type="time"],
  .field select {
    font-size: 0.85rem;
    padding: 4px 8px;
    border: 1px solid #cbd5e1;
    border-radius: 4px;
    font-family: inherit;
    background: white;
    color: #1f2937;
  }

  .field input[type="checkbox"] {
    width: 16px;
    height: 16px;
    cursor: pointer;
  }

  .field small {
    font-size: 0.7rem;
    color: #94a3b8;
  }

  .field .row {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .field .row.disabled {
    opacity: 0.5;
  }

  .field .row span {
    font-size: 0.8rem;
    color: #64748b;
  }

  /* -- per-agent default model section (Task 5) -- */
  .agent-models {
    border: 1px solid #cbd5e1;
    border-radius: 6px;
    padding: 10px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    background: #f1f5f9;
  }

  .agent-models legend {
    font-size: 0.82rem;
    color: #334155;
    font-weight: 500;
    padding: 0 4px;
  }

  .agent-models .hint {
    font-size: 0.7rem;
    color: #94a3b8;
  }

  .agent-row {
    display: grid;
    grid-template-columns: 80px auto 1fr;
    grid-template-rows: auto auto;
    align-items: center;
    gap: 4px 8px;
    padding: 6px 4px;
    border-bottom: 1px solid #e2e8f0;
  }

  .agent-row:last-child {
    border-bottom: none;
  }

  .agent-name {
    font-size: 0.82rem;
    color: #1f2937;
    font-weight: 500;
    grid-row: 1;
    grid-column: 1;
  }

  .probe-btn {
    grid-row: 1;
    grid-column: 2;
    font-size: 0.78rem;
    padding: 3px 8px;
    cursor: pointer;
    background: #2563eb;
    color: white;
    border: none;
    border-radius: 4px;
  }

  .probe-btn:disabled {
    background: #94a3b8;
    cursor: not-allowed;
  }

  .model-pick {
    grid-row: 1;
    grid-column: 3;
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }

  .model-pick select,
  .model-pick input[type="text"] {
    font-size: 0.82rem;
    padding: 3px 6px;
    border: 1px solid #cbd5e1;
    border-radius: 4px;
    font-family: inherit;
    background: white;
    color: #1f2937;
    min-width: 180px;
    flex: 1;
  }

  .probing-label {
    font-size: 0.78rem;
    color: #2563eb;
    font-style: italic;
  }

  .model-pick small.muted {
    font-size: 0.68rem;
    color: #94a3b8;
  }

  .link-btn {
    background: transparent;
    border: none;
    color: #2563eb;
    font-size: 0.72rem;
    cursor: pointer;
    padding: 0;
    text-decoration: underline;
  }

  .error-msg {
    grid-row: 2;
    grid-column: 2 / span 2;
    font-size: 0.72rem;
    color: #991b1b;
  }

  .actions {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 4px;
  }

  .actions button {
    padding: 6px 16px;
    font-size: 0.88rem;
    cursor: pointer;
    background: #2563eb;
    color: white;
    border: none;
    border-radius: 4px;
  }

  .actions .saved {
    font-size: 0.8rem;
    color: #16a34a;
  }

  .error {
    padding: 6px 10px;
    background: #fee2e2;
    color: #991b1b;
    font-size: 0.8rem;
    border-top: 1px solid #fecaca;
    margin-top: 8px;
  }
</style>
