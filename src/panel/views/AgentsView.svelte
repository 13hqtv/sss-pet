<!-- 智能体管理 tab. Two sub-sections: 智能体 (agent profiles) and 技能
     (skills). Agents are named profiles = provider + model + system_prompt
     + skills + mcp_config + cwd. Skills are reusable SKILL.md bodies
     assigned to agents. AI-powered creation is a multi-turn CONVERSATION:
     the user picks provider + model + describes the agent; the LLM streams
     an `<agent_draft>` JSON block back IN the conversation; the user
     confirms (→ agent_create), continues modifying (→ another turn), or
     cancels (→ discard). -->
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
    skillFileList,
    skillFileReplaceAll,
    listAgentSkills,
    addAgentSkill,
    removeAgentSkill,
    agentGenStart,
    agentGenContinue,
    agentGenHistory,
    agentGenCancel,
    automationList,
    automationCreate,
    automationUpdate,
    automationDelete,
    automationSetEnabled,
    automationRunNow,
    automationRunList,
    automationRunMessages,
    listModels,
    getExePath,
    type Agent,
    type Automation,
    type AutomationRun,
    type StoredMessage,
    type Message,
    type Skill,
    type Provider,
    type GenMessage,
    type ExecResult,
  } from "../lib/api";
  import { subscribeAgentGenStream, subscribeAutomationRunStream } from "../lib/stream";

  // -- sub-section toggle --
  let subView: "agents" | "skills" | "automation" = $state("agents");

  // -- core state --
  let agents: Agent[] = $state([]);
  let skills: Skill[] = $state([]);
  let errorMsg: string | null = $state(null);

  // -- automation --
  let automations: Automation[] = $state([]);
  let selAuto: string | null = $state(null);
  let autoEditing: boolean = $state(false);
  let autoSaving: boolean = $state(false);
  let autoForm = $state<{
    id: string | null;
    name: string;
    agentId: string;
    prompt: string;
    triggerType: "daily" | "interval";
    dailyTime: string;
    intervalValue: number;
    intervalUnit: "minutes" | "hours";
    enabled: boolean;
  }>({
    id: null,
    name: "",
    agentId: "",
    prompt: "",
    triggerType: "daily",
    dailyTime: "09:00",
    intervalValue: 60,
    intervalUnit: "minutes",
    enabled: true,
  });

  // -- automation run history + detail (Task 10) --
  let autoHistoryId: string | null = $state(null);
  let runs: AutomationRun[] = $state([]);
  let selRunId: string | null = $state(null);
  let runMsgs: StoredMessage[] = $state([]);
  // Live-streamed messages for the currently-running dispatch on the open
  // history (the `automation_run_message` event). Cleared when the run
  // settles (onResult/onError → reload runs).
  let liveMsgs: Message[] = $state([]);

  // -- agent form --
  // One named MCP server entry in the agent form: `name` is the server key
  // inside the stored `{"mcpServers": {<name>: <entry>}}` blob; `config` is
  // that entry's JSON text (e.g. {"command":"npx","args":[...],"env":{}}).
  interface McpEntry {
    name: string;
    config: string;
  }
  let formOpen: boolean = $state(false);
  let editingId: string | null = $state(null);
  let formName: string = $state("");
  let formDescription: string = $state("");
  let formProvider: Provider = $state("claude");
  let formModel: string = $state("");
  let formSystemPrompt: string = $state("");
  let formMcpConfig: string = $state("");
  // Named MCP server entries parsed from formMcpConfig. The UI edits these
  // (name + per-server JSON config); formMcpConfig is re-serialized from
  // them on change so save/load stay shape-compatible with the stored
  // {"mcpServers": {<name>: {<entry>}}} blob.
  let mcpEntries: McpEntry[] = $state([]);
  let formCwd: string = $state("");
  // skill assignment for the current form
  let formSkillIds: Set<string> = $state(new Set());
  // model probe state
  let modelList: string[] = $state([]);
  let probing: boolean = $state(false);
  let manualModel: boolean = $state(false);

  // -- AI generation (multi-turn conversation) --
  // genOpen = the generation panel is mounted (setup OR conversation
  // phase). genId = null during setup (provider+model+description), set
  // once agent_gen_start returns. genDraft = the latest parsed
  // <agent_draft> (drives the 确认/继续修改/取消 buttons). genStreaming
  // = a turn's drain task is still running (disables the composer).
  let genOpen: boolean = $state(false);
  let genId: string | null = $state(null);
  let genProvider: Provider = $state("claude");
  let genModel: string = $state("");
  let genModelList: string[] = $state([]);
  let genProbing: boolean = $state(false);
  let genManualModel: boolean = $state(false);
  let genDescription: string = $state("");
  let genMessages: GenMessage[] = $state([]);
  let genStreaming: boolean = $state(false);
  let genDraft: Record<string, string> | null = $state(null);
  let genModInput: string = $state("");
  let genInputEl: HTMLTextAreaElement | null = $state(null);

  // -- skill form --
  let skillFormOpen: boolean = $state(false);
  let skillEditingId: string | null = $state(null);
  let skillFormName: string = $state("");
  let skillFormDescription: string = $state("");
  let skillFormContent: string = $state("");
  // supporting script files for the skill being edited (working set,
  // saved as a full-replace on saveSkillForm). formFileEditingIdx: null
  // = not editing; >=0 = editing that index; -1 = adding a new file.
  let formSkillFiles: Array<{ path: string; content: string }> = $state([]);
  let formFileEditingIdx: number | null = $state(null);
  let formFilePath: string = $state("");
  let formFileContent: string = $state("");

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

  /** Parse the LATEST `<agent_draft>{...}</agent_draft>` JSON block from
   *  the generation conversation. The stream may split a single draft
   *  block across multiple Text deltas, so we concatenate all assistant
   *  Text content first; then find every block + return the LAST valid
   *  one (a revision turn produces a fresh draft — the last wins). */
  function parseLatestDraft(msgs: GenMessage[]): Record<string, string> | null {
    const full = msgs
      .filter((m) => m.role === "assistant" && m.kind === "Text")
      .map((m) => m.content)
      .join("");
    const re = /<agent_draft>\s*(\{[\s\S]*?\})\s*<\/agent_draft>/g;
    let last: Record<string, string> | null = null;
    let match: RegExpExecArray | null;
    while ((match = re.exec(full)) !== null) {
      try {
        last = JSON.parse(match[1]);
      } catch {
        // malformed block — skip, keep scanning for a later valid one
      }
    }
    return last;
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
      const [a, s, au] = await Promise.all([
        listAgents(),
        listSkills(),
        automationList(),
      ]);
      agents = a;
      skills = s;
      automations = au;
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
    mcpEntries = [];
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
    mcpEntries = parseMcpEntries(formMcpConfig);
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
    // Re-serialize the named-entry editor into the stored blob, and refuse
    // to save while any filled-in entry has invalid JSON config (the user
    // would silently lose that server on save).
    syncMcpConfig();
    const hasInvalidMcp = mcpEntries.some(
      (e) => e.name.trim() !== "" && !entryConfigValid(e.config),
    );
    if (hasInvalidMcp) {
      errorMsg = "存在 JSON 格式错误的 MCP 配置，请修正后再保存";
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

  // -- AI generation conversation flow --

  /** Open the generation panel in the setup phase (provider + model +
   *  description). Resets any prior generation state. */
  function openGen() {
    if (genId) {
      // A generation is already in flight — just reveal the panel.
      genOpen = true;
      return;
    }
    genOpen = true;
    genId = null;
    genProvider = "claude";
    genModel = "";
    genModelList = [];
    genManualModel = false;
    genDescription = "";
    genMessages = [];
    genDraft = null;
    genStreaming = false;
    genModInput = "";
    void probeGenModels("claude");
  }

  /** Probe the chosen provider for available models (reuses the same
   *  list_models path as the create form). Empty list → manual text
   *  entry fallback. */
  async function probeGenModels(provider: Provider) {
    genProbing = true;
    genManualModel = false;
    genModelList = [];
    try {
      const list = await listModels(provider);
      genModelList = list;
      if (list.length === 0) genManualModel = true;
    } catch {
      genManualModel = true;
    } finally {
      genProbing = false;
    }
  }

  function onGenProviderChange() {
    genModel = "";
    void probeGenModels(genProvider);
  }

  /** Kick off the first turn: call agent_gen_start → genId; the user's
   *  description is buffered server-side + the assistant's streamed
   *  draft arrives via agent_gen_message events. We seed the optimistic
   *  user bubble locally so the conversation renders immediately. */
  async function beginGen() {
    const desc = genDescription.trim();
    if (!desc) return;
    genStreaming = true;
    genDraft = null;
    errorMsg = null;
    try {
      const id = await agentGenStart(genProvider, genModel || null, desc);
      genId = id;
      genMessages = [{ role: "user", kind: "Text", content: desc, tool: null }];
    } catch (e) {
      errorMsg = `开始生成失败: ${e}`;
      genStreaming = false;
    }
  }

  /** Send a modification prompt (the 继续修改 path). The backend resumes
   *  the native session (or replays history) + streams a revised draft;
   *  genDraft is cleared so the action buttons hide until the new draft
   *  arrives. */
  async function sendGenModification() {
    if (!genId) return;
    const p = genModInput.trim();
    if (!p || genStreaming) return;
    genModInput = "";
    genStreaming = true;
    genDraft = null;
    errorMsg = null;
    try {
      await agentGenContinue(genId, p);
      genMessages = [...genMessages, { role: "user", kind: "Text", content: p, tool: null }];
    } catch (e) {
      errorMsg = `发送失败: ${e}`;
      genStreaming = false;
    }
  }

  function onGenKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void sendGenModification();
    }
  }

  /** 确认: parse the latest draft + create the agent via the existing
   *  agent_create command (provider + model from the generation
   *  conversation's pinning; name/description/system_prompt from the
   *  draft). Then cancel the generation session + close the panel. */
  async function confirmGen() {
    if (!genDraft) return;
    const name = (genDraft.name ?? "").trim();
    if (!name) {
      errorMsg = "草稿缺少名称，请继续修改";
      return;
    }
    try {
      await createAgent({
        name,
        description: (genDraft.description ?? "").trim() || null,
        provider: genProvider,
        model: genModel.trim() || null,
        systemPrompt: (genDraft.system_prompt ?? "").trim() || null,
        mcpConfig: null,
        cwd: null,
      });
      if (genId) {
        try { await agentGenCancel(genId); } catch { /* ignore */ }
      }
      closeGen();
      errorMsg = null;
      await refresh();
    } catch (e) {
      errorMsg = `创建失败: ${e}`;
    }
  }

  /** 取消: drop the in-memory generation session + close the panel. No
   *  agent is created. */
  async function cancelGen() {
    if (genId) {
      try { await agentGenCancel(genId); } catch { /* ignore */ }
    }
    closeGen();
  }

  function closeGen() {
    genOpen = false;
    genId = null;
    genMessages = [];
    genDraft = null;
    genStreaming = false;
    genDescription = "";
    genModInput = "";
  }

  /** 继续修改: focus the composer so the user knows to type their
   *  modification. The composer is always available while a genId is
   *  active; this button is the explicit affordance. */
  function focusGenInput() {
    genInputEl?.focus();
  }

  // -- agent_gen_* event handlers (routed by gen_id) --

  function handleGenMessage(recvGenId: string, message: GenMessage) {
    if (recvGenId !== genId) return;
    genMessages = [...genMessages, message];
  }

  async function handleGenResult(recvGenId: string, _result: ExecResult) {
    if (recvGenId !== genId) return;
    genStreaming = false;
    // Reload the full history to sync (recovers any messages the live
    // listener missed while the panel was hidden — Review Focus #3
    // parity). Then parse the latest draft from the authoritative copy.
    if (genId) {
      try {
        const full = await agentGenHistory(genId);
        genMessages = full;
      } catch {
        // session gone (cancelled / lost to restart) — leave the live
        // buffer as-is.
      }
    }
    genDraft = parseLatestDraft(genMessages);
  }

  function handleGenError(recvGenId: string, message: string) {
    if (recvGenId !== genId) return;
    errorMsg = `生成错误: ${message}`;
  }

  function onProviderChange() {
    formModel = "";
    void probeModels(formProvider);
  }

  /** Parse the stored {"mcpServers": {name: cfg, ...}} blob into named
   *  entries for editing. Returns [] when the blob is empty / invalid /
   *  has no mcpServers object — the form then starts with an empty list. */
  function parseMcpEntries(json: string): McpEntry[] {
    const trimmed = json.trim();
    if (!trimmed) return [];
    try {
      const parsed = JSON.parse(trimmed);
      const servers = parsed?.mcpServers;
      if (!servers || typeof servers !== "object") return [];
      return Object.entries(servers).map(([name, cfg]) => ({
        name,
        config: JSON.stringify(cfg, null, 2),
      }));
    } catch {
      return [];
    }
  }

  /** Serialize named entries back into the {"mcpServers": {name: cfg}} blob.
   *  Entries with an empty name or invalid JSON config are skipped (the
   *  entry itself flags the error in the UI). Returns "" when no valid
   *  entries remain, so save sends `null` (no MCP override). */
  function serializeMcpEntries(entries: McpEntry[]): string {
    const mcpServers: Record<string, unknown> = {};
    for (const e of entries) {
      const name = e.name.trim();
      if (!name) continue;
      try {
        mcpServers[name] = JSON.parse(e.config);
      } catch {
        // invalid JSON — skipped; the entry shows its own error
      }
    }
    if (Object.keys(mcpServers).length === 0) return "";
    return JSON.stringify({ mcpServers }, null, 2);
  }

  /** Re-serialize mcpEntries into formMcpConfig (the field save reads). */
  function syncMcpConfig() {
    formMcpConfig = serializeMcpEntries(mcpEntries);
  }

  /** A config box is valid when empty (skipped on save) or parses as JSON. */
  function entryConfigValid(config: string): boolean {
    if (!config.trim()) return true;
    try {
      JSON.parse(config);
      return true;
    } catch {
      return false;
    }
  }

  function addMcpEntry() {
    mcpEntries.push({ name: "", config: "" });
    syncMcpConfig();
  }

  function removeMcpEntry(i: number) {
    mcpEntries.splice(i, 1);
    syncMcpConfig();
  }

  /** Auto-inject the sss-pet-todos MCP server as a named entry. Gets the
   *  current exe path and builds the server config; replaces an existing
   *  "sss-pet-todos" entry or adds a new one. */
  async function addTodoMcp() {
    try {
      const exePath = await getExePath();
      const cfg = JSON.stringify({ command: exePath, args: ["mcp-server"] }, null, 2);
      const i = mcpEntries.findIndex((e) => e.name === "sss-pet-todos");
      if (i >= 0) {
        mcpEntries[i].config = cfg;
      } else {
        mcpEntries.push({ name: "sss-pet-todos", config: cfg });
      }
      syncMcpConfig();
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
    formSkillFiles = [];
    formFileEditingIdx = null;
    formFilePath = "";
    formFileContent = "";
  }

  async function startEditSkill(skill: Skill) {
    // Load the skill's files BEFORE opening the form — if the load fails
    // (transient IPC/db error), abort + show an error rather than opening
    // with an empty file list. Otherwise a subsequent 保存 would call
    // skillFileReplaceAll([]) and silently wipe every existing script.
    let loadedFiles: Array<{ path: string; content: string }> = [];
    try {
      const files = await skillFileList(skill.id);
      loadedFiles = files.map((f) => ({ path: f.path, content: f.content }));
    } catch (e) {
      errorMsg = `加载技能脚本失败，未打开编辑（请重试）: ${e}`;
      return;
    }
    skillFormOpen = true;
    skillEditingId = skill.id;
    skillFormName = skill.name;
    skillFormDescription = skill.description ?? "";
    skillFormContent = skill.content ?? "";
    formSkillFiles = loadedFiles;
    formFileEditingIdx = null;
    formFilePath = "";
    formFileContent = "";
  }

  function cancelSkillForm() {
    skillFormOpen = false;
    skillEditingId = null;
    formFileEditingIdx = null;
  }

  async function saveSkillForm() {
    const name = skillFormName.trim();
    if (!name) {
      errorMsg = "技能名称不能为空";
      return;
    }
    try {
      let skillId = skillEditingId;
      if (skillEditingId !== null) {
        await updateSkill({
          id: skillEditingId,
          name,
          description: skillFormDescription.trim() || null,
          content: skillFormContent.trim() || null,
        });
      } else {
        skillId = await createSkill({
          name,
          description: skillFormDescription.trim() || null,
          content: skillFormContent.trim() || null,
        });
      }
      // Full-replace the supporting files (SKILL.md paths are skipped
      // server-side — the body lives in skills.content).
      if (skillId !== null) {
        await skillFileReplaceAll(skillId, formSkillFiles);
      }
      skillFormOpen = false;
      skillEditingId = null;
      errorMsg = null;
      await refresh();
    } catch (e) {
      errorMsg = `保存技能失败: ${e}`;
    }
  }

  // -- skill file manager (inline add/edit/delete) --

  function startAddSkillFile() {
    formFileEditingIdx = -1;
    formFilePath = "";
    formFileContent = "";
  }

  function startEditSkillFile(idx: number) {
    formFileEditingIdx = idx;
    formFilePath = formSkillFiles[idx].path;
    formFileContent = formSkillFiles[idx].content;
  }

  function cancelSkillFileEdit() {
    formFileEditingIdx = null;
    formFilePath = "";
    formFileContent = "";
  }

  function saveSkillFile() {
    const path = formFilePath.trim();
    if (!path) {
      errorMsg = "文件路径不能为空";
      return;
    }
    if (path === "SKILL.md" || path.toLowerCase() === "skill.md") {
      errorMsg = "SKILL.md 是保留路径（正文在上方的 SKILL.md 编辑区）";
      return;
    }
    const entry = { path, content: formFileContent };
    if (formFileEditingIdx === -1) {
      // new — reject duplicate path
      if (formSkillFiles.some((f) => f.path === path)) {
        errorMsg = `已存在同路径文件: ${path}`;
        return;
      }
      formSkillFiles = [...formSkillFiles, entry];
    } else if (formFileEditingIdx !== null) {
      // editing existing — allow path change if no OTHER file collides
      const dup = formSkillFiles.findIndex(
        (f, i) => i !== formFileEditingIdx && f.path === path,
      );
      if (dup !== -1) {
        errorMsg = `已存在同路径文件: ${path}`;
        return;
      }
      const next = [...formSkillFiles];
      next[formFileEditingIdx] = entry;
      formSkillFiles = next;
    }
    formFileEditingIdx = null;
    formFilePath = "";
    formFileContent = "";
    errorMsg = null;
  }

  function deleteSkillFile(idx: number) {
    formSkillFiles = formSkillFiles.filter((_, i) => i !== idx);
    if (formFileEditingIdx === idx) {
      formFileEditingIdx = null;
      formFilePath = "";
      formFileContent = "";
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

  // Initial load on mount + subscribe to the agent_gen_* event stream
  // for the lifetime of this view (the generation panel lives here).
  // ===========================================================================
  // Automation actions
  // ===========================================================================

  function resetAutoForm() {
    autoForm.id = null;
    autoForm.name = "";
    autoForm.agentId = "";
    autoForm.prompt = "";
    autoForm.triggerType = "daily";
    autoForm.dailyTime = "09:00";
    autoForm.intervalValue = 60;
    autoForm.intervalUnit = "minutes";
    autoForm.enabled = true;
  }

  function startNewAuto() {
    resetAutoForm();
    if (agents.length > 0) autoForm.agentId = agents[0].id;
    autoEditing = true;
  }

  function startEditAuto(auto: Automation) {
    autoForm.id = auto.id;
    autoForm.name = auto.name;
    autoForm.agentId = auto.agent_id;
    autoForm.prompt = auto.prompt;
    autoForm.triggerType = auto.trigger_type;
    if (auto.trigger_type === "daily") {
      autoForm.dailyTime = auto.trigger_config || "09:00";
    } else {
      const mins = parseInt(auto.trigger_config, 10) || 60;
      if (mins >= 60 && mins % 60 === 0) {
        autoForm.intervalValue = mins / 60;
        autoForm.intervalUnit = "hours";
      } else {
        autoForm.intervalValue = mins;
        autoForm.intervalUnit = "minutes";
      }
    }
    autoForm.enabled = auto.enabled;
    selAuto = auto.id;
    autoEditing = true;
  }

  function triggerConfigFromForm(): string {
    if (autoForm.triggerType === "daily") return autoForm.dailyTime;
    const mins =
      autoForm.intervalUnit === "hours"
        ? autoForm.intervalValue * 60
        : autoForm.intervalValue;
    return String(mins);
  }

  /** "每天 09:00" / "每 60 分钟" / "每 2 小时" */
  function fmtTrigger(auto: Automation): string {
    if (auto.trigger_type === "daily") return `每天 ${auto.trigger_config}`;
    const mins = parseInt(auto.trigger_config, 10);
    if (auto.trigger_type === "interval") {
      if (mins >= 60 && mins % 60 === 0) return `每 ${mins / 60} 小时`;
      return `每 ${mins} 分钟`;
    }
    return auto.trigger_config;
  }

  function fmtLocal(rfc: string | null): string {
    if (!rfc) return "—";
    try {
      return new Date(rfc).toLocaleString("zh-CN", {
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
      });
    } catch {
      return rfc;
    }
  }

  async function loadAutomations() {
    try {
      automations = await automationList();
    } catch (e) {
      errorMsg = `加载自动化失败: ${e}`;
    }
  }

  async function saveAuto() {
    if (!autoForm.name || !autoForm.agentId) return;
    autoSaving = true;
    try {
      const params = {
        name: autoForm.name,
        agentId: autoForm.agentId,
        prompt: autoForm.prompt,
        triggerType: autoForm.triggerType,
        triggerConfig: triggerConfigFromForm(),
        enabled: autoForm.enabled,
      };
      if (autoForm.id) {
        await automationUpdate({ id: autoForm.id, ...params });
      } else {
        await automationCreate(params);
      }
      autoEditing = false;
      await loadAutomations();
      errorMsg = null;
    } catch (e) {
      errorMsg = `保存失败: ${e}`;
    } finally {
      autoSaving = false;
    }
  }

  async function deleteAuto(id: string) {
    if (!confirm("删除这个自动化任务？历史记录也会一并删除。")) return;
    try {
      await automationDelete(id);
      autoEditing = false;
      await loadAutomations();
      errorMsg = null;
    } catch (e) {
      errorMsg = `删除失败: ${e}`;
    }
  }

  async function toggleAutoEnabled(auto: Automation) {
    try {
      await automationSetEnabled(auto.id, !auto.enabled);
      await loadAutomations();
    } catch (e) {
      errorMsg = `切换失败: ${e}`;
    }
  }

  async function runNow(auto: Automation) {
    try {
      await automationRunNow(auto.id);
      errorMsg = null;
      // reload so the list reflects has_running=true (disables the button
      // until the run settles); the stream's onRunsChanged reloads again
      // when the run completes, flipping has_running back to false.
      await loadAutomations();
      if (autoHistoryId === auto.id) await loadRuns();
    } catch (e) {
      errorMsg = `启动失败: ${e}`;
    }
  }

  // -- run history + detail (Task 10) --

  function fmtRunStatus(status: string): string {
    if (status === "running") return "运行中";
    if (status === "completed") return "成功";
    if (status === "failed") return "失败";
    return status;
  }

  async function loadRuns() {
    if (!autoHistoryId) return;
    try {
      runs = await automationRunList(autoHistoryId);
      // clear detail if the selected run disappeared (automation deleted)
      if (selRunId && !runs.some((r) => r.id === selRunId)) {
        selRunId = null;
        runMsgs = [];
      }
    } catch (e) {
      errorMsg = `加载历史失败: ${e}`;
    }
  }

  async function loadRunMsgs(runId: string) {
    try {
      runMsgs = await automationRunMessages(runId);
    } catch (e) {
      errorMsg = `加载对话失败: ${e}`;
      runMsgs = [];
    }
  }

  function selectRun(run: AutomationRun) {
    selRunId = run.id;
    void loadRunMsgs(run.id);
  }

  function showHistory(auto: Automation) {
    autoHistoryId = auto.id;
    selRunId = null;
    runMsgs = [];
    liveMsgs = [];
    runs = [];
    void loadRuns();
  }

  function hideHistory() {
    autoHistoryId = null;
    selRunId = null;
    runMsgs = [];
    liveMsgs = [];
    runs = [];
  }

  $effect(() => {
    void refresh();
    // Subscribe once to agent_gen_message / agent_gen_result /
    // agent_gen_error, routed by gen_id. The handlers read the reactive
    // genId so they only act on the active generation. unlisten on
    // destroy.
    let unlisten: (() => void) | null = null;
    void subscribeAgentGenStream({
      onMessage: handleGenMessage,
      onResult: handleGenResult,
      onError: handleGenError,
    }).then((u) => {
      unlisten = u;
    });
    return () => {
      if (deleteTimer) clearTimeout(deleteTimer);
      if (unlisten) unlisten();
    };
  });

  // Subscribe once to the automation run stream: refresh the history list
  // + the selected run's detail whenever a run settles. Handlers read
  // autoHistoryId / selRunId live at event time (signal reads inside
  // callbacks return current value without re-triggering this effect).
  $effect(() => {
    let unlisten: (() => void) | null = null;
    void subscribeAutomationRunStream({
      onMessage: (_runId, automationId, message) => {
        // Live-stream the running dispatch's messages into the open history.
        if (automationId === autoHistoryId) {
          liveMsgs = [...liveMsgs, message];
        }
      },
      onRunsChanged: (automationId) => {
        if (automationId === autoHistoryId) {
          liveMsgs = [];
          void loadRuns();
          if (selRunId) void loadRunMsgs(selRunId);
        }
      },
      onResult: (_runId, automationId) => {
        if (automationId === autoHistoryId) {
          liveMsgs = [];
          void loadRuns();
          if (selRunId) void loadRunMsgs(selRunId);
        }
      },
      onError: (_runId, automationId) => {
        if (automationId === autoHistoryId) {
          liveMsgs = [];
          void loadRuns();
          if (selRunId) void loadRunMsgs(selRunId);
        }
      },
    }).then((u) => {
      unlisten = u;
    });
    return () => {
      if (unlisten) unlisten();
    };
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
    <button
      class="sub-tab"
      class:active={subView === "automation"}
      onclick={() => (subView = "automation")}
    >
      自动化
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
      <button class="btn-ghost" onclick={openGen} disabled={formOpen}>✨ AI 生成</button>
      <button class="btn-primary" onclick={startAdd} disabled={formOpen || genOpen}>+ 手动创建</button>
    </div>

    {#if genOpen}
      {@render genPanel()}
    {:else}
      <!-- Agent form (add) -->
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
    {/if}
  {:else if subView === "automation"}
    <!-- Automation section -->
    {#if autoHistoryId}
      <div class="auto-history">
        <div class="toolbar">
          <button onclick={hideHistory}>← 返回</button>
          <span class="auto-history-title">
            {automations.find((a) => a.id === autoHistoryId)?.name ?? "自动化"} — 运行历史
          </span>
        </div>
        <div class="auto-history-body">
          {#if liveMsgs.length > 0}
            <div class="live-panel card">
              <div class="live-head">运行中…（实时）</div>
              {#each liveMsgs as msg}
                <div class="msg live">
                  <span class="msg-kind">{msg.kind}</span>
                  <div class="msg-content">{msg.content}</div>
                </div>
              {/each}
            </div>
          {/if}
          <div class="runs-list">
            {#if runs.length === 0}
              <div class="empty">还没有运行记录。</div>
            {:else}
              {#each runs as run (run.id)}
                <button
                  class="run-row"
                  class:sel={selRunId === run.id}
                  onclick={() => selectRun(run)}
                >
                  <span class="run-status {run.status}">{fmtRunStatus(run.status)}</span>
                  <span class="run-src">{run.source === "schedule" ? "定时" : "手动"}</span>
                  <span class="run-time">{fmtLocal(run.started_at)}</span>
                  <span class="run-summary">{run.result_summary || run.error || ""}</span>
                </button>
              {/each}
            {/if}
          </div>
          {#if selRunId}
            <div class="run-detail card">
              {#if runMsgs.length === 0}
                <div class="empty">（无消息）</div>
              {:else}
                {#each runMsgs as msg}
                  <div class="msg {msg.role}">
                    <span class="msg-role">{msg.role}</span>
                    <div class="msg-content">{msg.content}</div>
                  </div>
                {/each}
              {/if}
            </div>
          {/if}
        </div>
      </div>
    {:else}
    <div class="toolbar">
      <button class="btn-primary" onclick={startNewAuto}>+ 新建自动化</button>
      {#if errorMsg}<span class="error">{errorMsg}</span>{/if}
    </div>

    {#if autoEditing}
      <div class="auto-form card">
        <h3>{autoForm.id ? "编辑自动化" : "新建自动化"}</h3>
        <label class="field">
          <span>名称</span>
          <input type="text" bind:value={autoForm.name} placeholder="每日总结代办" />
        </label>
        <label class="field">
          <span>智能体</span>
          <select bind:value={autoForm.agentId}>
            <option value="">（选择智能体）</option>
            {#each agents as a}
              <option value={a.id}>{a.name}（{a.provider}）</option>
            {/each}
          </select>
        </label>
        <label class="field">
          <span>提示词</span>
          <textarea
            bind:value={autoForm.prompt}
            rows="4"
            placeholder="总结我今天的代办并给三条建议"
          ></textarea>
        </label>
        <div class="field">
          <span>触发方式</span>
          <div class="trigger-options">
            <label
              class="trigger-option"
              class:sel={autoForm.triggerType === "daily"}
            >
              <input
                type="radio"
                name="autotrigger"
                bind:group={autoForm.triggerType}
                value="daily"
              />
              <span class="trigger-label">每天</span>
              {#if autoForm.triggerType === "daily"}
                <input type="time" bind:value={autoForm.dailyTime} />
              {:else}
                <span class="trigger-placeholder">选此项后设置时间</span>
              {/if}
            </label>
            <label
              class="trigger-option"
              class:sel={autoForm.triggerType === "interval"}
            >
              <input
                type="radio"
                name="autotrigger"
                bind:group={autoForm.triggerType}
                value="interval"
              />
              <span class="trigger-label">每</span>
              {#if autoForm.triggerType === "interval"}
                <input
                  type="number"
                  min="1"
                  bind:value={autoForm.intervalValue}
                  class="trigger-num"
                />
                <select bind:value={autoForm.intervalUnit}>
                  <option value="minutes">分钟</option>
                  <option value="hours">小时</option>
                </select>
              {:else}
                <span class="trigger-placeholder">选此项后设置间隔</span>
              {/if}
            </label>
          </div>
        </div>
        <label class="field inline">
          <input type="checkbox" bind:checked={autoForm.enabled} />
          启用定时（关闭则只在手动"立即运行"时跑）
        </label>
        <div class="form-actions">
          <button
            class="btn-primary"
            onclick={saveAuto}
            disabled={autoSaving || !autoForm.name || !autoForm.agentId}
          >
            {autoSaving ? "保存中…" : "保存"}
          </button>
          {#if autoForm.id}
            <button class="btn-danger" onclick={() => deleteAuto(autoForm.id!)}>
              删除
            </button>
          {/if}
          <button onclick={() => (autoEditing = false)}>取消</button>
        </div>
      </div>
    {/if}

    {#if automations.length === 0 && !autoEditing}
      <div class="empty">还没有自动化任务。点「+ 新建自动化」创建。</div>
    {:else if automations.length > 0}
      <div class="auto-list">
        {#each automations as auto (auto.id)}
          <div class="auto-row card" class:sel={selAuto === auto.id}>
            <div
              class="auto-row-main"
              role="button"
              tabindex="0"
              onclick={() => startEditAuto(auto)}
              onkeydown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  startEditAuto(auto);
                }
              }}
            >
              <div class="auto-name">
                {auto.name}
                {#if !auto.enabled}<span class="badge muted">停用</span>{/if}
              </div>
              <div class="auto-meta">{fmtTrigger(auto)}</div>
              <div class="auto-sub">
                上次: {fmtLocal(auto.last_run_at)} · 下次: {fmtLocal(
                  auto.next_fire_at,
                )}
              </div>
            </div>
            <div class="auto-row-actions">
              <button onclick={() => toggleAutoEnabled(auto)}>
                {auto.enabled ? "停用" : "启用"}
              </button>
              <button
                class="btn-primary"
                onclick={() => runNow(auto)}
                disabled={auto.has_running}
              >
                {auto.has_running ? "运行中…" : "立即运行"}
              </button>
              <button onclick={() => showHistory(auto)}>历史</button>
            </div>
          </div>
        {/each}
      </div>
    {/if}
    {/if}
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

        <!-- Supporting script files: a flat path-keyed list (subdirs via
             path strings, e.g. scripts/analyze.py). Saved as a full-replace
             on 保存. SKILL.md is reserved (body is the textarea above). -->
        <div class="skill-files">
          <div class="skill-files-head">
            <span class="skill-files-label">关联脚本</span>
            {#if formFileEditingIdx === null}
              <button class="act" onclick={startAddSkillFile}>+ 新增脚本</button>
            {/if}
          </div>

          {#if formFileEditingIdx !== null}
            <!-- add / edit one file -->
            <div class="skill-file-edit">
              <input
                type="text"
                bind:value={formFilePath}
                placeholder="相对路径，如 scripts/analyze.py（不可为 SKILL.md）"
                class="form-desc"
              />
              <textarea
                bind:value={formFileContent}
                placeholder="脚本内容"
                class="form-content"
                rows="6"
              ></textarea>
              <div class="form-actions">
                <button class="act cancel" onclick={cancelSkillFileEdit}>取消</button>
                <button class="act save" onclick={saveSkillFile} disabled={!formFilePath.trim()}>保存脚本</button>
              </div>
            </div>
          {:else}
            <!-- file list -->
            {#if formSkillFiles.length === 0}
              <div class="skill-files-empty">暂无脚本（技能可仅含 SKILL.md 正文）</div>
            {:else}
              {#each formSkillFiles as f, i (i)}
                <div class="skill-file-row">
                  <span class="skill-file-path">{f.path}</span>
                  <button class="act" onclick={() => startEditSkillFile(i)}>编辑</button>
                  <button class="act del" onclick={() => deleteSkillFile(i)}>删除</button>
                </div>
              {/each}
            {/if}
          {/if}
        </div>

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
    <div class="mcp-section">
      <div class="mcp-head">
        <span class="mcp-title">MCP 服务器</span>
        <div class="mcp-head-actions">
          <button class="mcp-mini-btn" onclick={addMcpEntry}>+ 添加</button>
          <button
            class="mcp-mini-btn"
            onclick={addTodoMcp}
            title="自动添加代办 MCP 服务器配置"
          >
            + 代办 MCP
          </button>
        </div>
      </div>
      {#if mcpEntries.length === 0}
        <div class="mcp-empty">尚未配置 MCP 服务器</div>
      {:else}
        <div class="mcp-list">
          {#each mcpEntries as entry, i (i)}
            <div class="mcp-entry">
              <div class="mcp-entry-row">
                <input
                  type="text"
                  class="mcp-name"
                  bind:value={entry.name}
                  placeholder="名称（如 my-server）"
                  onchange={syncMcpConfig}
                />
                <button
                  class="mcp-del"
                  onclick={() => removeMcpEntry(i)}
                  title="删除此 MCP 服务器"
                  aria-label="删除此 MCP 服务器"
                >
                  ×
                </button>
              </div>
              <textarea
                class="mcp-cfg"
                class:invalid={!entryConfigValid(entry.config)}
                bind:value={entry.config}
                placeholder={'JSON 配置，如 {"command":"npx","args":["-y","srv"],"env":{}}'}
                rows="3"
                onchange={syncMcpConfig}
              ></textarea>
              {#if !entryConfigValid(entry.config)}
                <div class="mcp-err">JSON 格式错误</div>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
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

<!-- Snippet: the AI generation conversation panel. Two phases:
     - Setup (genId === null): provider + model pickers + description
       textarea + 开始生成.
     - Conversation (genId !== null): streamed message list + draft
       preview card (when parseLatestDraft finds a <agent_draft>) +
       确认/继续修改/取消 buttons + a composer for sending modifications
       (the 继续修改 path). The model picker is shared with the create
       form's probe/listModels path. -->
{#snippet genPanel()}
  <div class="gen-panel">
    <div class="gen-header">
      <span class="gen-title">✨ AI 生成智能体</span>
      <button class="act" onclick={cancelGen} title="取消并关闭">✕</button>
    </div>

    {#if !genId}
      <!-- Setup phase: pick provider + model, describe the agent. -->
      <div class="gen-setup">
        <div class="form-row">
          <select bind:value={genProvider} onchange={onGenProviderChange} aria-label="Provider">
            {#each PROVIDERS as p (p)}
              <option value={p}>{PROVIDER_LABEL[p]}</option>
            {/each}
          </select>
          {#if genProbing}
            <span class="probing">探测模型中…</span>
          {:else if genManualModel || genModelList.length === 0}
            <input type="text" bind:value={genModel} placeholder="模型名（留空=默认）" class="model-input" />
          {:else}
            <select bind:value={genModel} class="model-select">
              <option value="">（默认模型）</option>
              {#each genModelList as m (m)}
                <option value={m}>{m}</option>
              {/each}
            </select>
          {/if}
        </div>
        <textarea
          bind:value={genDescription}
          placeholder="描述你想要的智能体，AI 帮你在对话中生成配置…"
          class="form-prompt gen-desc"
          rows="4"
        ></textarea>
        <div class="form-actions">
          <button class="act cancel" onclick={cancelGen}>取消</button>
          <button class="act save" onclick={beginGen} disabled={!genDescription.trim()}>开始生成</button>
        </div>
      </div>
    {:else}
      <!-- Conversation phase: messages + draft + buttons + composer. -->
      <div class="gen-messages">
        {#each genMessages as m, i (i)}
          <div class="bubble" data-role={m.role} data-kind={m.kind}>
            {#if m.role !== "user"}<span class="role">{m.role}</span>{/if}
            <div class="content">
              <pre class="gen-pre">{m.content}</pre>
            </div>
          </div>
        {/each}
        {#if genStreaming}
          <div class="gen-spinner">✦ 生成中…</div>
        {/if}
      </div>

      <!-- Draft preview + action buttons (shown when a <agent_draft>
           is parseable from the conversation). -->
      {#if genDraft}
        <div class="gen-draft">
          <div class="draft-head">📋 草稿预览</div>
          <div class="draft-row"><span class="draft-k">名称</span><b class="draft-v">{genDraft.name ?? "—"}</b></div>
          {#if genDraft.description}<div class="draft-row"><span class="draft-k">描述</span><span class="draft-v">{genDraft.description}</span></div>{/if}
          <div class="draft-row"><span class="draft-k">类型</span><span class="draft-v">{PROVIDER_LABEL[genProvider] ?? genProvider}</span></div>
          <div class="draft-row"><span class="draft-k">模型</span><span class="draft-v">{genModel || "（默认）"}</span></div>
        </div>
        <div class="gen-actions">
          <button class="act save" onclick={confirmGen} title="根据草稿创建智能体">✓ 确认创建</button>
          <button class="act" onclick={focusGenInput} title="在下方输入修改意见">✎ 继续修改</button>
          <button class="act del" onclick={cancelGen} title="放弃，不创建">✗ 取消</button>
        </div>
      {:else if !genStreaming}
        <div class="gen-hint">AI 未生成有效草稿，可在下方继续对话调整</div>
      {/if}

      <!-- Composer (the 继续修改 mechanism). Enter sends, Shift+Enter
           newlines. -->
      <div class="gen-composer">
        <textarea
          bind:this={genInputEl}
          bind:value={genModInput}
          onkeydown={onGenKeydown}
          placeholder="输入修改意见… (Enter 发送, Shift+Enter 换行)"
          rows="2"
        ></textarea>
        <button class="btn-primary" onclick={sendGenModification} disabled={genStreaming || !genModInput.trim() || !genId}>发送</button>
      </div>
    {/if}
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

  .btn-ghost {
    padding: var(--space-1) var(--space-3);
    font-size: 0.8rem;
    cursor: pointer;
    background: transparent;
    color: var(--mut);
    border: 1px solid var(--bd);
    border-radius: var(--r-chip);
    transition: all var(--t-fast);
    white-space: nowrap;
  }
  .btn-ghost:hover { color: var(--acc); border-color: var(--acc); background: var(--acc-soft); }
  .btn-ghost:disabled { color: var(--bd); border-color: var(--bd); cursor: not-allowed; }

  /* -- AI generation conversation panel -- */
  .gen-panel {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    background: var(--bg);
  }

  .gen-header {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding-bottom: var(--space-1);
    border-bottom: 1px solid var(--bd);
  }
  .gen-title { font-size: 0.85rem; font-weight: 600; color: var(--acc); flex: 1; }

  .gen-setup {
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-2);
    background: var(--surface);
    border: 1px solid var(--bd);
    border-radius: var(--r-bubble);
  }
  .gen-desc { min-height: 80px; }

  .gen-messages {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-2);
    background: var(--bg);
    border: 1px solid var(--bd);
    border-radius: var(--r-bubble);
  }

  /* Bubble: user = accent fill + right-aligned; assistant = surface
   * card + left-aligned. Mirrors ChatView's alignment so the
   * conversation reads naturally. */
  .bubble {
    max-width: 85%;
    padding: var(--space-1) var(--space-2);
    border-radius: var(--r-bubble);
    font-size: 0.78rem;
    line-height: 1.5;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .bubble[data-role="user"] {
    align-self: flex-end;
    background: var(--acc);
    color: #fff;
  }
  .bubble[data-role="assistant"] {
    align-self: flex-start;
    background: var(--surface);
    border: 1px solid var(--bd);
    color: var(--ink);
  }
  .bubble[data-role="system"],
  .bubble[data-role="tool"] {
    align-self: flex-start;
    background: var(--acc-soft);
    color: var(--mut);
    font-size: 0.72rem;
  }
  .bubble .role {
    font-size: 0.65rem;
    color: var(--mut);
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }
  .bubble[data-role="user"] .role { color: rgba(255,255,255,0.8); }
  .bubble .content { word-break: break-word; }
  .gen-pre {
    margin: 0;
    white-space: pre-wrap;
    word-break: break-word;
    font-family: var(--mono);
    font-size: 0.75rem;
  }

  .gen-spinner {
    align-self: flex-start;
    font-size: 0.72rem;
    color: var(--acc);
    font-style: italic;
    padding: var(--space-1) var(--space-2);
  }

  /* Draft preview card — shown when parseLatestDraft finds a
   * <agent_draft>. Summarizes what 确认 will create. */
  .gen-draft {
    flex-shrink: 0;
    background: var(--acc-soft);
    border: 1px solid var(--acc);
    border-radius: var(--r-bubble);
    padding: var(--space-2) var(--space-3);
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .draft-head { font-size: 0.75rem; font-weight: 600; color: var(--acc); margin-bottom: var(--space-1); }
  .draft-row { display: flex; gap: var(--space-2); font-size: 0.75rem; align-items: baseline; }
  .draft-k { flex-shrink: 0; color: var(--mut); min-width: 3rem; }
  .draft-v { color: var(--ink); word-break: break-word; }

  .gen-actions {
    flex-shrink: 0;
    display: flex;
    gap: var(--space-2);
    justify-content: center;
  }
  .gen-actions .act { padding: var(--space-1) var(--space-3); font-size: 0.78rem; }

  .gen-hint {
    flex-shrink: 0;
    text-align: center;
    font-size: 0.75rem;
    color: var(--mut);
    padding: var(--space-1);
  }

  .gen-composer {
    flex-shrink: 0;
    display: flex;
    gap: var(--space-2);
    align-items: flex-end;
  }
  .gen-composer textarea {
    flex: 1;
    font-size: 0.78rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--surface);
    color: var(--ink);
    resize: vertical;
    transition: border-color var(--t-fast);
  }
  .gen-composer textarea:focus { border-color: var(--acc); outline: none; }
  .gen-composer textarea::placeholder { color: var(--mut); }

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

  .form-prompt, .form-content {
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
  .form-prompt:focus, .form-content:focus {
    border-color: var(--acc);
    outline: none;
  }
  .form-prompt::placeholder, .form-content::placeholder {
    color: var(--mut);
  }

  /* -- skill supporting files (script manager) -- */
  .skill-files {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    padding: var(--space-2);
    border: 1px dashed var(--bd);
    border-radius: var(--r-code);
    background: var(--bg);
  }

  .skill-files-head {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .skill-files-label {
    flex: 1;
    font-size: 0.75rem;
    color: var(--mut);
    font-weight: 500;
  }

  .skill-files-empty {
    font-size: 0.72rem;
    color: var(--mut);
    font-style: italic;
    padding: var(--space-1) 0;
  }

  .skill-file-row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-1) var(--space-2);
    background: var(--surface);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
  }

  .skill-file-path {
    flex: 1;
    min-width: 0;
    font-size: 0.75rem;
    font-family: var(--mono);
    color: var(--ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .skill-file-edit {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-2);
    background: var(--surface);
    border: 1px solid var(--acc);
    border-radius: var(--r-code);
  }

  .mcp-section {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    padding: var(--space-2);
    background: var(--surface);
  }

  .mcp-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }

  .mcp-title {
    font-size: 0.72rem;
    font-weight: 600;
    color: var(--mut);
  }

  .mcp-head-actions {
    display: flex;
    gap: var(--space-1);
  }

  .mcp-empty {
    font-size: 0.72rem;
    color: var(--mut);
    padding: var(--space-1) 0;
  }

  .mcp-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .mcp-entry {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    padding: var(--space-2);
    background: var(--bg);
  }

  .mcp-entry-row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .mcp-name {
    flex: 1;
    font-size: 0.78rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    background: var(--bg);
    color: var(--ink);
  }

  .mcp-name:focus {
    border-color: var(--acc);
    outline: none;
  }

  .mcp-cfg {
    font-family: var(--mono);
    font-size: 0.72rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    background: var(--bg);
    color: var(--ink);
    resize: vertical;
    width: 100%;
    box-sizing: border-box;
  }

  .mcp-cfg:focus {
    border-color: var(--acc);
    outline: none;
  }

  .mcp-cfg.invalid {
    border-color: var(--err);
  }

  .mcp-err {
    font-size: 0.68rem;
    color: var(--err);
  }

  .mcp-del {
    flex-shrink: 0;
    width: 22px;
    height: 22px;
    line-height: 1;
    padding: 0;
    cursor: pointer;
    background: transparent;
    color: var(--mut);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    transition: all var(--t-fast);
  }

  .mcp-del:hover {
    color: var(--err);
    border-color: var(--err);
  }

  .mcp-mini-btn {
    font-size: 0.68rem;
    padding: var(--space-1) var(--space-2);
    cursor: pointer;
    background: var(--acc-soft);
    color: var(--acc);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    transition: all var(--t-fast);
    white-space: nowrap;
  }

  .mcp-mini-btn:hover {
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

  /* -- automation sub-tab -- */
  .card {
    background: var(--surface);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
  }
  .btn-danger {
    padding: var(--space-1) var(--space-3);
    font-size: 0.8rem;
    cursor: pointer;
    background: transparent;
    color: var(--err);
    border: 1px solid var(--err);
    border-radius: var(--r-chip);
    transition: background var(--t-fast), color var(--t-fast);
  }
  .btn-danger:hover {
    background: var(--err);
    color: #fff;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    font-size: 0.8rem;
  }
  .field.inline {
    flex-direction: row;
    align-items: center;
    gap: var(--space-2);
  }
  .field input,
  .field select,
  .field textarea {
    font-size: 0.8rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--bg);
    color: var(--ink);
  }
  .field textarea {
    resize: vertical;
  }
  .trigger-options {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }
  .trigger-option {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    cursor: pointer;
    font-size: 0.8rem;
    transition: border-color var(--t-fast), background var(--t-fast);
  }
  .trigger-option:hover {
    border-color: var(--acc);
  }
  .trigger-option.sel {
    border-color: var(--acc);
    background: var(--bg);
  }
  .trigger-label {
    min-width: 2.5em;
    color: var(--ink);
  }
  .trigger-num {
    width: 5em;
  }
  .trigger-placeholder {
    color: var(--mut);
    font-size: 0.72rem;
  }
  .badge {
    font-size: 0.65rem;
    padding: 0 var(--space-1);
    border-radius: var(--r-chip);
    background: var(--acc);
    color: #fff;
    margin-left: var(--space-1);
  }
  .badge.muted {
    background: var(--mut);
  }

  .auto-form {
    padding: var(--space-3);
    margin: var(--space-2) var(--space-3);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }
  .auto-form h3 {
    margin: 0 0 var(--space-1);
    font-size: 0.95rem;
  }
  .form-actions {
    display: flex;
    gap: var(--space-2);
    align-items: center;
  }

  .auto-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
  }
  .auto-row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
  }
  .auto-row.sel {
    border-color: var(--acc);
  }
  .auto-row-main {
    flex: 1;
    cursor: pointer;
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .auto-name {
    font-size: 0.85rem;
    font-weight: 600;
    display: flex;
    align-items: center;
  }
  .auto-meta {
    font-size: 0.75rem;
    color: var(--acc);
  }
  .auto-sub {
    font-size: 0.7rem;
    color: var(--mut);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .auto-row-actions {
    display: flex;
    gap: var(--space-1);
    flex-shrink: 0;
  }
  .auto-row-actions button {
    padding: var(--space-1) var(--space-2);
    font-size: 0.75rem;
    cursor: pointer;
    background: transparent;
    color: var(--ink);
    border: 1px solid var(--bd);
    border-radius: var(--r-chip);
  }
  .auto-row-actions .btn-primary {
    background: var(--acc);
    color: #fff;
    border: none;
  }

  /* -- automation run history + detail (Task 10) -- */
  .auto-history {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
  }
  .auto-history-title {
    font-size: 0.85rem;
    font-weight: 600;
    color: var(--ink);
  }
  .auto-history-body {
    flex: 1;
    overflow: auto;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
  }
  .runs-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }
  .run-row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-1) var(--space-2);
    background: var(--surface);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    cursor: pointer;
    text-align: left;
    font-size: 0.78rem;
    width: 100%;
  }
  .run-row:hover {
    border-color: var(--acc);
  }
  .run-row.sel {
    border-color: var(--acc);
    background: var(--bg);
  }
  .run-status {
    font-size: 0.7rem;
    padding: 0 var(--space-1);
    border-radius: var(--r-chip);
    color: #fff;
    flex-shrink: 0;
  }
  .run-status.running {
    background: #e90;
  }
  .run-status.completed {
    background: #3a3;
  }
  .run-status.failed {
    background: var(--err);
  }
  .run-src {
    color: var(--mut);
    flex-shrink: 0;
  }
  .run-time {
    color: var(--ink);
    flex-shrink: 0;
  }
  .run-summary {
    color: var(--mut);
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .run-detail {
    padding: var(--space-2) var(--space-3);
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    max-height: 60vh;
    overflow: auto;
  }
  .msg {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: var(--space-1) var(--space-2);
    border-left: 2px solid var(--bd);
  }
  .msg.user {
    border-left-color: var(--acc);
  }
  .msg.assistant {
    border-left-color: #3a3;
  }
  .msg-role {
    font-size: 0.65rem;
    color: var(--mut);
    text-transform: uppercase;
  }
  .msg-content {
    font-size: 0.8rem;
    color: var(--ink);
    white-space: pre-wrap;
    word-break: break-word;
  }

  /* -- live progress panel (Task 10 review fix #4) -- */
  .live-panel {
    padding: var(--space-2) var(--space-3);
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    border-color: #e90;
    max-height: 40vh;
    overflow: auto;
  }
  .live-head {
    font-size: 0.75rem;
    color: #e90;
    font-weight: 600;
  }
  .msg.live {
    border-left-color: #e90;
  }
  .msg-kind {
    font-size: 0.65rem;
    color: var(--mut);
    text-transform: uppercase;
  }
</style>
