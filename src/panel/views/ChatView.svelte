<!-- Task 5: the 对话 (chat) tab — the main panel view. Left sidebar =
     conversation list + a "新建对话" button that opens a modal dialog
     (provider select → model probe → cwd input → confirm →
     new_conversation(provider, model, cwd)); right pane = message
     bubbles (load_history on select + live-streamed chat_message
     events) + composer. On send → chat_send(conv_id, prompt) → backend
     spawns the stream-drain task + returns (Task 5 carry: NO `provider`
     arg — the conversation's agent is fixed at creation; the backend
     uses `conv.provider`); chat_message / chat_result events arrive via
     stream.ts (dispatched by conv_id).

     Review Focus #3 (survives-hide): if the panel is hidden
     mid-stream, the emit closures are no-ops — but the backend keeps
     running + persists the full stream + result via save_turn_record.
     Re-selecting the conversation calls load_history, which returns
     the persisted stream. The messages are NOT lost. -->
<script lang="ts">
  import {
    listConversations,
    newConversation,
    loadHistory,
    chatSend,
    listModels,
    type Conversation,
    type StoredMessage,
    type Message,
    type MessageKind,
    type ExecResult,
    type Provider,
  } from "../lib/api";
  import { getSettings, defaultModelFor } from "../lib/settings";
  import { subscribeChatStream } from "../lib/stream";

  // Reactive state. Svelte 5 runes — typed via the variable annotation
  // (NOT `$state<T>(...)`, which svelte-check mis-parses as a generic
  // call on a global `$state`). The variable-annotation pattern is the
  // canonical Svelte 5 form anyway.
  let conversations: Conversation[] = $state([]);
  let activeConvId: string | null = $state(null);
  let messages: DisplayMessage[] = $state([]);
  let draft: string = $state("");
  let streamingConvIds: Set<string> = $state(new Set());
  let errorMsg: string | null = $state(null);

  // Per-conversation live message buffer. The active conversation's
  // buffer is mirrored to `messages` for rendering; non-active
  // conversations' buffers are kept here so a user who switches tabs
  // mid-stream + back sees the messages that arrived while away (in
  // addition to load_history's persisted view, which may lag the live
  // stream by one save_turn_record call).
  const liveBuffers: Map<string, DisplayMessage[]> = new Map();

  // Display shape: StoredMessage (role + content) PLUS the live
  // MessageKind (for visual distinction — thinking / tool-use / error
  // all render differently even though they all persist as role
  // "assistant"). `kind` is undefined for messages loaded from
  // history (we didn't persist it); we infer a kind from the role for
  // rendering (assistant → Text, tool → ToolResult, system → Status).
  interface DisplayMessage {
    role: StoredMessage["role"];
    content: string;
    kind?: MessageKind;
  }

  const PROVIDERS: Provider[] = ["claude", "codex", "opencode"];

  const PROVIDER_LABEL: Record<Provider, string> = {
    claude: "Claude",
    codex: "Codex",
    opencode: "OpenCode",
  };

  // -- new-conversation dialog state --
  // The dialog sets provider + model + cwd at creation (immutable
  // thereafter — design "改动 1"). `model` comes from `listModels(p)`
  // (probe) OR a manual text input (fallback if the probe list is
  // empty). `cwd` pre-fills from the global config's `working_dir`.
  let showDialog: boolean = $state(false);
  let dialogProvider: Provider = $state("claude");
  let dialogModel: string = $state("");
  let dialogCwd: string = $state("");
  let modelList: string[] = $state([]);
  let probing: boolean = $state(false);
  let probeError: string | null = $state(null);
  let manualModel: boolean = $state(false);
  let creating: boolean = $state(false);

  // Map a streamed MessageKind to a StoredMessage role — mirrors
  // db.rs::message_kind_to_role so the live buffer + the persisted
  // history agree on role assignment (the persisted shape drops the
  // kind tag; the live buffer keeps it for richer rendering).
  function kindToRole(kind: MessageKind): StoredMessage["role"] {
    switch (kind) {
      case "Text":
      case "Thinking":
      case "ToolUse":
      case "Error":
        return "assistant";
      case "ToolResult":
        return "tool";
      case "Status":
      case "Log":
        return "system";
    }
  }

  function roleToKind(role: StoredMessage["role"]): MessageKind {
    switch (role) {
      case "assistant":
        return "Text";
      case "tool":
        return "ToolResult";
      case "system":
        return "Status";
      case "user":
        return "Text";
    }
  }

  // -- load + select --

  async function refreshConversations() {
    try {
      conversations = await listConversations();
      // If the active conversation was deleted (future Task 16), fall
      // back to the first. If there's none selected but the list is
      // non-empty, select the first.
      if (activeConvId && !conversations.some((c) => c.id === activeConvId)) {
        activeConvId = null;
      }
      if (!activeConvId && conversations.length > 0) {
        await selectConversation(conversations[0].id);
      }
    } catch (e) {
      errorMsg = `加载会话列表失败: ${e}`;
    }
  }

  async function selectConversation(convId: string) {
    activeConvId = convId;
    streamingConvIds = new Set(streamingConvIds); // touch for reactivity
    try {
      const history = await loadHistory(convId);
      // Merge the persisted history with any live messages that
      // arrived since the last save_turn_record (the live buffer may
      // have messages the db hasn't persisted yet — keep them, deduped
      // by content+role+order). For v1 simplicity, prefer the live
      // buffer if it's longer (the live stream is ahead of the
      // persisted view by one save_turn_record call); else use history.
      const live = liveBuffers.get(convId) ?? [];
      if (live.length >= history.length) {
        messages = [...live];
      } else {
        // History is longer (the conversation was re-selected after a
        // save_turn_record) — render history, mapping roles→kinds for
        // display. The live buffer is stale; reset it.
        messages = history.map((m) => ({
          role: m.role,
          content: m.content,
          kind: roleToKind(m.role),
        }));
        liveBuffers.set(convId, [...messages]);
      }
    } catch (e) {
      errorMsg = `加载历史失败: ${e}`;
    }
  }

  // -- new-conversation dialog --

  /** Open the dialog. Pre-fill provider from the global default_provider,
   * cwd from the global working_dir, model from the per-agent
   * default_model (SettingsView). Reset probe state. */
  async function openDialog() {
    showDialog = true;
    probing = false;
    probeError = null;
    manualModel = false;
    modelList = [];
    dialogModel = "";
    creating = false;
    try {
      const s = await getSettings();
      dialogProvider = (s.default_provider as Provider) || "claude";
      dialogCwd = s.working_dir ?? "";
      // Pre-fill model from the per-agent default (if set).
      const dm = defaultModelFor(s, dialogProvider);
      dialogModel = dm ?? "";
      // Kick off the probe for the pre-filled provider so the model
      // list is ready by the time the user looks at the dropdown.
      void probeModels(dialogProvider);
    } catch (e) {
      // Settings load failed — the dialog still works with defaults.
      probeError = `加载设置失败: ${e}`;
    }
  }

  /** Probe models for `provider` via `listModels(provider)`. On empty
   * result → switch to manual text entry (the design's 兜底). On non-
   * empty → populate the dropdown; if the per-agent default isn't in
   * the list, keep the pre-filled model but it won't be selectable
   * from the dropdown (the user can switch to manual entry). */
  async function probeModels(provider: Provider) {
    probing = true;
    probeError = null;
    manualModel = false;
    modelList = [];
    try {
      const list = await listModels(provider);
      modelList = list;
      if (list.length === 0) {
        // Probe failed / empty → manual text entry. Keep dialogModel
        // (the per-agent default pre-fill, if any) so the user sees
        // what was configured + can edit it.
        manualModel = true;
      } else if (dialogModel && !list.includes(dialogModel)) {
        // The pre-filled default isn't in the probed list — switch to
        // manual entry so the user can edit/keep the pre-fill rather
        // than silently dropping it.
        manualModel = true;
      } else if (!dialogModel) {
        // No pre-fill + non-empty list → pick the first as a
        // convenience default (the user can change it).
        dialogModel = list[0];
      }
    } catch (e) {
      probeError = `探测模型失败: ${e}`;
      manualModel = true;
    } finally {
      probing = false;
    }
  }

  /** Re-probe when the user changes the provider in the dialog. Also
   * re-pull the per-agent default model for the new provider (the
   * user may have configured a default for claude but not codex). */
  async function onDialogProviderChange() {
    dialogModel = "";
    try {
      const s = await getSettings();
      const dm = defaultModelFor(s, dialogProvider);
      dialogModel = dm ?? "";
    } catch {
      // ignore — settings load is best-effort here
    }
    await probeModels(dialogProvider);
  }

  /** Toggle manual entry (lets the user type a model name even when the
   * probe returned a non-empty list — e.g. for a model not yet
   * advertised by the CLI). */
  function toggleManual() {
    manualModel = !manualModel;
  }

  /** Confirm the dialog → create the conversation with the chosen
   * provider + model + cwd. model is null when the field is empty
   * (→ the agent's own default). */
  async function confirmDialog() {
    creating = true;
    try {
      const model = dialogModel.trim() || null;
      const conv = await newConversation(dialogProvider, model, dialogCwd);
      conversations = [conv, ...conversations];
      liveBuffers.set(conv.id, []);
      await selectConversation(conv.id);
      showDialog = false;
      errorMsg = null;
    } catch (e) {
      errorMsg = `新建会话失败: ${e}`;
    } finally {
      creating = false;
    }
  }

  function cancelDialog() {
    showDialog = false;
  }

  // -- send --

  async function send() {
    const prompt = draft.trim();
    if (!prompt || !activeConvId) return;
    const convId = activeConvId;
    // Optimistic: append the user's prompt to the local buffer NOW
    // (immediate feedback). The backend also persists it in
    // run_chat_turn step 1; on next full load_history, the local
    // buffer is replaced (cleared + reloaded), so no duplication.
    const userMsg: DisplayMessage = {
      role: "user",
      content: prompt,
      kind: "Text",
    };
    messages = [...messages, userMsg];
    liveBuffers.set(convId, [...(liveBuffers.get(convId) ?? []), userMsg]);
    draft = "";
    // Mark the conversation as streaming — disables the composer +
    // shows a spinner. Cleared on chat_result.
    const next = new Set(streamingConvIds);
    next.add(convId);
    streamingConvIds = next;
    try {
      // Task 5 carry: NO `provider` arg — the backend uses
      // conv.provider (fixed at creation, immutable).
      await chatSend(convId, prompt);
      errorMsg = null;
    } catch (e) {
      errorMsg = `发送失败: ${e}`;
      const cleared = new Set(streamingConvIds);
      cleared.delete(convId);
      streamingConvIds = cleared;
    }
  }

  // -- stream subscription (mounted once) --

  function handleStreamMessage(convId: string, message: Message) {
    const dm: DisplayMessage = {
      role: kindToRole(message.kind),
      content: message.content,
      kind: message.kind,
    };
    // Append to the conv's live buffer.
    const buf = liveBuffers.get(convId) ?? [];
    buf.push(dm);
    liveBuffers.set(convId, buf);
    // If it's the active conversation, mirror to the rendered buffer.
    if (convId === activeConvId) {
      messages = [...buf];
    }
  }

  function handleStreamResult(convId: string, result: ExecResult) {
    const cleared = new Set(streamingConvIds);
    cleared.delete(convId);
    streamingConvIds = cleared;
    // Surface a failed / non-completed `ExecResult` (CLI not on PATH,
    // 403 quota, crash, timeout → status "failed"/"timeout"/etc.).
    // Without this the spinner stops with NO message — an invisible
    // failure (final-review fix #2). The streamed messages (if any)
    // already arrived as `chat_message` bubbles; this adds a visible
    // failure bubble inline + sets the banner. The error bubble is
    // frontend-only (not persisted) — on reload `load_history` shows
    // whatever the backend actually saved.
    if (result.status && result.status !== "completed") {
      const detail =
        result.output && result.output.trim().length > 0
          ? result.output.trim()
          : "本轮失败";
      errorMsg = `本轮出错: ${detail}`;
      const errBubble: DisplayMessage = {
        role: "assistant",
        content: `本轮出错: ${detail}`,
        kind: "Error",
      };
      const buf = liveBuffers.get(convId) ?? [];
      buf.push(errBubble);
      liveBuffers.set(convId, buf);
      if (convId === activeConvId) {
        messages = [...buf];
      }
    }
  }

  // `chat_error` — a `save_turn_record` failure surfaced from the
  // backend (disk full / FS error / db lock poisoned). The turn DID
  // complete in-memory (`chat_result` still fires + clears the
  // spinner), but the history won't survive a restart. Warn the user
  // via the banner + an inline system bubble. Final-review fix #1:
  // the frontend learns the save failed instead of the turn silently
  // vanishing on restart + the next turn replaying instead of resuming.
  function handleStreamError(convId: string, message: string) {
    errorMsg = message;
    const errBubble: DisplayMessage = {
      role: "system",
      content: message,
      kind: "Status",
    };
    const buf = liveBuffers.get(convId) ?? [];
    buf.push(errBubble);
    liveBuffers.set(convId, buf);
    if (convId === activeConvId) {
      messages = [...buf];
    }
  }

  $effect(() => {
    // Initial load.
    void refreshConversations();
    // Subscribe to the stream for the panel's lifetime.
    let unlisten: (() => void) | null = null;
    (async () => {
      unlisten = await subscribeChatStream({
        onMessage: handleStreamMessage,
        onResult: handleStreamResult,
        onError: handleStreamError,
      });
    })();
    return () => {
      unlisten?.();
    };
  });

  // Keyboard: Ctrl/Cmd+Enter sends.
  function onKeydown(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
      e.preventDefault();
      void send();
    }
  }

  // Derived: is the active conversation currently streaming?
  let isStreaming = $derived(
    activeConvId !== null && streamingConvIds.has(activeConvId),
  );

  // The active conversation object (for the read-only provider/model/cwd
  // header — the conversation's agent triple is fixed at creation, the
  // UI does NOT offer changing them mid-conversation).
  let activeConv = $derived(
    conversations.find((c) => c.id === activeConvId) ?? null,
  );

  // Auto-scroll the message list to the bottom on new messages.
  let scrollEl: HTMLDivElement | null = $state(null);
  $effect(() => {
    // Touch messages.length so the effect re-runs when messages change.
    void messages.length;
    if (scrollEl) {
      scrollEl.scrollTop = scrollEl.scrollHeight;
    }
  });
</script>

<section class="chat">
  <!-- Sidebar: conversation list + 新建对话 button (opens the dialog). -->
  <aside class="sidebar">
    <div class="new-row">
      <button onclick={openDialog}>新建对话</button>
    </div>
    <ul class="conv-list">
      {#each conversations as conv (conv.id)}
        <li>
          <button
            class="conv-row"
            class:active={conv.id === activeConvId}
            onclick={() => selectConversation(conv.id)}
          >
            <span class="conv-provider">{PROVIDER_LABEL[conv.provider as Provider] ?? conv.provider}</span>
            <span class="conv-title">{conv.title}</span>
            {#if conv.model}<span class="conv-model">{conv.model}</span>{/if}
          </button>
        </li>
      {/each}
      {#if conversations.length === 0}
        <li class="empty">暂无会话</li>
      {/if}
    </ul>
  </aside>

  <!-- Main pane: messages + composer. -->
  <div class="main">
    {#if activeConv === null}
      <div class="no-conv">
        <p>选择左侧会话或点击「新建对话」开始对话</p>
      </div>
    {:else}
      <!-- Read-only header: the conversation's fixed provider + model +
           cwd. No per-send picker (the agent is immutable at creation). -->
      <div class="main-header">
        <span class="hdr-chip">{PROVIDER_LABEL[activeConv.provider as Provider] ?? activeConv.provider}</span>
        {#if activeConv.model}
          <span class="hdr-chip model">{activeConv.model}</span>
        {:else}
          <span class="hdr-chip model muted">默认模型</span>
        {/if}
        <span class="hdr-chip cwd" title={activeConv.cwd}>{activeConv.cwd}</span>
        {#if isStreaming}<span class="streaming">生成中…</span>{/if}
      </div>
      <div class="messages" bind:this={scrollEl}>
        {#each messages as m (m)}
          <div class="bubble" data-role={m.role} data-kind={m.kind}>
            <span class="role">{m.role}</span>
            <span class="content">{m.content}</span>
          </div>
        {/each}
      </div>
      <div class="composer">
        <textarea
          bind:value={draft}
          onkeydown={onKeydown}
          placeholder="输入消息… (Ctrl+Enter 发送)"
          rows="3"
        ></textarea>
        <button onclick={send} disabled={!draft.trim() || isStreaming}>
          发送
        </button>
      </div>
    {/if}
    {#if errorMsg}<div class="error">{errorMsg}</div>{/if}
  </div>
</section>

<!-- New-conversation dialog (Task 5). Provider select → listModels probe →
     model dropdown (or manual text input fallback) + cwd input → confirm
     → newConversation(provider, model, cwd). -->
{#if showDialog}
  <div class="dialog-backdrop" role="dialog" aria-modal="true" aria-label="新建对话">
    <div class="dialog">
      <h2>新建对话</h2>
      <div class="dialog-field">
        <label for="dlg-provider">Agent</label>
        <select
          id="dlg-provider"
          bind:value={dialogProvider}
          onchange={onDialogProviderChange}
          disabled={probing || creating}
        >
          {#each PROVIDERS as p (p)}
            <option value={p}>{PROVIDER_LABEL[p]}</option>
          {/each}
        </select>
      </div>
      <div class="dialog-field">
        <label for="dlg-model">模型</label>
        {#if probing}
          <div class="model-probing">探测中…</div>
        {:else if manualModel || modelList.length === 0}
          <input
            id="dlg-model"
            type="text"
            bind:value={dialogModel}
            placeholder="手动输入模型名 (留空 = 用 agent 默认模型)"
            disabled={creating}
          />
          {#if modelList.length === 0}
            <small class="muted">未探测到模型列表，请手动填写</small>
          {:else}
            <small class="muted">已切换为手动输入</small>
          {/if}
          {#if modelList.length > 0}
            <button class="link-btn" onclick={toggleManual} disabled={creating}>
              改用下拉选择
            </button>
          {/if}
        {:else}
          <select
            id="dlg-model"
            bind:value={dialogModel}
            disabled={creating}
          >
            {#each modelList as m (m)}
              <option value={m}>{m}</option>
            {/each}
          </select>
          <button class="link-btn" onclick={toggleManual} disabled={creating}>
            手动输入
          </button>
        {/if}
      </div>
      <div class="dialog-field">
        <label for="dlg-cwd">工作目录</label>
        <input
          id="dlg-cwd"
          type="text"
          bind:value={dialogCwd}
          placeholder="留空 = 用全局工作目录"
          disabled={creating}
        />
        <small class="muted">agent 子进程的 cwd。留空 = 用全局工作目录。</small>
      </div>
      {#if probeError}<div class="dialog-error">{probeError}</div>{/if}
      <div class="dialog-actions">
        <button onclick={confirmDialog} disabled={probing || creating}>
          {creating ? "创建中…" : "确定"}
        </button>
        <button class="secondary" onclick={cancelDialog} disabled={creating}>
          取消
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .chat {
    display: flex;
    height: 100%;
    min-height: 0;
  }

  .sidebar {
    width: 200px;
    flex: 0 0 200px;
    border-right: 1px solid #cbd5e1;
    background: #f1f5f9;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .new-row {
    display: flex;
    gap: 4px;
    padding: 6px;
    border-bottom: 1px solid #cbd5e1;
  }

  .new-row button {
    flex: 1;
    font-size: 0.85rem;
    padding: 4px 8px;
    cursor: pointer;
    background: #2563eb;
    color: white;
    border: none;
    border-radius: 4px;
  }

  .new-row button:disabled {
    background: #94a3b8;
    cursor: not-allowed;
  }

  .conv-list {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow-y: auto;
    flex: 1;
  }

  .conv-list li {
    margin: 0;
    padding: 0;
  }

  .conv-row {
    width: 100%;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    padding: 6px 10px;
    background: transparent;
    border: none;
    border-bottom: 1px solid #e2e8f0;
    cursor: pointer;
    text-align: left;
  }

  .conv-row:hover {
    background: #e2e8f0;
  }

  .conv-row.active {
    background: #dbeafe;
  }

  .conv-provider {
    font-size: 0.65rem;
    color: #64748b;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .conv-title {
    font-size: 0.85rem;
    color: #1f2937;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    width: 100%;
  }

  .conv-model {
    font-size: 0.65rem;
    color: #475569;
    font-family: ui-monospace, "Cascadia Code", monospace;
  }

  .empty {
    padding: 1rem;
    text-align: center;
    color: #64748b;
    font-size: 0.85rem;
  }

  .main {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
    overflow: hidden;
  }

  .no-conv {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #64748b;
    font-size: 0.9rem;
  }

  .main-header {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 10px;
    border-bottom: 1px solid #cbd5e1;
    background: #f8fafc;
    flex-wrap: wrap;
  }

  .hdr-chip {
    font-size: 0.72rem;
    padding: 2px 8px;
    border-radius: 10px;
    background: #e2e8f0;
    color: #334155;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .hdr-chip.model {
    background: #ddd6fe;
    color: #5b21b6;
    font-family: ui-monospace, "Cascadia Code", monospace;
  }

  .hdr-chip.model.muted {
    background: #e2e8f0;
    color: #94a3b8;
    font-style: italic;
  }

  .hdr-chip.cwd {
    background: #dcfce7;
    color: #166534;
    font-family: ui-monospace, "Cascadia Code", monospace;
  }

  .streaming {
    font-size: 0.75rem;
    color: #2563eb;
    font-style: italic;
    margin-left: auto;
  }

  .messages {
    flex: 1;
    overflow-y: auto;
    padding: 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    background: #f8fafc;
  }

  .bubble {
    max-width: 80%;
    padding: 6px 10px;
    border-radius: 8px;
    font-size: 0.85rem;
    line-height: 1.4;
    word-break: break-word;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .bubble .role {
    font-size: 0.65rem;
    text-transform: uppercase;
    color: #64748b;
    letter-spacing: 0.05em;
  }

  .bubble[data-role="user"] {
    align-self: flex-end;
    background: #dbeafe;
    color: #1e3a8a;
  }

  .bubble[data-role="user"] .role {
    color: #1e40af;
  }

  .bubble[data-role="assistant"] {
    align-self: flex-start;
    background: #e2e8f0;
    color: #1f2937;
  }

  .bubble[data-role="tool"] {
    align-self: flex-start;
    background: #f1f5f9;
    color: #475569;
    font-family: ui-monospace, "Cascadia Code", monospace;
    font-size: 0.78rem;
    border: 1px solid #cbd5e1;
  }

  .bubble[data-role="system"] {
    align-self: center;
    background: transparent;
    color: #64748b;
    font-style: italic;
    font-size: 0.78rem;
  }

  .bubble[data-kind="Error"] {
    background: #fee2e2;
    color: #991b1b;
  }

  .bubble[data-kind="Thinking"] {
    background: #f5f3ff;
    color: #5b21b6;
    font-style: italic;
  }

  .composer {
    display: flex;
    gap: 6px;
    padding: 8px;
    border-top: 1px solid #cbd5e1;
    background: #f1f5f9;
  }

  .composer textarea {
    flex: 1;
    resize: vertical;
    font-family: inherit;
    font-size: 0.85rem;
    padding: 6px;
    border: 1px solid #cbd5e1;
    border-radius: 4px;
  }

  .composer button {
    padding: 6px 14px;
    font-size: 0.85rem;
    cursor: pointer;
    background: #2563eb;
    color: white;
    border: none;
    border-radius: 4px;
  }

  .composer button:disabled {
    background: #94a3b8;
    cursor: not-allowed;
  }

  .error {
    padding: 6px 10px;
    background: #fee2e2;
    color: #991b1b;
    font-size: 0.8rem;
    border-top: 1px solid #fecaca;
  }

  /* -- new-conversation dialog -- */
  .dialog-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }

  .dialog {
    background: white;
    border-radius: 8px;
    padding: 16px;
    width: 420px;
    max-width: 90vw;
    max-height: 90vh;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 12px;
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.3);
  }

  .dialog h2 {
    margin: 0;
    font-size: 1rem;
    color: #1f2937;
  }

  .dialog-field {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .dialog-field label {
    font-size: 0.82rem;
    color: #334155;
    font-weight: 500;
  }

  .dialog-field select,
  .dialog-field input[type="text"] {
    font-size: 0.85rem;
    padding: 4px 8px;
    border: 1px solid #cbd5e1;
    border-radius: 4px;
    font-family: inherit;
    background: white;
    color: #1f2937;
  }

  .dialog-field .model-probing {
    font-size: 0.85rem;
    color: #2563eb;
    font-style: italic;
    padding: 4px 8px;
  }

  .dialog-field small.muted {
    font-size: 0.7rem;
    color: #94a3b8;
  }

  .link-btn {
    align-self: flex-start;
    background: transparent;
    border: none;
    color: #2563eb;
    font-size: 0.75rem;
    cursor: pointer;
    padding: 2px 0;
    text-decoration: underline;
  }

  .link-btn:disabled {
    color: #94a3b8;
    cursor: not-allowed;
  }

  .dialog-error {
    font-size: 0.8rem;
    color: #991b1b;
    background: #fee2e2;
    padding: 4px 8px;
    border-radius: 4px;
  }

  .dialog-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 4px;
  }

  .dialog-actions button {
    padding: 6px 16px;
    font-size: 0.88rem;
    cursor: pointer;
    background: #2563eb;
    color: white;
    border: none;
    border-radius: 4px;
  }

  .dialog-actions button:disabled {
    background: #94a3b8;
    cursor: not-allowed;
  }

  .dialog-actions button.secondary {
    background: #e2e8f0;
    color: #1f2937;
  }
</style>
