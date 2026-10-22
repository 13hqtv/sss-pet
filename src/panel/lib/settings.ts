// Task 16: thin `invoke` wrappers for the todos / reminders / settings
// / dismiss commands. Mirrors the pattern of `api.ts` (Task 15): one
// wrapper per Tauri command, with typed arg names matching the Rust
// function's snake_case parameters (Tauri deserializes via serde
// which keeps field names as-is). Keeps the Svelte views free of IPC
// boilerplate so they read as declarative business logic.

import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// Todos (db::Todo: id, text, status, priority, assignee, start_at,
//        end_at, notes, created_at)
// ---------------------------------------------------------------------------

export type TodoStatus = "pending" | "in_progress" | "completed" | "cancelled";
export type TodoPriority = "high" | "medium" | "low";

export interface Todo {
  id: number;
  text: string;
  status: TodoStatus;
  priority: TodoPriority;
  assignee: string | null;
  start_at: string | null;
  end_at: string | null;
  notes: string | null;
  created_at: string;
}

export async function listTodos(): Promise<Todo[]> {
  return invoke<Todo[]>("todo_list");
}

export async function addTodo(params: {
  text: string;
  priority?: TodoPriority;
  assignee?: string | null;
  startAt?: string | null;
  endAt?: string | null;
  notes?: string | null;
}): Promise<number> {
  return invoke<number>("todo_add", {
    text: params.text,
    priority: params.priority ?? null,
    assignee: params.assignee ?? null,
    startAt: params.startAt ?? null,
    endAt: params.endAt ?? null,
    notes: params.notes ?? null,
  });
}

export async function updateTodo(params: {
  id: number;
  text: string;
  priority?: TodoPriority;
  assignee?: string | null;
  startAt?: string | null;
  endAt?: string | null;
  notes?: string | null;
}): Promise<void> {
  await invoke("todo_update", {
    id: params.id,
    text: params.text,
    priority: params.priority ?? null,
    assignee: params.assignee ?? null,
    startAt: params.startAt ?? null,
    endAt: params.endAt ?? null,
    notes: params.notes ?? null,
  });
}

export async function setTodoStatus(id: number, status: TodoStatus): Promise<void> {
  await invoke("todo_set_status", { id, status });
}

export async function deleteTodo(id: number): Promise<void> {
  await invoke("todo_delete", { id });
}

// ---------------------------------------------------------------------------
// Reminders (db::Reminder: id, text, due_at, fired)
// ---------------------------------------------------------------------------

export interface Reminder {
  id: number;
  text: string;
  /** RFC3339 UTC datetime string. */
  due_at: string;
  fired: boolean;
}

export async function listReminders(): Promise<Reminder[]> {
  return invoke<Reminder[]>("reminder_list");
}

export async function addReminder(text: string, dueAt: string): Promise<number> {
  return invoke<number>("reminder_add", { text, dueAt });
}

export async function deleteReminder(id: number): Promise<void> {
  await invoke("reminder_delete", { id });
}

// ---------------------------------------------------------------------------
// Settings (config::Settings: follow_activity_min_secs, theme, focus,
//                       default_provider, working_dir)
// ---------------------------------------------------------------------------

export type Theme = "system" | "light" | "dark";
export type Provider = "claude" | "codex" | "opencode";

export interface FocusCfg {
  enabled: boolean;
  /** Optional (start, end) time window, e.g. ("22:00", "07:00"). */
  window: [string, string] | null;
}

export interface Settings {
  follow_activity_min_secs: number;
  theme: string;
  focus: FocusCfg;
  default_provider: string;
  working_dir: string | null;
  /** Per-agent config (Task 5: each agent's `default_model`, probed
   * + picked in SettingsView, pre-filled in the new-conv dialog).
   * Mirrors `config::ProvidersCfg`. `default_model: null` = "use the
   * agent's own default model". */
  providers: ProvidersCfg;
  /** Roster of responsible persons for the 代办 tab. Configured as
   * comma-separated text in SettingsView. */
  responsible_persons: string[];
}

/** Mirrors `config::ProviderCfg` — the per-agent config (Task 5:
 * `default_model` only for now; future fields extend this without a
 * breaking IPC shape change). */
export interface ProviderCfg {
  default_model: string | null;
}

/** Mirrors `config::ProvidersCfg` — one section per whitelisted agent
 * (claude / codex / opencode). `serde(default)` on the Rust side keeps
 * old `config.toml` files (no `[providers.<agent>]` section) loading
 * as `ProviderCfg::default()` (`default_model: null`). */
export interface ProvidersCfg {
  claude: ProviderCfg;
  codex: ProviderCfg;
  opencode: ProviderCfg;
}

/** Read the default model for `provider` from the loaded settings
 * (one of `claude` / `codex` / `opencode`). Returns `null` for
 * unknown providers or unset defaults. Used by the new-conv dialog
 * to pre-fill the model field from the per-agent default. */
export function defaultModelFor(
  settings: Settings,
  provider: Provider,
): string | null {
  return settings.providers[provider]?.default_model ?? null;
}

export async function getSettings(): Promise<Settings> {
  return invoke<Settings>("settings_get");
}

/** Save settings + immediately apply FocusMode (勿扰) to the brain. */
export async function saveSettings(settings: Settings): Promise<void> {
  await invoke("settings_set_with_focus", { settings });
}

// ---------------------------------------------------------------------------
// Pet alert dismiss (for Pet.svelte click-when-NeedsYou)
// ---------------------------------------------------------------------------

export async function dismissPetAlert(): Promise<void> {
  await invoke("dismiss_pet_alert");
}

// ---------------------------------------------------------------------------
// Auto-start (tauri-plugin-autostart — OS registry is the source of truth)
// ---------------------------------------------------------------------------

/** Query whether auto-start is enabled. The OS registry is the source of
 * truth, not config.toml. Called by SettingsView on mount. */
export async function getAutostart(): Promise<boolean> {
  return invoke<boolean>("get_autostart");
}

/** Enable or disable auto-start. Takes effect immediately — no save button
 * needed for this setting. */
export async function setAutostart(enabled: boolean): Promise<void> {
  await invoke("set_autostart", { enabled });
}
