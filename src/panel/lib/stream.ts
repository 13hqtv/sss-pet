// Task 15: Tauri event subscription for `chat_message` / `chat_result`
// / `chat_error`, routed to the active conversation by `conv_id`.
//
// `chat_send` returns immediately; the spawned drain task in
// `chat::run_chat_turn` emits these events as the backend streams +
// when the turn completes / when a save fails. The frontend's
// `ChatView` mounts this router once + dispatches by `conv_id` to the
// right conversation's live message buffer (so the user sees the
// stream even if they switched conversations mid-stream — the messages
// land in the right conversation).
//
// Review Focus #3 (survives-hide): if the panel is hidden mid-stream
// (no listener), the emit is a no-op — but the backend keeps running
// + persists the full message stream + result to the db via
// `save_turn_record`. Re-selecting the conversation calls
// `load_history`, which returns the persisted stream (the messages
// are NOT lost — they were captured + persisted regardless of the
// frontend's listener state).

import { listen } from "@tauri-apps/api/event";
import type { ExecResult, GenMessage, Message } from "./api";

// ---------------------------------------------------------------------------
// Event payload shapes (mirror the `json!({...})` payloads in chat.rs)
// ---------------------------------------------------------------------------

/** `chat_message` payload — one streamed `Message` from the backend. */
export interface ChatMessageEvent {
  conv_id: string;
  message: Message;
}

/** `chat_result` payload — the terminal `ExecResult` for the turn. */
export interface ChatResultEvent {
  conv_id: string;
  result: ExecResult;
}

/** `chat_error` payload — a non-fatal warning surfaced when
 * `save_turn_record` fails (disk full / FS error / db lock poisoned).
 * The turn DID complete in-memory (`chat_result` is still emitted
 * afterward), but the messages + agent-native `session_id` won't
 * survive a restart. Emitted by `chat::run_chat_turn`'s drain task
 * BEFORE `chat_result` (save runs before emit). The frontend surfaces
 * this as a warning banner + an inline system bubble so the user
 * learns the turn was NOT persisted (final-review fix: no failure is
 * invisible). */
export interface ChatErrorEvent {
  conv_id: string;
  message: string;
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/** Callbacks the `ChatView` registers per-event. The `conv_id` is
 * already extracted by the router so the callback only handles its own
 * conversation's message / result / error. */
export interface StreamHandlers {
  onMessage: (convId: string, message: Message) => void;
  onResult: (convId: string, result: ExecResult) => void;
  onError: (convId: string, message: string) => void;
}

/**
 * Subscribe to `chat_message` + `chat_result` for the lifetime of the
 * panel, routing each event to the matching `handlers` callback by
 * `conv_id`. Returns an `unlisten` fn (call on component destroy —
 * Svelte 5's `$effect` cleanup is the canonical place).
 *
 * Multiple conversations can be streaming concurrently (the user sent
 * a prompt, then switched to another conversation + sent another);
 * each event is routed to its own conversation's handler — the
 * `ChatView` keeps per-`conv_id` message buffers.
 */
export async function subscribeChatStream(
  handlers: StreamHandlers,
): Promise<() => void> {
  const unsubs: Array<() => void> = [];

  // `chat_message` — append to the conv_id's live message buffer.
  const unlistenMessage = await listen<ChatMessageEvent>(
    "chat_message",
    (e) => {
      const { conv_id, message } = e.payload;
      handlers.onMessage(conv_id, message);
    },
  );
  unsubs.push(unlistenMessage);

  // `chat_result` — mark the conv_id's turn done + surface the result.
  const unlistenResult = await listen<ChatResultEvent>(
    "chat_result",
    (e) => {
      const { conv_id, result } = e.payload;
      handlers.onResult(conv_id, result);
    },
  );
  unsubs.push(unlistenResult);

  // `chat_error` — a `save_turn_record` failure (the turn completed
  // in-memory but won't be persisted). Surfaced by the drain task
  // BEFORE `chat_result` (save runs before emit). Final-review fix:
  // the frontend learns the save failed instead of silently dropping
  // the turn on restart.
  const unlistenError = await listen<ChatErrorEvent>(
    "chat_error",
    (e) => {
      const { conv_id, message } = e.payload;
      handlers.onError(conv_id, message);
    },
  );
  unsubs.push(unlistenError);

  // Single cleanup fn — calls both unlistens. Best-effort: an
  // unlisten that throws (e.g. webview already torn down) doesn't
  // block the other.
  return () => {
    for (const u of unsubs) {
      try {
        u();
      } catch {
        // Swallow — the webview may already be gone (panel hidden +
        // torn down on app exit). The backend's stream-drain task
        // keeps running regardless (it doesn't depend on a
        // listener — Review Focus #3).
      }
    }
  };
}

// ---------------------------------------------------------------------------
// Agent generation stream — `agent_gen_message` / `agent_gen_result` /
// `agent_gen_error`, routed to the active generation conversation by
// `gen_id`. Mirrors the chat stream router but for the in-memory
// generation flow in AgentsView (no DB persistence — the gen session
// lives in the backend's `GenState` map, recovered via `agent_gen_history`
// if the panel hides mid-stream).
// ---------------------------------------------------------------------------

/** `agent_gen_message` payload — one streamed `GenMessage` (role +
 *  kind + content + tool) from the backend's generation drain task. */
export interface AgentGenMessageEvent {
  gen_id: string;
  message: GenMessage;
}

/** `agent_gen_result` payload — the terminal `ExecResult` for one
 *  generation turn (status completed / failed + the native `session_id`
 *  the next turn will resume). */
export interface AgentGenResultEvent {
  gen_id: string;
  result: ExecResult;
}

/** `agent_gen_error` payload — reserved for future save-failure parity
 *  with chat (the in-memory generation flow currently doesn't emit this,
 *  but subscribing keeps the frontend resilient if a future revision adds
 *  a persistence hook). */
export interface AgentGenErrorEvent {
  gen_id: string;
  message: string;
}

/** Callbacks the AgentsView generation panel registers per-event. The
 *  `gen_id` is already extracted by the router so the callback only
 *  handles its own conversation's message / result / error. */
export interface AgentGenStreamHandlers {
  onMessage: (genId: string, message: GenMessage) => void;
  onResult: (genId: string, result: ExecResult) => void;
  onError: (genId: string, message: string) => void;
}

/**
 * Subscribe to `agent_gen_message` + `agent_gen_result` +
 * `agent_gen_error` for the lifetime of the panel, routing each event to
 * the matching `handlers` callback by `gen_id`. Returns an `unlisten` fn
 * (call on component destroy — Svelte 5's `$effect` cleanup is the
 * canonical place). Mirrors `subscribeChatStream` for the generation
 * conversation flow.
 */
export async function subscribeAgentGenStream(
  handlers: AgentGenStreamHandlers,
): Promise<() => void> {
  const unsubs: Array<() => void> = [];

  const unlistenMessage = await listen<AgentGenMessageEvent>(
    "agent_gen_message",
    (e) => {
      const { gen_id, message } = e.payload;
      handlers.onMessage(gen_id, message);
    },
  );
  unsubs.push(unlistenMessage);

  const unlistenResult = await listen<AgentGenResultEvent>(
    "agent_gen_result",
    (e) => {
      const { gen_id, result } = e.payload;
      handlers.onResult(gen_id, result);
    },
  );
  unsubs.push(unlistenResult);

  const unlistenError = await listen<AgentGenErrorEvent>(
    "agent_gen_error",
    (e) => {
      const { gen_id, message } = e.payload;
      handlers.onError(gen_id, message);
    },
  );
  unsubs.push(unlistenError);

  return () => {
    for (const u of unsubs) {
      try {
        u();
      } catch {
        // Swallow — the webview may already be gone (panel hidden +
        // torn down on app exit). The backend's generation drain task
        // keeps running regardless (it buffers into the in-memory
        // GenState + recovers via `agent_gen_history`).
      }
    }
  };
}

// ---------------------------------------------------------------------------
// Automation run stream — `automation_run_message` / `automation_run_result`
// / `automation_run_error` / `automation_runs_changed`. Emitted by
// `automation::automation_dispatch_async`'s emit accumulator (which re-emits
// the underlying `chat_*` events as `automation_run_*` + finalizes the run
// row on result/error). The automation sub-tab subscribes for the lifetime
// of the panel: `onMessage` feeds the live-progress view, `onRunsChanged`
// refreshes the history list, `onResult`/`onError` close out a run.
//
// Event payloads use camelCase keys (the backend emits `json!({"runId": …})`
// — string-literal keys are preserved verbatim).

export interface AutomationRunMessageEvent {
  runId: string;
  automationId: string;
  message: Message;
}
export interface AutomationRunResultEvent {
  runId: string;
  automationId: string;
  result: ExecResult;
}
export interface AutomationRunErrorEvent {
  runId: string;
  automationId: string;
  message: string;
}
export interface AutomationRunsChangedEvent {
  automationId: string;
}

export interface AutomationStreamHandlers {
  onMessage?: (runId: string, automationId: string, message: Message) => void;
  onResult?: (runId: string, automationId: string, result: ExecResult) => void;
  onError?: (runId: string, automationId: string, message: string) => void;
  onRunsChanged?: (automationId: string) => void;
}

/** Subscribe to the automation run events. Returns an `unlisten` fn (call
 *  on component destroy — Svelte 5's `$effect` cleanup is the canonical
 *  place). Handlers are optional — subscribe only to the events you need. */
export async function subscribeAutomationRunStream(
  handlers: AutomationStreamHandlers,
): Promise<() => void> {
  const unsubs: Array<() => void> = [];

  if (handlers.onMessage) {
    const h = handlers.onMessage;
    unsubs.push(
      await listen<AutomationRunMessageEvent>("automation_run_message", (e) => {
        const { runId, automationId, message } = e.payload;
        h(runId, automationId, message);
      }),
    );
  }
  if (handlers.onResult) {
    const h = handlers.onResult;
    unsubs.push(
      await listen<AutomationRunResultEvent>("automation_run_result", (e) => {
        const { runId, automationId, result } = e.payload;
        h(runId, automationId, result);
      }),
    );
  }
  if (handlers.onError) {
    const h = handlers.onError;
    unsubs.push(
      await listen<AutomationRunErrorEvent>("automation_run_error", (e) => {
        const { runId, automationId, message } = e.payload;
        h(runId, automationId, message);
      }),
    );
  }
  if (handlers.onRunsChanged) {
    const h = handlers.onRunsChanged;
    unsubs.push(
      await listen<AutomationRunsChangedEvent>("automation_runs_changed", (e) => {
        h(e.payload.automationId);
      }),
    );
  }

  return () => {
    for (const u of unsubs) {
      try {
        u();
      } catch {
        // Swallow — the webview may already be gone.
      }
    }
  };
}
