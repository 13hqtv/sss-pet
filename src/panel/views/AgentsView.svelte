<!-- 智能体管理 tab. Two sub-sections: 智能体 (agent profiles) and 技能
     (skills). Agents are named profiles = provider + model + system_prompt
     + skills + mcp_config + cwd. Skills are reusable SKILL.md bodies
     assigned to agents. AI-powered creation calls agent_generate (one-shot
     LLM call) and parses the <agent_draft> JSON block to fill the form. -->
<script lang="ts">
  import {
    listAgents,
    getAgent,
    createAgent,
    updateAgent,
    deleteAgent,
    listSkills,
    createSkill,
    updateSkill,
    deleteSkill,
    listAgentSkills,
    addAgentSkill,
    removeAgentSkill,
    generateAgent,
    listModels,
    getExePath,
    type Agent,
    type Skill,
    type Provider,
  } from "../lib/api";

  // -- sub-section toggle --
  let subView: "agents" | "skills" = $state("agents");

  // -- core state --
  let agents: Agent[] = $state([]);
  let skills: Skill[] = $state([]);
  let errorMsg: string | null = $state(null);

  // -- agent form --
  let formOpen: boolean = $state(false);
  let editingId: string | null = $state(null);
  let formName: string = $state("");
  let formDescription: string = $state("");
  let formProvider: Provider = $state("claude");
  let formModel: string = $state("");
  let formSystemPrompt: string = $state("");
  let formMcpConfig: string = $state("");
  let formCwd: string = $state("");
  // skill assignment for the current form
  let formSkillIds: Set<string> = $state(new Set());
  // model probe state
  let modelList: string[] = $state([]);
  let probing: boolean = $state(false);
  let manualModel: boolean = $state(false);

  // -- AI create --
  let aiDescription: string = $state("");
  let aiProvider: Provider = $state("claude");
  let aiGenerating: boolean = $state(false);

  // -- skill form --
  let skillFormOpen: boolean = $state(false);
  let skillEditingId: string | null = $state(null);
  let skillFormName: string = $state("");
  let skillFormDescription: string = $state("");
  let skillFormContent: string = $state("");

  // -- delete confirmation --
  let pendingDeleteId: string | null = $state(null);
  let deleteTimer: ReturnType<typeof setTimeout> | null = null;

  const PROVIDERS: Provider[] = ["claude", "codex", "opencode"];
  const PROVIDER_LABEL: Record<Provider, string> = {
    claude: "Claude",
    codex: "Codex",
    opencode: "OpenCode",
  };

  // ===========================================================================
  // Helpers
  // ===========================================================================

  /** Parse an <agent_draft>{...}</agent_draft> JSON block from the LLM
   *  response. Returns the parsed object or null. */
  function parseAgentDraft(text: string): Record<string, string> | null {
    const match = text.match(/<agent_draft>\s*(\{[\s\S]*?\})\s*<\/agent_draft>/);
    if (!match) return null;
    try {
      return JSON.parse(match[1]);
    } catch {
      return null;
    }
  }

  /** Get skill names assigned to an agent (for display in the card). */
  function agentSkillNames(agent: Agent): string {
    // We don't have the assignment loaded per-card; show a count instead.
    return "";
  }

  // ===========================================================================
  // Actions
  // ===========================================================================

  async function refresh() {
    try {
      const [a, s] = await Promise.all([listAgents(), listSkills()]);
      agents = a;
      skills = s;
      errorMsg = null;
    } catch (e) {
      errorMsg = `加载失败: ${e}`;
    }
  }

  async function probeModels(provider: Provider) {
    probing = true;
    manualModel = false;
    modelList = [];
    try {
      const list = await listModels(provider);
      modelList = list;
      if (list.length === 0) {
        manualModel = true;
      }
    } catch {
      manualModel = true;
    } finally {
      probing = false;
    }
  }

  function startAdd() {
    formOpen = true;
    editingId = null;
    formName = "";
    formDescription = "";
    formProvider = "claude";
    formModel = "";
    formSystemPrompt = "";
    formMcpConfig = "";
    formCwd = "";
    formSkillIds = new Set();
    modelList = [];
    manualModel = false;
    void probeModels("claude");
  }

  async function startEdit(agent: Agent) {
    formOpen = true;
    editingId = agent.id;
    formName = agent.name;
    formDescription = agent.description ?? "";
    formProvider = agent.provider as Provider;
    formModel = agent.model ?? "";
    formSystemPrompt = agent.system_prompt ?? "";
    formMcpConfig = agent.mcp_config ?? "";
    formCwd = agent.cwd ?? "";
    formSkillIds = new Set();
    // Load skill assignments
    try {
      const assignments = await listAgentSkills(agent.id);
      for (const [skill, enabled] of assignments) {
        if (enabled) formSkillIds.add(skill.id);
      }
    } catch (e) {
      console.warn("[agents] load skills failed:", e);
    }
    void probeModels(formProvider);
  }

  function cancelForm() {
    formOpen = false;
    editingId = null;
  }

  async function saveForm() {
    const name = formName.trim();
    if (!name) {
      errorMsg = "名称不能为空";
      return;
    }
    try {
      const params = {
        name,
        description: formDescription.trim() || null,
        provider: formProvider,
        model: formModel.trim() || null,
        systemPrompt: formSystemPrompt.trim() || null,
        mcpConfig: formMcpConfig.trim() || null,
        cwd: formCwd.trim() || null,
      };
      if (editingId !== null) {
        await updateAgent({ id: editingId, ...params });
        // Sync skill assignments
        await syncSkills(editingId);
      } else {
        const newId = await createAgent(params);
        await syncSkills(newId);
      }
      formOpen = false;
      editingId = null;
      errorMsg = null;
      await refresh();
    } catch (e) {
      errorMsg = `保存失败: ${e}`;
    }
  }

  /** Sync skill assignments: add new ones, remove unselected ones. */
  async function syncSkills(agentId: string) {
    const current = await listAgentSkills(agentId);
    const currentIds = new Set(current.map(([s]) => s.id));
    // Add newly selected
    for (const id of formSkillIds) {
      if (!currentIds.has(id)) {
        await addAgentSkill(agentId, id);
      }
    }
    // Remove unselected
    for (const id of currentIds) {
      if (!formSkillIds.has(id)) {
        await removeAgentSkill(agentId, id);
      }
    }
  }

  function toggleSkill(skillId: string) {
    const next = new Set(formSkillIds);
    if (next.has(skillId)) {
      next.delete(skillId);
    } else {
      next.add(skillId);
    }
    formSkillIds = next;
  }

  async function aiGenerate() {
    const desc = aiDescription.trim();
    if (!desc) return;
    aiGenerating = true;
    errorMsg = null;
    try {
      const response = await generateAgent(desc, aiProvider);
      const draft = parseAgentDraft(response);
      if (draft) {
        // Populate the form with the generated values
        formOpen = true;
        editingId = null;
        formName = draft.name ?? "";
        formDescription = draft.description ?? "";
        const provider = draft.provider ?? "claude";
        formProvider = (PROVIDERS.includes(provider as Provider) ? provider : "claude") as Provider;
        formModel = draft.model ?? "";
        formSystemPrompt = draft.system_prompt ?? "";
        formMcpConfig = "";
        formCwd = "";
        formSkillIds = new Set();
        void probeModels(formProvider);
        aiDescription = "";
      } else {
        errorMsg = "AI 未生成有效的配置，请手动创建或重试";
      }
    } catch (e) {
      errorMsg = `AI 生成失败: ${e}`;
    } finally {
      aiGenerating = false;
    }
  }

  function onProviderChange() {
    formModel = "";
    void probeModels(formProvider);
  }

  /** Auto-inject the sss-pet-todos MCP server into the mcp_config JSON.
   *  Gets the current exe path and builds the server config. Merges
   *  into existing mcp_config if present, or creates a new one. */
  async function addTodoMcp() {
    try {
      const exePath = await getExePath();
      const serverConfig = { command: exePath, args: ["mcp-server"] };
      let config: Record<string, unknown> = {};
      if (formMcpConfig.trim()) {
        try {
          config = JSON.parse(formMcpConfig);
        } catch {
          // not valid JSON — start fresh
          config = {};
        }
      }
      // Ensure mcpServers key exists
      if (!config.mcpServers || typeof config.mcpServers !== "object") {
        config.mcpServers = {};
      }
      (config.mcpServers as Record<string, unknown>)["sss-pet-todos"] = serverConfig;
      formMcpConfig = JSON.stringify(config, null, 2);
    } catch (e) {
      errorMsg = `获取 exe 路径失败: ${e}`;
    }
  }

  // -- skill CRUD --

  function startAddSkill() {
    skillFormOpen = true;
    skillEditingId = null;
    skillFormName = "";
    skillFormDescription = "";
    skillFormContent = "";
  }

  function startEditSkill(skill: Skill) {
    skillFormOpen = true;
    skillEditingId = skill.id;
    skillFormName = skill.name;
    skillFormDescription = skill.description ?? "";
    skillFormContent = skill.content ?? "";
  }

  function cancelSkillForm() {
    skillFormOpen = false;
    skillEditingId = null;
  }

  async function saveSkillForm() {
    const name = skillFormName.trim();
    if (!name) {
      errorMsg = "技能名称不能为空";
      return;
    }
    try {
      if (skillEditingId !== null) {
        await updateSkill({
          id: skillEditingId,
          name,
          description: skillFormDescription.trim() || null,
          content: skillFormContent.trim() || null,
        });
      } else {
        await createSkill({
          name,
          description: skillFormDescription.trim() || null,
          content: skillFormContent.trim() || null,
        });
      }
      skillFormOpen = false;
      skillEditingId = null;
      errorMsg = null;
      await refresh();
    } catch (e) {
      errorMsg = `保存技能失败: ${e}`;
    }
  }

  function requestDelete(id: string) {
    if (pendingDeleteId === id) {
      if (deleteTimer) { clearTimeout(deleteTimer); deleteTimer = null; }
      pendingDeleteId = null;
      void confirmDelete(id);
      return;
    }
    pendingDeleteId = id;
    if (deleteTimer) clearTimeout(deleteTimer);
    deleteTimer = setTimeout(() => { pendingDeleteId = null; deleteTimer = null; }, 3000);
  }

  async function confirmDelete(id: string) {
    try {
      if (subView === "agents") {
        await deleteAgent(id);
      } else {
        await deleteSkill(id);
      }
      await refresh();
    } catch (e) {
      errorMsg = `删除失败: ${e}`;
    }
  }

  // Initial load on mount.
  $effect(() => {
    void refresh();
    return () => { if (deleteTimer) clearTimeout(deleteTimer); };
  });
</script>

<section class="agents-page">
  <!-- Sub-section toggle -->
  <div class="sub-tabs">
    <button class="sub-tab" class:active={subView === "agents"} onclick={() => (subView = "agents")}>
      智能体
    </button>
    <button class="sub-tab" class:active={subView === "skills"} onclick={() => (subView = "skills")}>
      技能
    </button>
  </div>

  {#if subView === "agents"}
    <!-- Agent section -->
    <div class="toolbar">
      <input
        type="text"
        placeholder="搜索智能体…"
        class="search"
      />
      <button class="btn-primary" onclick={startAdd} disabled={formOpen}>+ 手动创建</button>
    </div>

    <!-- AI create section -->
    {#if !formOpen}
      <div class="ai-create">
        <input
          type="text"
          bind:value={aiDescription}
          placeholder="描述你想要的智能体，AI 帮你生成配置…"
          class="ai-input"
          onkeydown={(e) => { if (e.key === "Enter" && !aiGenerating) void aiGenerate(); }}
        />
        <select bind:value={aiProvider} class="ai-provider" aria-label="AI 创建使用的 Provider">
          {#each PROVIDERS as p (p)}
            <option value={p}>{PROVIDER_LABEL[p]}</option>
          {/each}
        </select>
        <button class="btn-primary" onclick={aiGenerate} disabled={aiGenerating || !aiDescription.trim()}>
          {aiGenerating ? "生成中…" : "AI 生成"}
        </button>
      </div>
    {/if}

    <!-- Agent form (add/edit) -->
    {#if formOpen && editingId === null}
      {@render agentForm()}
    {/if}

    <!-- Agent list -->
    <div class="list">
      {#each agents as a (a.id)}
        {#if formOpen && editingId === a.id}
          {@render agentForm()}
        {:else}
          <div class="agent-card">
            <div class="card-head">
              <span class="agent-icon">🤖</span>
              <span class="agent-name">{a.name}</span>
              <span class="provider-badge">{PROVIDER_LABEL[a.provider as Provider] ?? a.provider}</span>
              {#if a.model}
                <span class="model-badge">{a.model}</span>
              {/if}
              <div class="card-spacer"></div>
              <button class="act" onclick={() => startEdit(a)}>编辑</button>
              <button
                class="act del"
                class:confirm={pendingDeleteId === a.id}
                onclick={() => requestDelete(a.id)}
              >
                {pendingDeleteId === a.id ? "确认?" : "删除"}
              </button>
            </div>
            {#if a.description}
              <div class="card-desc">{a.description}</div>
            {/if}
          </div>
        {/if}
      {/each}

      {#if agents.length === 0 && !formOpen}
        <div class="empty">
          <p>还没有智能体</p>
          <button class="btn-primary" onclick={startAdd}>+ 创建第一个</button>
        </div>
      {/if}
    </div>
  {:else}
    <!-- Skills section -->
    <div class="toolbar">
      <input
        type="text"
        placeholder="搜索技能…"
        class="search"
      />
      <button class="btn-primary" onclick={startAddSkill} disabled={skillFormOpen}>+ 新增技能</button>
    </div>

    {#if skillFormOpen}
      <div class="skill-form">
        <input
          type="text"
          bind:value={skillFormName}
          placeholder="技能名称"
          class="form-title"
        />
        <input
          type="text"
          bind:value={skillFormDescription}
          placeholder="一句话描述"
          class="form-desc"
        />
        <textarea
          bind:value={skillFormContent}
          placeholder="SKILL.md 正文内容"
          class="form-content"
          rows="8"
        ></textarea>
        <div class="form-actions">
          <button class="act cancel" onclick={cancelSkillForm}>取消</button>
          <button class="act save" onclick={saveSkillForm} disabled={!skillFormName.trim()}>保存</button>
        </div>
      </div>
    {/if}

    <div class="list">
      {#each skills as s (s.id)}
        <div class="skill-card">
          <div class="card-head">
            <span class="skill-icon">📦</span>
            <span class="agent-name">{s.name}</span>
            <div class="card-spacer"></div>
            <button class="act" onclick={() => startEditSkill(s)}>编辑</button>
            <button
              class="act del"
              class:confirm={pendingDeleteId === s.id}
              onclick={() => requestDelete(s.id)}
            >
              {pendingDeleteId === s.id ? "确认?" : "删除"}
            </button>
          </div>
          {#if s.description}
            <div class="card-desc">{s.description}</div>
          {/if}
        </div>
      {/each}

      {#if skills.length === 0 && !skillFormOpen}
        <div class="empty">
          <p>还没有技能</p>
          <button class="btn-primary" onclick={startAddSkill}>+ 创建第一个</button>
        </div>
      {/if}
    </div>
  {/if}

  {#if errorMsg}
    <div class="error">{errorMsg}</div>
  {/if}
</section>

<!-- Snippet: the agent create/edit form. -->
{#snippet agentForm()}
  <div class="agent-form">
    <input
      type="text"
      bind:value={formName}
      placeholder="名称（必填）"
      class="form-title"
    />
    <input
      type="text"
      bind:value={formDescription}
      placeholder="一句话描述"
      class="form-desc"
    />
    <div class="form-row">
      <select bind:value={formProvider} onchange={onProviderChange} aria-label="Provider">
        {#each PROVIDERS as p (p)}
          <option value={p}>{PROVIDER_LABEL[p]}</option>
        {/each}
      </select>
      {#if probing}
        <span class="probing">探测模型中…</span>
      {:else if manualModel || modelList.length === 0}
        <input type="text" bind:value={formModel} placeholder="模型名（留空=默认）" class="model-input" />
      {:else}
        <select bind:value={formModel} class="model-select">
          <option value="">（默认模型）</option>
          {#each modelList as m (m)}
            <option value={m}>{m}</option>
          {/each}
        </select>
      {/if}
    </div>
    <div class="form-row">
      <input type="text" bind:value={formCwd} placeholder="工作目录（留空=全局）" class="cwd-input" />
    </div>
    <textarea
      bind:value={formSystemPrompt}
      placeholder="系统提示词 — 定义智能体的角色、能力、行为规范"
      class="form-prompt"
      rows="6"
    ></textarea>
    <div class="mcp-row">
      <textarea
        bind:value={formMcpConfig}
        placeholder='MCP 配置（JSON 格式，可选）'
        class="form-mcp"
        rows="3"
      ></textarea>
      <button class="mcp-add-btn" onclick={addTodoMcp} title="自动添加代办 MCP 服务器配置">
        + 代办 MCP
      </button>
    </div>

    <!-- Skill assignment -->
    {#if skills.length > 0}
      <div class="skill-assign">
        <span class="skill-assign-label">技能</span>
        <div class="skill-assign-list">
          {#each skills as s (s.id)}
            <label class="skill-check">
              <input
                type="checkbox"
                checked={formSkillIds.has(s.id)}
                onchange={() => toggleSkill(s.id)}
              />
              <span>{s.name}</span>
            </label>
          {/each}
        </div>
      </div>
    {/if}

    <div class="form-actions">
      <button class="act cancel" onclick={cancelForm}>取消</button>
      <button class="act save" onclick={saveForm} disabled={!formName.trim()}>保存</button>
    </div>
  </div>
{/snippet}

<style>
  .agents-page {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    background: var(--bg);
  }

  .sub-tabs {
    flex-shrink: 0;
    display: flex;
    gap: var(--space-1);
    padding: var(--space-2) var(--space-3) 0;
    border-bottom: 1px solid var(--bd);
    background: var(--surface);
  }

  .sub-tab {
    padding: var(--space-1) var(--space-3);
    font-size: 0.8rem;
    background: transparent;
    border: none;
    color: var(--mut);
    cursor: pointer;
    border-bottom: 2px solid transparent;
    transition: color var(--t-fast), border-color var(--t-fast);
  }

  .sub-tab:hover {
    color: var(--ink);
  }

  .sub-tab.active {
    color: var(--acc);
    border-bottom-color: var(--acc);
  }

  .toolbar {
    flex-shrink: 0;
    display: flex;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--bd);
    background: var(--surface);
  }

  .search {
    flex: 1;
    font-size: 0.8rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--bg);
    color: var(--ink);
    transition: border-color var(--t-fast);
  }

  .search:focus { border-color: var(--acc); outline: none; }
  .search::placeholder { color: var(--mut); }

  .btn-primary {
    padding: var(--space-1) var(--space-3);
    font-size: 0.8rem;
    cursor: pointer;
    background: var(--acc);
    color: #fff;
    border: none;
    border-radius: var(--r-chip);
    transition: opacity var(--t-fast);
    white-space: nowrap;
  }
  .btn-primary:hover { opacity: 0.9; }
  .btn-primary:disabled { background: var(--mut); cursor: not-allowed; opacity: 1; }

  .ai-create {
    flex-shrink: 0;
    display: flex;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--bd);
    background: var(--acc-soft);
  }

  .ai-input {
    flex: 1;
    font-size: 0.8rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--surface);
    color: var(--ink);
    transition: border-color var(--t-fast);
  }
  .ai-input:focus { border-color: var(--acc); outline: none; }
  .ai-input::placeholder { color: var(--mut); }

  .ai-provider {
    font-size: 0.78rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--surface);
    color: var(--ink);
  }

  .list {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-2) var(--space-3);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  /* -- agent/skill card -- */
  .agent-card, .skill-card {
    background: var(--surface);
    border-radius: var(--r-bubble);
    padding: var(--space-2) var(--space-3);
    border-left: 3px solid var(--acc);
  }

  .card-head {
    display: flex;
    align-items: center;
    gap: var(--space-1);
  }

  .agent-icon, .skill-icon {
    font-size: 0.9rem;
    flex-shrink: 0;
  }

  .agent-name {
    flex: 1;
    min-width: 0;
    font-size: 0.85rem;
    color: var(--ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 500;
  }

  .provider-badge {
    flex-shrink: 0;
    font-size: 0.65rem;
    padding: 1px 6px;
    border-radius: var(--r-chip);
    background: var(--acc-soft);
    color: var(--acc);
    font-weight: 500;
  }

  .model-badge {
    flex-shrink: 0;
    font-size: 0.65rem;
    padding: 1px 6px;
    border-radius: var(--r-chip);
    background: var(--bd);
    color: var(--mut);
  }

  .card-spacer { flex: 1; }

  .card-desc {
    margin-top: var(--space-1);
    font-size: 0.75rem;
    color: var(--mut);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* -- action buttons -- */
  .act {
    background: transparent;
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    color: var(--mut);
    padding: 2px var(--space-2);
    font-size: 0.7rem;
    cursor: pointer;
    transition: all var(--t-fast);
    flex-shrink: 0;
  }
  .act:hover { background: var(--acc-soft); color: var(--acc); border-color: var(--acc); }
  .act.del:hover { background: var(--err-soft); color: var(--err); border-color: var(--err); }
  .act.del.confirm { background: var(--err); color: #fff; border-color: var(--err); }
  .act.save { background: var(--acc); color: #fff; border-color: var(--acc); }
  .act.save:disabled { background: var(--mut); border-color: var(--mut); cursor: not-allowed; }

  /* -- agent form -- */
  .agent-form, .skill-form {
    background: var(--surface);
    border: 1px solid var(--bd);
    border-radius: var(--r-bubble);
    padding: var(--space-3);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .form-title, .form-desc {
    font-size: 0.82rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--bg);
    color: var(--ink);
    transition: border-color var(--t-fast);
  }
  .form-title:focus, .form-desc:focus { border-color: var(--acc); outline: none; }
  .form-title::placeholder, .form-desc::placeholder { color: var(--mut); }

  .form-row {
    display: flex;
    gap: var(--space-2);
  }

  .form-row select, .model-input, .model-select, .cwd-input {
    flex: 1;
    font-size: 0.78rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--surface);
    color: var(--ink);
    min-width: 0;
  }
  .form-row select:focus, .model-input:focus, .model-select:focus, .cwd-input:focus {
    border-color: var(--acc);
    outline: none;
  }

  .probing {
    font-size: 0.78rem;
    color: var(--acc);
    font-style: italic;
  }

  .form-prompt, .form-mcp, .form-content {
    font-size: 0.78rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: var(--mono);
    background: var(--bg);
    color: var(--ink);
    resize: vertical;
    transition: border-color var(--t-fast);
  }
  .form-prompt:focus, .form-mcp:focus, .form-content:focus {
    border-color: var(--acc);
    outline: none;
  }
  .form-prompt::placeholder, .form-mcp::placeholder, .form-content::placeholder {
    color: var(--mut);
  }

  .mcp-row {
    display: flex;
    gap: var(--space-2);
    align-items: flex-start;
  }

  .mcp-row .form-mcp {
    flex: 1;
  }

  .mcp-add-btn {
    flex-shrink: 0;
    font-size: 0.68rem;
    padding: var(--space-1) var(--space-2);
    cursor: pointer;
    background: var(--acc-soft);
    color: var(--acc);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    transition: all var(--t-fast);
    white-space: nowrap;
    margin-top: 2px;
  }

  .mcp-add-btn:hover {
    background: var(--acc);
    color: #fff;
    border-color: var(--acc);
  }

  .skill-assign {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }

  .skill-assign-label {
    font-size: 0.75rem;
    color: var(--mut);
    font-weight: 500;
  }

  .skill-assign-list {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }

  .skill-check {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 0.75rem;
    color: var(--ink);
    cursor: pointer;
  }

  .skill-check input[type="checkbox"] {
    width: 14px;
    height: 14px;
    accent-color: var(--acc);
  }

  .form-actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }

  /* -- empty state -- */
  .empty {
    padding: var(--space-5);
    text-align: center;
    color: var(--mut);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-3);
  }
  .empty p { font-size: 0.85rem; margin: 0; }

  /* -- error -- */
  .error {
    flex-shrink: 0;
    padding: var(--space-1) var(--space-3);
    background: var(--err-soft);
    color: var(--err);
    font-size: 0.78rem;
    border-top: 1px solid var(--err);
  }
</style>
