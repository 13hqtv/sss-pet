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
  model: string | null;
  cwd: string;
  /** The saved agent profile this conversation was created with.
   * `null` = no agent profile (raw provider+model+cwd). Immutable. */
  agent_id: string | null;
}

/** Mirrors `agent::conversation::StoredMessage` (the persisted shape —
 * role + content + the `MessageKind` variant NAME). Task 1 added the
 * `kind` field so reopening a history conversation still renders
 * thinking / tool blocks the same way the live stream did (design
 * "改动 3"). `kind` is `null` for rows written before Task 1's
 * migration whose role didn't match the db's backfill heuristic
 * (`infer_kind_from_role`); the frontend treats `null` as `Text`.
 * Task 6 fix-round-1: added the `tool` field (the tool NAME, e.g.
 * "Bash") so history ToolUse chips show the tool identity on reload
 * (was dropped → "使用工具 工具" doubled fallback). `null` for pre-fix
 * rows + non-tool kinds; the frontend falls back to "（未知）". */
export interface StoredMessage {
  role: "user" | "assistant" | "system" | "tool";
  content: string;
  kind?: string | null;
  tool?: string | null;
  /** RFC3339 UTC timestamp (set by the backend at persist time).
   * The frontend converts this to local `YYYY-MM-DD HH:MM:SS` for display. */
  created_at?: string | null;
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
  agentId?: string | null,
): Promise<Conversation> {
  return invoke<Conversation>("new_conversation", {
    provider,
    model,
    cwd,
    agentId: agentId ?? null,
  });
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

/** `get_exe_path` — returns the absolute path to the running sss-pet.exe.
 * Used by AgentsView to auto-fill the MCP config's `command` field. */
export async function getExePath(): Promise<string> {
  return invoke<string>("get_exe_path");
}

// ---------------------------------------------------------------------------
// Agent management (agents + skills + agent_skills + agent generation)
// ---------------------------------------------------------------------------

/** Mirrors `db::Agent`. A saved agent profile = provider + model +
 * system_prompt + mcp_config + cwd, named and reusable. */
export interface Agent {
  id: string;
  name: string;
  description: string | null;
  provider: string;
  model: string | null;
  system_prompt: string | null;
  mcp_config: string | null;
  cwd: string | null;
  created_at: string;
  updated_at: string;
}

/** Mirrors `db::SkillFile`. A supporting script file belonging to a skill.
 *  `path` is a slash-delimited relative path that may include subdirs
 *  (e.g. `scripts/analyze.py`); directories are inferred from paths. Text
 *  only (binary rejected by the backend). Fields are snake_case at the IPC
 *  boundary (serde default — matches the `Skill` interface). */
export interface SkillFile {
  id: string;
  skill_id: string;
  path: string;
  content: string;
  created_at: string;
  updated_at: string;
}

/** Mirrors `db::Skill`. A reusable skill (SKILL.md body + metadata +
 *  supporting script files). Skills are assigned to agents via the
 *  `agent_skills` junction; at chat-turn time each enabled skill is
 *  materialized to `<cwd>/.<provider>/skills/<slug>/` so the agent CLI
 *  discovers it natively. */
export interface Skill {
  id: string;
  name: string;
  description: string | null;
  content: string | null;
  created_at: string;
  updated_at: string;
}

// -- Agent CRUD --

export async function listAgents(): Promise<Agent[]> {
  return invoke<Agent[]>("agent_list");
}

export async function getAgent(id: string): Promise<Agent | null> {
  return invoke<Agent | null>("agent_get", { id });
}

export async function createAgent(params: {
  name: string;
  description?: string | null;
  provider: string;
  model?: string | null;
  systemPrompt?: string | null;
  mcpConfig?: string | null;
  cwd?: string | null;
}): Promise<string> {
  return invoke<string>("agent_create", {
    name: params.name,
    description: params.description ?? null,
    provider: params.provider,
    model: params.model ?? null,
    systemPrompt: params.systemPrompt ?? null,
    mcpConfig: params.mcpConfig ?? null,
    cwd: params.cwd ?? null,
  });
}

export async function updateAgent(params: {
  id: string;
  name: string;
  description?: string | null;
  provider: string;
  model?: string | null;
  systemPrompt?: string | null;
  mcpConfig?: string | null;
  cwd?: string | null;
}): Promise<void> {
  await invoke("agent_update", {
    id: params.id,
    name: params.name,
    description: params.description ?? null,
    provider: params.provider,
    model: params.model ?? null,
    systemPrompt: params.systemPrompt ?? null,
    mcpConfig: params.mcpConfig ?? null,
    cwd: params.cwd ?? null,
  });
}

export async function deleteAgent(id: string): Promise<void> {
  await invoke("agent_delete", { id });
}

// -- Skill CRUD --

export async function listSkills(): Promise<Skill[]> {
  return invoke<Skill[]>("skill_list");
}

export async function createSkill(params: {
  name: string;
  description?: string | null;
  content?: string | null;
}): Promise<string> {
  return invoke<string>("skill_create", {
    name: params.name,
    description: params.description ?? null,
    content: params.content ?? null,
  });
}

export async function updateSkill(params: {
  id: string;
  name: string;
  description?: string | null;
  content?: string | null;
}): Promise<void> {
  await invoke("skill_update", {
    id: params.id,
    name: params.name,
    description: params.description ?? null,
    content: params.content ?? null,
  });
}

export async function deleteSkill(id: string): Promise<void> {
  await invoke("skill_delete", { id });
}

// -- Skill files (supporting scripts) --

/** `skill_file_list` — list a skill's supporting files (path + content),
 *  ordered by path ASC. Called by the skill-form UI on edit. */
export async function skillFileList(skillId: string): Promise<SkillFile[]> {
  return invoke<SkillFile[]>("skill_file_list", { skillId });
}

/** `skill_file_replace_all` — replace the full file set of a skill
 *  (full-replace-on-save: delete all existing rows then insert the new
 *  set). The backend takes `Vec<(String, String)>` (tuples serialize as
 *  JSON arrays), so the wrapper converts `{path, content}` objects to
 *  `[path, content]` pairs. SKILL.md paths are silently skipped (the body
 *  lives in `skills.content`); binary/oversize files are rejected. */
export async function skillFileReplaceAll(
  skillId: string,
  files: Array<{ path: string; content: string }>,
): Promise<void> {
  await invoke("skill_file_replace_all", {
    skillId,
    files: files.map((f) => [f.path, f.content]),
  });
}

// -- Agent-Skill assignment --

export async function listAgentSkills(agentId: string): Promise<Array<[Skill, boolean]>> {
  return invoke<Array<[Skill, boolean]>>("agent_skills_list", { agentId });
}

export async function addAgentSkill(agentId: string, skillId: string): Promise<void> {
  await invoke("agent_skill_add", { agentId, skillId });
}

export async function removeAgentSkill(agentId: string, skillId: string): Promise<void> {
  await invoke("agent_skill_remove", { agentId, skillId });
}

export async function setAgentSkillEnabled(agentId: string, skillId: string, enabled: boolean): Promise<void> {
  await invoke("agent_skill_set_enabled", { agentId, skillId, enabled });
}

// -- AI-powered agent generation (multi-turn conversation) --

/** Mirrors `agent_gen::GenMessage`. One displayable message in a
 *  generation conversation — the streamed `agent::Message` shape PLUS a
 *  `role` (user / assistant / system / tool) so the frontend can align +
 *  color bubbles without inferring the role from the kind. Both the live
 *  `agent_gen_message` events AND the `agent_gen_history` snapshot
 *  return this type. */
export interface GenMessage {
  role: string;
  kind: string;
  content: string;
  tool: string | null;
}

/** `agent_gen::agent_gen_start` — begin a generation conversation: the
 *  user picks provider + model + describes the agent; the backend runs
 *  the first turn with the agent-builder system prompt + spawns a drain
 *  task that streams `<agent_draft>` back via `agent_gen_message` /
 *  `agent_gen_result` events. Returns the `genId` the frontend keys its
 *  event subscription + state on. Provider + model are pinned here (the
 *  created agent uses them too on 确认). */
export async function agentGenStart(
  provider: Provider,
  model: string | null,
  description: string,
): Promise<string> {
  return invoke<string>("agent_gen_start", { provider, model, description });
}

/** `agent_gen::agent_gen_continue` — send a modification prompt in an
 *  active generation conversation (the 继续修改 path). Resumes the
 *  backend's native session if the first turn yielded a `session_id`,
 *  else replays the in-memory history. Returns immediately; the revised
 *  draft arrives via the same `agent_gen_*` events. */
export async function agentGenContinue(
  genId: string,
  prompt: string,
): Promise<void> {
  await invoke("agent_gen_continue", { genId, prompt });
}

/** `agent_gen::agent_gen_history` — reload the full in-memory message
 *  history for a generation session (used when the panel reopens after a
 *  hide, to recover messages the live listener missed). */
export async function agentGenHistory(genId: string): Promise<GenMessage[]> {
  return invoke<GenMessage[]>("agent_gen_history", { genId });
}

/** `agent_gen::agent_gen_cancel` — drop the in-memory generation session
 *  (取消). No agent is created. Safe to call on an unknown / already-
 *  cancelled id. */
export async function agentGenCancel(genId: string): Promise<void> {
  await invoke("agent_gen_cancel", { genId });
}

// ---------------------------------------------------------------------------
// Automation sub-tab — `automation_*` commands. An automation binds a saved
// agent + a prompt + a trigger (daily HH:MM local, or every-N-minutes); the
// reminders scheduler tick fires due automations, and each run creates a
// first-class conversation (🤖 title prefix) whose messages are the run's
// transcript. See docs/superpowers/specs/2026-10-26-automation-design.md.

/** Mirrors `db::Automation` (snake_case fields at the IPC boundary). */
export interface Automation {
  id: string;
  name: string;
  agent_id: string;
  prompt: string;
  trigger_type: "daily" | "interval";
  /** daily: "HH:MM" (local 24h); interval: integer-minutes string. */
  trigger_config: string;
  enabled: boolean;
  last_run_at: string | null;
  next_fire_at: string | null;
  created_at: string;
  updated_at: string;
}

/** Mirrors `db::AutomationRun`. `conversation_id` links to the run's
 *  transcript (loaded via `automationRunMessages` for the run-detail view). */
export interface AutomationRun {
  id: string;
  automation_id: string;
  conversation_id: string | null;
  source: "schedule" | "manual";
  status: "running" | "completed" | "failed";
  started_at: string;
  finished_at: string | null;
  error: string | null;
  result_summary: string | null;
  created_at: string;
}

export async function automationList(): Promise<Automation[]> {
  return invoke<Automation[]>("automation_list");
}

export async function automationCreate(params: {
  name: string;
  agentId: string;
  prompt: string;
  triggerType: "daily" | "interval";
  triggerConfig: string;
  enabled: boolean;
}): Promise<Automation> {
  return invoke<Automation>("automation_create", {
    name: params.name,
    agentId: params.agentId,
    prompt: params.prompt,
    triggerType: params.triggerType,
    triggerConfig: params.triggerConfig,
    enabled: params.enabled,
  });
}

export async function automationUpdate(params: {
  id: string;
  name: string;
  agentId: string;
  prompt: string;
  triggerType: "daily" | "interval";
  triggerConfig: string;
  enabled: boolean;
}): Promise<Automation> {
  return invoke<Automation>("automation_update", {
    id: params.id,
    name: params.name,
    agentId: params.agentId,
    prompt: params.prompt,
    triggerType: params.triggerType,
    triggerConfig: params.triggerConfig,
    enabled: params.enabled,
  });
}

export async function automationDelete(id: string): Promise<void> {
  await invoke("automation_delete", { id });
}

export async function automationSetEnabled(
  id: string,
  enabled: boolean,
): Promise<Automation> {
  return invoke<Automation>("automation_set_enabled", { id, enabled });
}

export async function automationRunNow(id: string): Promise<AutomationRun> {
  return invoke<AutomationRun>("automation_run_now", { id });
}

export async function automationRunList(
  automationId: string,
): Promise<AutomationRun[]> {
  return invoke<AutomationRun[]>("automation_run_list", { automationId });
}

export async function automationRunMessages(
  runId: string,
): Promise<StoredMessage[]> {
  return invoke<StoredMessage[]>("automation_run_messages", { runId });
}
