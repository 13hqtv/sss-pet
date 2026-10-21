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
import type { ExecResult, Message } from "./api";

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
