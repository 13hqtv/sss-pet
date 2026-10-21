<!-- Task 15: the 对话 (chat) tab — the main panel view. Left sidebar =
     conversation list (list_conversations + 新建 button with a provider
     picker); right pane = message bubbles (load_history on select +
     live-streamed chat_message events) + provider picker + composer.
     On send → chat_send(conv_id, prompt, provider) → backend spawns
     the stream-drain task + returns; chat_message / chat_result
     events arrive via stream.ts (dispatched by conv_id).

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
    type Conversation,
    type StoredMessage,
    type Message,
    type MessageKind,
    type Provider,
  } from "../lib/api";
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
  let newProvider: Provider = $state("claude");
  // The provider used for the active conversation's NEXT send.
  // Defaults to the active conversation's own provider; the user can
  // override via the picker (the override only affects the send — it
  // doesn't rewrite history).
  let sendProvider: Provider = $state("claude");

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
    const conv = conversations.find((c) => c.id === convId);
    if (conv) {
      sendProvider = (conv.provider as Provider) || "claude";
    }
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

  async function createConversation() {
    try {
      const conv = await newConversation(newProvider);
      conversations = [conv, ...conversations];
      liveBuffers.set(conv.id, []);
      await selectConversation(conv.id);
      errorMsg = null;
    } catch (e) {
      errorMsg = `新建会话失败: ${e}`;
    }
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
      await chatSend(convId, prompt, sendProvider);
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

  function handleStreamResult(convId: string, _result: unknown) {
    const cleared = new Set(streamingConvIds);
    cleared.delete(convId);
    streamingConvIds = cleared;
    // The result is persisted by save_turn_record; we don't render the
    // structured ExecResult as a bubble (its output, if non-empty,
    // already arrived as a Text chat_message). A future tightening
    // could surface a "turn done" status chip here.
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
  <!-- Sidebar: conversation list + 新建 button + provider picker. -->
  <aside class="sidebar">
    <div class="new-row">
      <select
        bind:value={newProvider}
        aria-label="新建会话的 provider"
        title="新建会话的 provider"
      >
        {#each PROVIDERS as p}
          <option value={p}>{PROVIDER_LABEL[p]}</option>
        {/each}
      </select>
      <button onclick={createConversation}>新建</button>
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
    {#if activeConvId === null}
      <div class="no-conv">
        <p>选择左侧会话或点击「新建」开始对话</p>
      </div>
    {:else}
      <div class="main-header">
        <select
          bind:value={sendProvider}
          aria-label="本次发送的 provider"
          title="本次发送的 provider"
        >
          {#each PROVIDERS as p}
            <option value={p}>{PROVIDER_LABEL[p]}</option>
          {/each}
        </select>
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

  .new-row select {
    flex: 1;
    font-size: 0.8rem;
    padding: 2px 4px;
  }

  .new-row button {
    font-size: 0.8rem;
    padding: 2px 8px;
    cursor: pointer;
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
    gap: 8px;
    padding: 6px 10px;
    border-bottom: 1px solid #cbd5e1;
    background: #f8fafc;
  }

  .main-header select {
    font-size: 0.8rem;
    padding: 2px 4px;
  }

  .streaming {
    font-size: 0.75rem;
    color: #2563eb;
    font-style: italic;
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
</style>
