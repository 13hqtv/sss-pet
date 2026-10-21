// Task 16: thin `invoke` wrappers for the todos / reminders / settings
// / dismiss commands. Mirrors the pattern of `api.ts` (Task 15): one
// wrapper per Tauri command, with typed arg names matching the Rust
// function's snake_case parameters (Tauri deserializes via serde
// which keeps field names as-is). Keeps the Svelte views free of IPC
// boilerplate so they read as declarative business logic.

import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// Todos (db::Todo: id, text, done, created_at)
// ---------------------------------------------------------------------------

export interface Todo {
  id: number;
  text: string;
  done: boolean;
  created_at: string;
}

export async function listTodos(): Promise<Todo[]> {
  return invoke<Todo[]>("todo_list");
}

export async function addTodo(text: string): Promise<number> {
  return invoke<number>("todo_add", { text });
}

export async function toggleTodo(id: number): Promise<void> {
  await invoke("todo_toggle", { id });
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
