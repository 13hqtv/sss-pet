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
    type Agent,
    listAgents,
  } from "../lib/api";
  import { getSettings, defaultModelFor } from "../lib/settings";
  import { subscribeChatStream } from "../lib/stream";
  import { marked } from "marked";
  import DOMPurify from "dompurify";

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

  // Display shape: StoredMessage (role + content + persisted kind +
  // persisted tool) PLUS the live MessageKind + the streamed `tool`
  // metadata (for the ToolUse "使用工具 X" label). `kind` is undefined
  // for messages loaded from pre-Task-1 history without a persisted
  // kind (the history-load path falls back to roleToKind); live
  // messages always carry the backend's MessageKind. `tool` is set on
  // live ToolUse messages AND on history ToolUse rows (Task 6
  // fix-round-1 persisted it); null for pre-fix rows + non-tool kinds
  // → the chip shows "（未知）".
  interface DisplayMessage {
    role: StoredMessage["role"];
    content: string;
    kind?: MessageKind;
    tool?: string | null;
    timestamp?: string | null;
  }

  // Format the current local time as YYYY-MM-DD HH:MM:SS (the format the
  // user requested: 2026-10-22 15:45:32). Used for live-streamed messages
  // (user sends + agent responds) — the timestamp is set when the message
  // arrives at the frontend, which is close enough to when it was actually
  // sent/received for display purposes.
  function nowTimestamp(): string {
    const d = new Date();
    const pad = (n: number) => n.toString().padStart(2, "0");
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  }

  // Convert an RFC3339 UTC timestamp (from the db) to local YYYY-MM-DD
  // HH:MM:SS for display. Returns null if the input is missing or
  // unparseable (the history row predates the created_at column, or the
  // stored value is malformed).
  function formatTimestamp(rfc3339: string | null | undefined): string | null {
    if (!rfc3339) return null;
    const d = new Date(rfc3339);
    if (isNaN(d.getTime())) return null;
    const pad = (n: number) => n.toString().padStart(2, "0");
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
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
  let dialogAgentId: string | null = $state(null);
  let agents: Agent[] = $state([]);
  let modelList: string[] = $state([]);
  let probing: boolean = $state(false);
  let probeError: string | null = $state(null);
  let manualModel: boolean = $state(false);
  let creating: boolean = $state(false);

  // Generation counter guarding the provider-switch probe race
  // (Fix round 1 #2). `openDialog()` fires `void probeModels(...)`
  // (fire-and-forget); if the user changes provider before that
  // first probe's `await listModels(...)` resolves, a second probe
  // starts + both overlap. Without a guard, the first probe's
  // `finally { probing = false }` runs AFTER the second set
  // `probing = true` (leaving probing falsely false), and its
  // `modelList = list` can overwrite the second's list with the
  // WRONG provider's models if it completes last. Each probe
  // captures `my = ++probeGen` and bails early if a newer probe
  // started (`my !== probeGen`); only the latest probe owns the
  // `probing` flag + `modelList`. Plain `let` (not `$state`) — the
  // UI never renders `probeGen` directly, so reactivity isn't
  // needed; the closure captures the live binding.
  let probeGen = 0;

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

  // Render assistant Text content as markdown (Task 2). marked parses
  // to HTML (breaks:true → single newlines render as <br>); DOMPurify
  // strips <script>/event handlers/other XSS vectors before {@html}
  // injection (agent output is半可信 but still defense-in-depth).
  // marked.parse is sync by default (async:false) → returns string;
  // the cast guards against the string|Promise<string> union type from
  // marked's overload signatures (the sync overload returns string).
  // User bubbles + non-Text kinds keep their existing plain-text render
  // (Task 3 restyles the Thinking/ToolUse/ToolResult/Status/Error blocks).
  function renderMarkdown(content: string): string {
    return DOMPurify.sanitize(
      marked.parse(content ?? "", { breaks: true }) as string,
    );
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
        // save_turn_record) — render history. Task 1 / design "改动 3":
        // the persisted `kind` lets the SAME per-kind render branches
        // the live stream uses fire for history too (thinking blocks
        // stay collapsed + pretty after a reload — Review Focus #3).
        // `load_messages` backfills `kind` from `role` for pre-Task-1
        // rows, so `kind` is rarely null at the IPC boundary; when it
        // IS null (unknown role), fall back to `roleToKind` so the
        // render branch is still sensible (Text for assistant/user,
        // ToolResult for tool, Status for system). The brief's "kind
        // null → Text" rule is honored for truly-unknown roles.
        messages = history.map((m) => ({
          role: m.role,
          content: m.content,
          kind: (m.kind as MessageKind | null | undefined) ?? roleToKind(m.role),
          // Task 6 fix-round-1: carry the persisted `tool` name so the
          // reload path shows "使用工具 Bash" like the live path (was
          // dropped → "使用工具 工具" doubled fallback). `m.tool` is
          // null/undefined for pre-fix rows + non-tool kinds; the chip
          // fallback handles those.
          tool: m.tool ?? null,
          // Convert the persisted RFC3339 UTC timestamp to local
          // YYYY-MM-DD HH:MM:SS for display. null for pre-created_at
          // rows (the formatTimestamp helper handles gracefully).
          timestamp: formatTimestamp(m.created_at),
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
    dialogAgentId = null;
    creating = false;
    try {
      // Load saved agent profiles for the selector.
      agents = await listAgents();
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
   * from the dropdown (the user can switch to manual entry).
   *
   * Generation-counter guard (Fix round 1 #2): if a newer probe has
   * started while we were awaiting, bail without touching
   * `modelList` / `manualModel` / `probeError` / `probing` — the
   * newer probe owns those. Its `finally` will clear `probing`. */
  async function probeModels(provider: Provider) {
    probeGen++;
    const my = probeGen;
    probing = true;
    probeError = null;
    manualModel = false;
    modelList = [];
    try {
      const list = await listModels(provider);
      // Stale: a newer probe started while we were awaiting. Bail
      // without touching shared state — the newer probe owns it.
      if (my !== probeGen) return;
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
      // Stale probe's error must not overwrite the newer probe's
      // state — bail the same way.
      if (my !== probeGen) return;
      probeError = `探测模型失败: ${e}`;
      manualModel = true;
    } finally {
      // Only the latest probe owns the `probing` flag. A stale probe
      // clearing `probing` would leave it falsely false while the
      // newer probe is still in flight.
      if (my === probeGen) probing = false;
    }
  }

  /** When the user selects a saved agent profile, auto-fill
   *  provider/model/cwd from the profile (read-only). When "无" is
   *  selected, revert to manual provider/model/cwd entry. */
  function onAgentChange() {
    if (!dialogAgentId) {
      // "无" — manual mode: reset to defaults and re-probe
      dialogModel = "";
      manualModel = false;
      modelList = [];
      void onDialogProviderChange();
      return;
    }
    const agent = agents.find((a) => a.id === dialogAgentId);
    if (!agent) return;
    // Auto-fill from the agent profile
    dialogProvider = agent.provider as Provider;
    dialogModel = agent.model ?? "";
    dialogCwd = agent.cwd ?? "";
    manualModel = true; // skip probe; the model comes from the profile
    modelList = [];
    probing = false;
  }

  /** Re-probe when the user changes the provider in the dialog. Also
   *  re-pull the per-agent default model for the new provider (the
   *  user may have configured a default for claude but not codex). */
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
      const conv = await newConversation(dialogProvider, model, dialogCwd, dialogAgentId);
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
      timestamp: nowTimestamp(),
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
      // Carry the tool name so the ToolUse block's "使用工具 X"
      // label shows the actual tool (e.g. "Bash"). History ToolUse
      // rows ALSO carry `tool` now (Task 6 fix-round-1 persisted it
      // — the history-load map passes `m.tool`); null → "（未知）".
      tool: message.tool,
      timestamp: nowTimestamp(),
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
        timestamp: nowTimestamp(),
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
      timestamp: nowTimestamp(),
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

  // Keyboard: Enter sends; Shift+Enter or Ctrl/Cmd+Enter inserts a newline
  // (conventional chat-composer behavior — Enter alone is the primary send
  // gesture, multi-line input needs an explicit modifier).
  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey && !e.ctrlKey && !e.metaKey) {
      e.preventDefault();
      void send();
    }
    // Ctrl/Cmd+Enter or Shift+Enter → fall through (textarea inserts a newline)
  }

  // Derived: is the active conversation currently streaming?
  let isStreaming = $derived(
    activeConvId !== null && streamingConvIds.has(activeConvId),
  );

  // Live status label for the streaming indicator — derived from the latest
  // streamed message kind, similar to multica's TaskStatusPill. Shows what
  // the agent is doing RIGHT NOW (thinking, using a tool, generating text)
  // rather than a generic "生成中…". Falls back to "处理中…" when no
  // messages have arrived yet (the agent is still initializing).
  let streamingLabel = $derived.by(() => {
    if (!isStreaming) return "";
    const last = messages[messages.length - 1];
    if (!last) return "连接中…";
    switch (last.kind) {
      case "Thinking":
        return "思考中…";
      case "ToolUse":
        return last.tool ? `使用 ${last.tool}…` : "使用工具…";
      case "ToolResult":
        return "处理结果中…";
      case "Text":
        return "生成中…";
      case "Status":
        return last.content || "处理中…";
      case "Error":
        return "出错了";
      default:
        return "处理中…";
    }
  });

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
        {#if isStreaming}<span class="streaming">{streamingLabel}</span>{/if}
      </div>
      <div class="messages" bind:this={scrollEl}>
        {#each messages as m (m)}
          <!-- Per-kind render branches (design "改动 3"). The SAME
               branches fire for live-streamed messages (m.kind from
               the backend's MessageKind) AND for history messages
               (m.kind from StoredMessage.kind, persisted by Task 1 /
               db::save_turn_record + backfilled by load_messages).
               kind null/unknown → Text (the brief's rule). -->
          {#if m.kind === "Thinking"}
            <!-- 🤔 思考: dimmed, default-collapsed (native <details>
                 is collapsed until the user clicks the summary). -->
            <details class="block thinking">
              <summary>🤔 思考</summary>
              <div class="block-content">{m.content}</div>
            </details>
          {:else if m.kind === "ToolUse"}
            <!-- 🔧 使用工具 X: X = m.tool (the streamed Message.tool
                 field OR the persisted StoredMessage.tool — Task 6
                 fix-round-1 added the latter so history reload shows
                 the tool name). Falls back to "（未知）" when tool is
                 null/empty (pre-fix rows / non-tool kinds) so the chip
                 reads "使用工具（未知）" rather than the doubled
                 "使用工具 工具". The tool INPUT is m.content (the
                 backends parse the tool-call input into the ToolUse
                 message's content). -->
            <details class="block tool-use">
              <summary>🔧 使用工具{m.tool ? ` ${m.tool}` : "（未知）"}</summary>
              <pre class="block-content">{m.content}</pre>
            </details>
          {:else if m.kind === "ToolResult"}
            <!-- ✅ 工具结果: default-collapsed; m.content is the
                 tool's output (parsed by the backends). -->
            <details class="block tool-result">
              <summary>✅ 工具结果</summary>
              <pre class="block-content">{m.content}</pre>
            </details>
          {:else if m.kind === "Status"}
            <!-- Status: a small dimmed inline line (not a full bubble
                 — design "改动 3": "小状态行"). -->
            <div class="status-line">{m.content}</div>
          {:else if m.kind === "Error"}
            <!-- Error: red error bubble (the content is the error
                 message). Keep the .bubble base + data-role so the
                 align/color base still applies, layered with the
                 red error override. -->
            <div class="bubble error" data-role={m.role} data-kind="Error">
              <span class="role">error</span>
              <span class="content">{m.content}</span>
            </div>
          {:else if m.kind === "Log"}
            <!-- Log: best-effort debug line — very dimmed, small.
                 Usually not surfaced to the user but render it if
                 it arrives (don't silently drop streamed data). -->
            <div class="log-line">{m.content}</div>
          {:else}
            <!-- Text (or kind null/unknown → treat as Text per the
                 brief). Normal user/assistant bubble; the role-based
                 align + color is driven by data-role. Hide the role
                 label on user messages (the right-aligned blue bubble
                 is self-evidently the user — multica style); show it
                 for assistant so the source is clear.
                 Task 2: assistant Text renders as markdown via
                 renderMarkdown → {@html} (marked + DOMPurify). User
                 Text stays plain text. The .md class scopes the
                 GLOBAL markdown element styles in theme.css to the
                 assistant bubble only — Svelte scoped <style> does NOT
                 reach {@html}-injected content, so those styles must
                 live in theme.css (not this component's <style>). The
                 content wrapper is a <div> (not <span>) so block
                 elements (<p>/<pre>/<ul>) render validly inside it. -->
            <div
              class="bubble"
              class:md={m.role !== "user"}
              data-role={m.role}
              data-kind="Text"
            >
              {#if m.role !== "user"}<span class="role">{m.role}</span>{/if}
              <div class="content">
                {#if m.role === "user"}{m.content}{:else}{@html renderMarkdown(m.content)}{/if}
              </div>
              {#if m.timestamp}<span class="msg-time">{m.timestamp}</span>{/if}
            </div>
          {/if}
        {/each}
      </div>
      <div class="composer">
        <textarea
          bind:value={draft}
          onkeydown={onKeydown}
          placeholder="输入消息… (Enter 发送, Shift+Enter 换行)"
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
      {#if agents.length > 0}
        <div class="dialog-field">
          <label for="dlg-agent">智能体</label>
          <select
            id="dlg-agent"
            bind:value={dialogAgentId}
            onchange={onAgentChange}
            disabled={creating}
          >
            <option value={null}>无（直接选 Provider）</option>
            {#each agents as a (a.id)}
              <option value={a.id}>{a.name}</option>
            {/each}
          </select>
        </div>
      {/if}
      <div class="dialog-field">
        <label for="dlg-provider">Agent</label>
        <select
          id="dlg-provider"
          bind:value={dialogProvider}
          onchange={onDialogProviderChange}
          disabled={probing || creating || dialogAgentId !== null}
        >
          {#each PROVIDERS as p (p)}
            <option value={p}>{PROVIDER_LABEL[p]}</option>
          {/each}
        </select>
      </div>
      <div class="dialog-field">
        <label for="dlg-model">模型</label>
        {#if probing && !manualModel}
          <!-- Probing + the user hasn't toggled manual: show the
               "探测中…" hint AND the `手动输入` escape hatch (Fix
               round 1 #1). For codex (probe up to ~15s) the user was
               previously FORCED to wait with no way to confirm or
               switch to manual entry — the toggle was in the
               `{:else}` (dropdown) branch, unreachable during
               probing, and confirm was `disabled={probing || ...}`.
               Now the user can click `手动输入` mid-probe to switch
               to a text input + type a model (or keep a pre-filled
               default) while the probe runs. `manualModel` survives
               the probe's completion (probeModels only ever SETS it
               to true, never back to false after the await), so the
               user's toggle is respected. -->
          <div class="model-probing">探测中…</div>
          <button class="link-btn" onclick={toggleManual} disabled={creating}>
            手动输入
          </button>
        {:else if manualModel || modelList.length === 0}
          <input
            id="dlg-model"
            type="text"
            bind:value={dialogModel}
            placeholder="手动输入模型名 (留空 = 用 agent 默认模型)"
            disabled={creating || dialogAgentId !== null}
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
          disabled={creating || dialogAgentId !== null}
        />
        <small class="muted">agent 子进程的 cwd。留空 = 用全局工作目录。</small>
      </div>
      {#if probeError}<div class="dialog-error">{probeError}</div>{/if}
      <div class="dialog-actions">
        <!-- Fix round 1 #1: allow confirm while `probing` IF the user
             has a model (typed or pre-filled) — the probe only
             populates the dropdown, it doesn't validate the model, so
             a user with a pre-filled per-agent default (or who typed
             a name via the now-visible `手动输入` escape hatch) can
             confirm without waiting out the codex ~15s probe. Still
             disabled when `creating` (the actual newConversation
             call). `probing && !dialogModel` keeps it disabled when
             probing + no model yet (the probe might auto-pick one). -->
        <button
          onclick={confirmDialog}
          disabled={creating || (probing && !dialogModel)}
        >
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
  /* Task 3: refined restyle. Every color is a theme.css token so the
   * whole view flips with data-theme; no hardcoded hex anywhere. Soft
   * shadow only on the truly elevated surface (the new-conv dialog).
   * Radius ladder: bubbles → --r-bubble, chips/buttons → --r-chip,
   * code/input blocks → --r-code, dialog card → --r-panel. Spacing
   * follows the 8px rhythm (--space-1..5). The GLOBAL .bubble.md
   * markdown element styles live in theme.css (Task 2) and are NOT
   * duplicated here — only the bubble container + non-markdown blocks
   * (Thinking/ToolUse/ToolResult/Status/Error) are styled below. */

  .chat {
    display: flex;
    height: 100%;
    min-height: 0;
  }

  /* -- sidebar: conversation list + 新建对话 button -- */
  .sidebar {
    width: 200px;
    flex: 0 0 200px;
    border-right: 1px solid var(--bd);
    background: var(--surface);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .new-row {
    display: flex;
    gap: var(--space-1);
    padding: var(--space-2);
    border-bottom: 1px solid var(--bd);
  }

  .new-row button {
    flex: 1;
    font-size: 0.85rem;
    padding: var(--space-1) var(--space-2);
    cursor: pointer;
    background: var(--acc);
    color: #fff;
    border: none;
    border-radius: var(--r-chip);
    transition: opacity var(--t-fast);
  }

  .new-row button:hover {
    opacity: 0.9;
  }

  .new-row button:disabled {
    background: var(--mut);
    cursor: not-allowed;
    opacity: 1;
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
    padding: var(--space-1) var(--space-3);
    background: transparent;
    border: none;
    border-bottom: 1px solid var(--bd);
    cursor: pointer;
    text-align: left;
    color: var(--ink);
    transition: background var(--t-fast);
  }

  .conv-row:hover {
    background: var(--acc-soft);
  }

  .conv-row.active {
    background: var(--acc-soft);
  }

  .conv-row.active .conv-title {
    color: var(--acc);
  }

  .conv-provider {
    font-size: 0.65rem;
    color: var(--mut);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .conv-title {
    font-size: 0.8rem;
    color: var(--ink);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    width: 100%;
  }

  .conv-model {
    font-size: 0.65rem;
    color: var(--mut);
    font-family: var(--mono);
  }

  .empty {
    padding: var(--space-4);
    text-align: center;
    color: var(--mut);
    font-size: 0.85rem;
  }

  /* -- main pane -- */
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
    color: var(--mut);
    font-size: 0.9rem;
  }

  /* Header chips: provider / model / cwd — all share the same pill
   * treatment (bd border, mut text, bg fill) per the brief; the model
   * + cwd chips additionally use the mono font as a subtle content
   * cue. The "默认模型" muted variant drops to italic mut. */
  .main-header {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--bd);
    background: var(--surface);
    flex-wrap: wrap;
  }

  .hdr-chip {
    font-size: 0.68rem;
    padding: 2px var(--space-2);
    border-radius: var(--r-chip);
    background: var(--bg);
    color: var(--mut);
    border: 1px solid var(--bd);
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .hdr-chip.model {
    font-family: var(--mono);
  }

  .hdr-chip.model.muted {
    font-style: italic;
  }

  .hdr-chip.cwd {
    font-family: var(--mono);
  }

  .streaming {
    font-size: 0.7rem;
    color: var(--acc);
    font-style: italic;
    margin-left: auto;
  }

  /* -- messages -- */
  .messages {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-3);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    background: var(--bg);
  }

  /* Bubble container. User = accent fill + white text (accent
   * foreground); assistant = layered surface card (surface bg + bd
   * border) so it reads as a raised tile over --bg; system = dimmed
   * inline line. The inner markdown HTML is styled by the GLOBAL
   * .bubble.md rules in theme.css — nothing here touches p/ul/code. */
  .bubble {
    max-width: 80%;
    padding: var(--space-2) var(--space-3);
    border-radius: var(--r-bubble);
    font-size: 0.8rem;
    line-height: 1.5;
    word-break: break-word;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .bubble .role {
    font-size: 0.6rem;
    text-transform: uppercase;
    color: var(--mut);
    letter-spacing: 0.05em;
  }

  /* Timestamp line below the message content — small, muted, mono font.
   * Shows the local time the message was sent (user) or received (agent)
   * in YYYY-MM-DD HH:MM:SS format. */
  .msg-time {
    font-size: 0.62rem;
    color: currentColor;
    font-family: var(--mono);
    opacity: 0.55;
    margin-top: 2px;
    align-self: flex-end;
  }

  .bubble[data-role="user"] {
    align-self: flex-end;
    background: var(--acc);
    color: #fff;
  }

  .bubble[data-role="user"] .role {
    color: #fff;
    opacity: 0.8;
  }

  .bubble[data-role="assistant"] {
    align-self: flex-start;
    background: var(--surface);
    color: var(--ink);
    border: 1px solid var(--bd);
  }

  .bubble[data-role="tool"] {
    align-self: flex-start;
    background: var(--surface);
    color: var(--mut);
    font-family: var(--mono);
    font-size: 0.72rem;
    border: 1px solid var(--bd);
  }

  .bubble[data-role="system"] {
    align-self: center;
    background: transparent;
    color: var(--mut);
    font-style: italic;
    font-size: 0.72rem;
  }

  /* Error: red treatment via the --err/--err-soft tokens. Source order
   * after the role rules so it overrides the assistant base. */
  .bubble[data-kind="Error"] {
    background: var(--err-soft);
    color: var(--err);
    border: 1px solid var(--err);
  }

  .bubble[data-kind="Error"] .role {
    color: var(--err);
    opacity: 0.85;
  }

  /* -- collapsible blocks (thinking / tool-use / tool-result) --
     Native <details>/<summary>: collapsed by default, expands on
     click. No JS toggle needed. The .block card uses --r-bubble + a
     --bd border + --surface fill; per-kind summary/content tints
     follow (thinking → soft accent dimmed, tool-use → indigo summary
     + dark code content, tool-result → muted summary + dark code
     content). The inner <pre> block-content is a dark code block
     (var(--code-bg)/var(--code-ink)) to match the markdown code
     aesthetic in BOTH themes. */
  .block {
    align-self: flex-start;
    width: 100%;
    max-width: 96%;
    border: 1px solid var(--bd);
    border-radius: var(--r-bubble);
    background: var(--surface);
    font-size: 0.75rem;
    overflow: hidden;
  }

  .block > summary {
    cursor: pointer;
    padding: var(--space-1) var(--space-3);
    font-size: 0.72rem;
    font-weight: 500;
    user-select: none;
    list-style: none;
    display: flex;
    align-items: center;
    gap: var(--space-1);
    color: var(--ink);
  }

  /* Drop the default disclosure triangle — the emoji is the marker + a
     custom ▸ that rotates when open. */
  .block > summary::-webkit-details-marker {
    display: none;
  }

  .block > summary::before {
    content: "▸";
    font-size: 0.65rem;
    color: var(--mut);
    transition: transform var(--t-fast);
  }

  .block[open] > summary::before {
    transform: rotate(90deg);
  }

  .block-content {
    padding: var(--space-2) var(--space-3);
    border-top: 1px solid var(--bd);
    white-space: pre-wrap;
    word-break: break-word;
    font-family: var(--mono);
    font-size: 0.7rem;
    line-height: 1.4;
    color: var(--ink);
    /* Cap the height so a huge tool output doesn't swallow the whole
       panel — scroll inside the block instead. */
    max-height: 320px;
    overflow-y: auto;
  }

  /* Thinking: soft accent-tinted card + dimmed muted italic content
   * (chain-of-thought reads as secondary). */
  .block.thinking {
    background: var(--acc-soft);
    border-color: var(--acc-soft);
  }

  .block.thinking > summary {
    color: var(--acc);
  }

  .block.thinking .block-content {
    color: var(--mut);
    font-style: italic;
    background: transparent;
    border-top-color: var(--acc-soft);
  }

  /* ToolUse: neutral card + indigo summary (the agent is acting) +
   * dark code content (the tool input). */
  .block.tool-use {
    background: var(--surface);
    border-color: var(--bd);
  }

  .block.tool-use > summary {
    color: var(--acc);
  }

  .block.tool-use .block-content {
    background: var(--code-bg);
    color: var(--code-ink);
    border-top-color: var(--code-bg);
  }

  /* ToolResult: neutral card + muted summary (the result is
   * secondary) + dark code content (the tool output). */
  .block.tool-result {
    background: var(--surface);
    border-color: var(--bd);
  }

  .block.tool-result > summary {
    color: var(--mut);
  }

  .block.tool-result .block-content {
    background: var(--code-bg);
    color: var(--code-ink);
    border-top-color: var(--code-bg);
  }

  /* Status: a small dimmed inline line (not a full bubble). */
  .status-line {
    align-self: center;
    font-size: 0.68rem;
    color: var(--mut);
    font-style: italic;
    padding: 2px var(--space-2);
    text-align: center;
  }

  /* Log: very dimmed small line (debug — usually not surfaced). */
  .log-line {
    align-self: flex-start;
    font-size: 0.68rem;
    color: var(--mut);
    font-family: var(--mono);
    padding: 1px var(--space-2);
    max-width: 96%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    opacity: 0.75;
  }

  /* -- composer -- */
  .composer {
    display: flex;
    gap: var(--space-2);
    padding: var(--space-2);
    border-top: 1px solid var(--bd);
    background: var(--surface);
  }

  .composer textarea {
    flex: 1;
    resize: vertical;
    font-family: inherit;
    font-size: 0.8rem;
    padding: var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-bubble);
    background: var(--bg);
    color: var(--ink);
    transition: border-color var(--t-fast);
  }

  .composer textarea:focus {
    border-color: var(--acc);
    outline: none;
  }

  .composer textarea::placeholder {
    color: var(--mut);
  }

  .composer button {
    padding: var(--space-2) var(--space-4);
    font-size: 0.8rem;
    cursor: pointer;
    background: var(--acc);
    color: #fff;
    border: none;
    border-radius: var(--r-chip);
    transition: opacity var(--t-fast);
  }

  .composer button:hover {
    opacity: 0.9;
  }

  .composer button:disabled {
    background: var(--mut);
    cursor: not-allowed;
    opacity: 1;
  }

  /* -- error banner (bottom of the main pane) -- */
  .error {
    padding: var(--space-2) var(--space-3);
    background: var(--err-soft);
    color: var(--err);
    font-size: 0.8rem;
    border-top: 1px solid var(--err);
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

  /* The elevated card: surface fill + bd border + panel radius + the
   * soft shadow token — the one truly raised surface in this view. */
  .dialog {
    background: var(--surface);
    border: 1px solid var(--bd);
    border-radius: var(--r-panel);
    padding: var(--space-4);
    width: 420px;
    max-width: 90vw;
    max-height: 90vh;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    box-shadow: var(--shadow);
  }

  .dialog h2 {
    margin: 0;
    font-size: 1rem;
    color: var(--ink);
  }

  .dialog-field {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }

  .dialog-field label {
    font-size: 0.82rem;
    color: var(--ink);
    font-weight: 500;
  }

  .dialog-field select,
  .dialog-field input[type="text"] {
    font-size: 0.85rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--bg);
    color: var(--ink);
  }

  .dialog-field select:focus,
  .dialog-field input[type="text"]:focus {
    border-color: var(--acc);
    outline: none;
  }

  /* Probing state: accent italic hint. */
  .dialog-field .model-probing {
    font-size: 0.85rem;
    color: var(--acc);
    font-style: italic;
    padding: var(--space-1) var(--space-2);
  }

  .dialog-field small.muted {
    font-size: 0.7rem;
    color: var(--mut);
  }

  .link-btn {
    align-self: flex-start;
    background: transparent;
    border: none;
    color: var(--acc);
    font-size: 0.75rem;
    cursor: pointer;
    padding: 2px 0;
    text-decoration: underline;
  }

  .link-btn:disabled {
    color: var(--mut);
    cursor: not-allowed;
  }

  .dialog-error {
    font-size: 0.8rem;
    color: var(--err);
    background: var(--err-soft);
    padding: var(--space-1) var(--space-2);
    border-radius: var(--r-code);
  }

  .dialog-actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
    margin-top: var(--space-1);
  }

  .dialog-actions button {
    padding: var(--space-2) var(--space-4);
    font-size: 0.88rem;
    cursor: pointer;
    background: var(--acc);
    color: #fff;
    border: none;
    border-radius: var(--r-chip);
    transition: opacity var(--t-fast);
  }

  .dialog-actions button:hover {
    opacity: 0.9;
  }

  .dialog-actions button:disabled {
    background: var(--mut);
    cursor: not-allowed;
    opacity: 1;
  }

  .dialog-actions button.secondary {
    background: var(--bg);
    color: var(--ink);
    border: 1px solid var(--bd);
  }

  .dialog-actions button.secondary:hover {
    background: var(--acc-soft);
    opacity: 1;
  }
</style>
