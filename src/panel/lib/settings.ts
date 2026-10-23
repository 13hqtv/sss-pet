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
// Reminders (db::Reminder v2: status, recurrence, snooze, next-fire)
// ---------------------------------------------------------------------------

export type ReminderStatus = "pending" | "active" | "done";
export type ReminderType = "once" | "recurring";
export type IntervalUnit = "minutes" | "hours" | "days" | "weeks";

export interface Reminder {
  id: number;
  text: string;
  status: ReminderStatus;
  reminder_type: ReminderType;
  interval_value: number;
  interval_unit: IntervalUnit;
  next_fire_at: string | null;
  snoozed_until: string | null;
  created_at: string;
  updated_at: string;
}

export async function listReminders(): Promise<Reminder[]> {
  return invoke<Reminder[]>("reminder_list");
}

export async function addReminder(params: {
  text: string;
  reminderType?: ReminderType;
  intervalValue?: number;
  intervalUnit?: IntervalUnit;
  nextFireAt?: string | null;
}): Promise<number> {
  return invoke<number>("reminder_add", {
    text: params.text,
    reminderType: params.reminderType ?? "once",
    intervalValue: params.intervalValue ?? 0,
    intervalUnit: params.intervalUnit ?? "minutes",
    nextFireAt: params.nextFireAt ?? null,
  });
}

export async function updateReminder(params: {
  id: number;
  text: string;
  reminderType?: ReminderType;
  intervalValue?: number;
  intervalUnit?: IntervalUnit;
  nextFireAt?: string | null;
}): Promise<void> {
  await invoke("reminder_update", {
    id: params.id,
    text: params.text,
    reminderType: params.reminderType ?? "once",
    intervalValue: params.intervalValue ?? 0,
    intervalUnit: params.intervalUnit ?? "minutes",
    nextFireAt: params.nextFireAt ?? null,
  });
}

export async function setReminderStatus(id: number, status: ReminderStatus): Promise<void> {
  await invoke("reminder_set_status", { id, status });
}

export async function snoozeReminder(id: number, snoozedUntil: string): Promise<void> {
  await invoke("reminder_snooze", { id, snoozedUntil });
}

export async function deleteReminder(id: number): Promise<void> {
  await invoke("reminder_delete", { id });
}

// ---------------------------------------------------------------------------
// KPIs (db::Kpi: id, year, kpi_type, title, details, planned_at, status,
//        created_at, updated_at + db::KpiTarget: year+type → target_count)
// ---------------------------------------------------------------------------

export type KpiStatus = "not_started" | "in_progress" | "completed";

export interface Kpi {
  id: number;
  year: number;
  kpi_type: string;
  title: string;
  details: string | null;
  planned_at: string | null;
  status: KpiStatus;
  created_at: string;
  updated_at: string;
}

export interface KpiTarget {
  id: number;
  year: number;
  kpi_type: string;
  target_count: number;
  created_at: string;
  updated_at: string;
}

export async function listKpis(): Promise<Kpi[]> {
  return invoke<Kpi[]>("kpi_list");
}

export async function addKpi(params: {
  year: number;
  kpiType: string;
  title: string;
  details?: string | null;
  plannedAt?: string | null;
}): Promise<number> {
  return invoke<number>("kpi_add", {
    year: params.year,
    kpiType: params.kpiType,
    title: params.title,
    details: params.details ?? null,
    plannedAt: params.plannedAt ?? null,
  });
}

export async function updateKpi(params: {
  id: number;
  year: number;
  kpiType: string;
  title: string;
  details?: string | null;
  plannedAt?: string | null;
}): Promise<void> {
  await invoke("kpi_update", {
    id: params.id,
    year: params.year,
    kpiType: params.kpiType,
    title: params.title,
    details: params.details ?? null,
    plannedAt: params.plannedAt ?? null,
  });
}

export async function setKpiStatus(id: number, status: KpiStatus): Promise<void> {
  await invoke("kpi_set_status", { id, status });
}

export async function deleteKpi(id: number): Promise<void> {
  await invoke("kpi_delete", { id });
}

export async function listKpiTargets(year: number): Promise<KpiTarget[]> {
  return invoke<KpiTarget[]>("kpi_target_list", { year });
}

export async function upsertKpiTarget(params: {
  year: number;
  kpiType: string;
  targetCount: number;
}): Promise<void> {
  await invoke("kpi_target_upsert", {
    year: params.year,
    kpiType: params.kpiType,
    targetCount: params.targetCount,
  });
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
  /** Roster of KPI types for the KPI tab. Configured as comma-separated
   * text in SettingsView, same pattern as responsible_persons. */
  kpi_types: string[];
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
