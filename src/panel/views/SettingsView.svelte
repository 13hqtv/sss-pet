<!-- Task 16: the 设置 (settings) tab. Default-provider select,
     follow_activity_min (number, min 900, ≥15min), 勿扰 toggle +
     optional time window (start/end), theme (system/light/dark),
     工作目录 (working dir for agent cwd — a path field). Save →
     settings_set_with_focus → config::save AND applies
     Signal::FocusMode to the brain immediately. Chinese. -->
<script lang="ts">
  import {
    getSettings,
    saveSettings,
    type Settings,
    type Provider,
  } from "../lib/settings";

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
      errorMsg = null;
    } catch (e) {
      errorMsg = `加载设置失败: ${e}`;
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
    };
    try {
      await saveSettings(newSettings);
      settings = newSettings;
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
        <small>对话发送时 agent 子进程的 cwd。留空则使用宠物进程的当前目录。</small>
      </div>

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
