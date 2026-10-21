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
 * RFC3339 string at the IPC boundary (chrono serializes via serde). */
export interface Conversation {
  id: string;
  provider: string;
  session_id: string | null;
  title: string;
  created_at: string;
}

/** Mirrors `agent::conversation::StoredMessage` (the persisted shape —
 * role + content only; the kind tag / tool metadata are dropped at
 * persist time per the design spec). */
export interface StoredMessage {
  role: "user" | "assistant" | "system" | "tool";
  content: string;
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
 * null, title="新对话", now) + return it. Called by the "新建" button
 * after the user picks a provider. */
export async function newConversation(provider: Provider): Promise<Conversation> {
  return invoke<Conversation>("new_conversation", { provider });
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
 * `chat_result` events (subscribed in `stream.ts`). */
export async function chatSend(
  convId: string,
  prompt: string,
  provider: Provider,
): Promise<void> {
  await invoke("chat_send", { convId, prompt, provider });
}
