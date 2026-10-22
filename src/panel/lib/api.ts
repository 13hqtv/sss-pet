// Task 15: thin `invoke` wrappers for the four chat Tauri commands
// (`list_conversations`, `new_conversation`, `load_history`,
// `chat_send`). Keeps the `invoke("plugin_chat_command", { args })`
// boilerplate + the arg-name snake_casing in ONE place so
// `ChatView.svelte` reads as declarative business logic, not IPC.
//
// `withGlobalTauri:true` is set in `tauri.conf.json`, so the
// `@tauri-apps/api/core` `invoke` is the canonical IPC call (the
// global `window.__TAURI__` shim is for sandboxed contexts only).
//
// Shapes mirror the Rust types in `src-tauri/src/chat.rs` +
// `src-tauri/src/agent/conversation.rs`:
// - `Conversation` ← `agent::conversation::Conversation` (id, provider,
//   session_id, title, created_at — a chrono RFC3339 string at the
//   IPC boundary).
// - `StoredMessage` ← `agent::conversation::StoredMessage` (role,
//   content).
// - The streamed `Message` (the `chat_message` event payload's
//   `message` field) ← `agent::Message` (kind, content, tool, call_id).
//   The `ExecResult` (`chat_result`'s `result` field) ←
//   `agent::ExecResult`.

import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// Types (mirror the Rust serde shapes — snake_case at the IPC boundary
// because serde serializes Rust fields as-is by default).
// ---------------------------------------------------------------------------

export type Provider = "claude" | "codex" | "opencode";

/** Mirrors `agent::conversation::Conversation`. `created_at` is an
 * RFC3339 string at the IPC boundary (chrono serializes via serde).
 * Task 1: `model` + `cwd` are set at creation (immutable) — the
 * frontend reads them for display but never rewrites them. */
export interface Conversation {
  id: string;
  provider: string;
  session_id: string | null;
  title: string;
  created_at: string;
  /** The model selected at creation (`null` = the agent's own default
   * model — no `--model` flag). Immutable. Task 1. */
  model: string | null;
  /** The working directory the agent CLI runs in for this conversation.
   * Immutable. Task 1. */
  cwd: string;
}

/** Mirrors `agent::conversation::StoredMessage` (the persisted shape —
 * role + content + the `MessageKind` variant NAME). Task 1 added the
 * `kind` field so reopening a history conversation still renders
 * thinking / tool blocks the same way the live stream did (design
 * "改动 3"). `kind` is `null` for rows written before Task 1's
 * migration whose role didn't match the db's backfill heuristic
 * (`infer_kind_from_role`); the frontend treats `null` as `Text`.
 * The `tool` metadata is NOT persisted (only the kind name + content
 * are) — ToolUse blocks from history fall back to the "工具" label. */
export interface StoredMessage {
  role: "user" | "assistant" | "system" | "tool";
  content: string;
  kind?: string | null;
}

/** Mirrors `agent::MessageKind`. The streamed Message carries this tag
 * + tool metadata so the webview can render text / thinking / tool-use
 * / tool-result uniformly. */
export type MessageKind =
  | "Text"
  | "Thinking"
  | "ToolUse"
  | "ToolResult"
  | "Status"
  | "Error"
  | "Log";

/** Mirrors `agent::Message` (the streamed shape — distinct from
 * `StoredMessage`). The `chat_message` event payload's `message`
 * field is one of these. */
export interface Message {
  kind: MessageKind;
  content: string;
  tool: string | null;
  call_id: string | null;
}

/** Mirrors `agent::ExecResult`. The `chat_result` event payload's
 * `result` field is one of these. `usage` is a map of model name →
 * token counts (empty for v1 backends — Task 10/11/12 don't populate
 * it yet). */
export interface ExecResult {
  status: string;
  output: string;
  session_id: string | null;
  usage: Record<string, unknown>;
}

// ---------------------------------------------------------------------------
// Command wrappers
// ---------------------------------------------------------------------------

/** `chat::list_conversations` — all persisted conversations, oldest
 * first. Called by `ChatView`'s sidebar on mount + after a new
 * conversation is created. */
export async function listConversations(): Promise<Conversation[]> {
  return invoke<Conversation[]>("list_conversations");
}

/** `chat::new_conversation` — create a row (id=uuid v4, session_id=
 * null, title="新对话", now) + return it. Called by the new-conversation
 * dialog (Task 5) after the user picks a provider + model + cwd. Task 5:
 * `model` (null = use the agent's own default model) + `cwd` (the working
 * directory the agent CLI runs in, pinned at creation — immutable) are
 * passed from the dialog. An empty `cwd` falls back to the global
 * config's `working_dir` server-side. */
export async function newConversation(
  provider: Provider,
  model: string | null,
  cwd: string,
): Promise<Conversation> {
  return invoke<Conversation>("new_conversation", { provider, model, cwd });
}

/** `chat::load_history` — all stored messages for a conversation
 * (arrival order). Called by `ChatView` when the user selects a
 * conversation in the sidebar. */
export async function loadHistory(convId: string): Promise<StoredMessage[]> {
  return invoke<StoredMessage[]>("load_history", { convId });
}

/** `chat::chat_send` — kick off a chat turn. Returns immediately
 * (run_chat_turn spawns the stream-drain task + returns); the live
 * message stream + final result arrive via `chat_message` /
 * `chat_result` events (subscribed in `stream.ts`).
 *
 * Task 5 carry: NO `provider` arg — the conversation's agent is fixed
 * at creation (`conv.provider`, immutable). The backend loads the
 * conversation row + uses `conv.provider` for `agent::new`. The
 * frontend no longer offers a per-send provider picker. */
export async function chatSend(convId: string, prompt: string): Promise<void> {
  await invoke("chat_send", { convId, prompt });
}

/** `list_models` — per-agent model discovery (Task 3). Returns the
 * model IDs the agent's CLI advertises (claude `list_models` control
 * request, codex `debug models`, opencode `models`). Empty list on any
 * failure (binary missing / timeout / parse error) — the new-conv
 * dialog + SettingsView fall back to a manual text input for the model
 * name (design: 兜底). */
export async function listModels(provider: Provider): Promise<string[]> {
  return invoke<string[]>("list_models", { provider });
}
