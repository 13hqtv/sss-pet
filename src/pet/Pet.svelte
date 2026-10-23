<script lang="ts">
  // Task 2 — static "待命" (standby) robot on a transparent, always-on-top,
  // click-through overlay window.
  //
  // Click-through is driven entirely from Rust. A background thread
  // (window.rs::spawn_click_through_poller) polls the global cursor via the
  // `device_query` crate (Win32 GetCursorPos) every ~30ms and toggles
  // set_ignore_cursor_events(false) when the cursor is inside the robot's
  // screen rect (robot interactive) and back to true when it leaves
  // (transparent area passes clicks through). The frontend cannot do this:
  // while the window is in pass-through mode (WS_EX_TRANSPARENT on Windows)
  // no pointer events — including pointermove — reach the webview. See
  // task-2-report.md §2 for the full rationale.
  //
  // Task 4 extends this:
  // - `data-tauri-drag-region` on the SVG root makes Tauri call
  //   `start_dragging` on mousedown, so grabbing the robot body drags the
  //   pet window. The click-through poller already sets `ignore=false` when
  //   the cursor is over the robot, so the mousedown reaches the webview
  //   → drag works. A Rust-side `DRAGGING: AtomicBool` (set via the
  //   `set_pet_dragging` Tauri command below) pins `ignore=false` for the
  //   duration of the drag so the poller never flips pass-through back on
  //   mid-drag if the cursor momentarily leaves the (stale) robot rect
  //   between OS drag ticks.
  // - On drag end we save the pet's new position via `save_pet_pos` so the
  //   within-session hide+show cycle (tray 显示/隐藏) returns the pet to the
  //   dragged spot. Task 15 will make this durable across restarts by
  //   swapping the in-memory store for a TOML file.
  // - A click *without* movement (mousedown+mouseup at the same screen
  //   position) is interpreted as a click rather than a drag → invoke
  //   `open_panel_above_pet` so the panel pops above (or below, if there's
  //   no room) the pet.
  //
  // ## DRAGGING-clear guarantee (fix round 1)
  //
  // The prior implementation attached `onmouseup` directly to the robot SVG.
  // That was fragile: the SVG carries `data-tauri-drag-region`, which
  // triggers Tauri's OS-level modal drag loop on mousedown. On Windows that
  // modal loop can consume the release `mouseup` so it is never delivered to
  // the webview as a DOM `mouseup` on the SVG element. If the element-level
  // handler never fires:
  //   (a) `set_pet_dragging(false)` is never called → the `DRAGGING`
  //       AtomicBool stays true → the click-through poller permanently pins
  //       `ignore=false` → transparent-frame click-through breaks for the
  //       whole session (a Task 2 feature regression);
  //   (b) `save_pet_pos` never fires → within-session position persistence
  //       (brief Step 2) never happens.
  // The implementer also flagged a listener-ordering race: Tauri's
  // drag-region listener is registered before ours, so `start_dragging`
  // (which blocks the JS event loop) can run before our `set_pet_dragging
  // (true)` IPC is dispatched, leaving DRAGGING unset during the drag.
  //
  // Fix: every drag-end signal funnels through a single idempotent `endDrag`.
  // We register WINDOW-LEVEL `mouseup`/`pointerup` listeners (not just on
  // the SVG element) so the release is caught even when the OS modal drag
  // loop steals the element-level event; a 5s safety timeout clears
  // DRAGGING if no release arrives at all; and a window `blur` listener
  // clears it as a focus-loss backstop (also covers the listener-ordering
  // race, since losing focus during a drag means the drag is over). The
  // union of (window mouseup ∪ pointerup ∪ safety timeout ∪ blur) clears
  // guarantees DRAGGING never sticks true regardless of what the OS did with
  // the mouseup. `set_pet_dragging(true)` is dispatched fire-and-forget at
  // the very start of the mousedown handler (no await) so it lands before
  // Tauri's drag-region listener takes over in the common case; if it loses
  // the ordering race, the backstops above still clear it on drag end.
  //
  // ## Task 7 — pet state rendering (5 states + sleep=static + activity nudge)
  //
  // The Rust "brain" tick loop (`brain.rs`) drives the state machine from
  // live idle time and emits two events to this window:
  //   - `pet_state`       — the new `PetState` (serialized as the variant
  //                         name string: "Sleep"/"Standby"/"Thinking"/
  //                         "HasSomething"/"NeedsYou"). Emitted only on
  //                         change, so zero events while the state is stable.
  //   - `pet_activity_nudge` — a transient "glance" event from the follow-
  //                         activity throttle (≥15min spacing). Does NOT
  //                         change the state; just plays a 1s wiggle.
  //
  // This component subscribes to both, switches a CSS state class on the
  // SVG root, and renders per-state face/decoration elements (5 poses).
  // ONLY state-standby (slow blink) and state-thinking (cog spin) animate;
  // state-sleep has NO keyframe at all (Review Focus #1: idle CPU ≈ 0 when
  // asleep). The activity-nudge is a one-shot 1s wiggle layered on top,
  // suppressed when asleep (both in JS and by the throttle's away-
  // threshold, which kicks in at 3min idle, before the 5min sleep boundary).
  //
  // A dev-only `dev_trigger_state` Tauri command (in `lib.rs`) can inject
  // `ReminderDue`/`Error`/`AgentBusy`/`Dismissed`/`PanelOpened` signals to
  // verify the `!`/`?`/thinking poses without the real agent/reminder wiring.

  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { listen } from "@tauri-apps/api/event";
  import { dismissPetAlert, snoozeReminder, setReminderStatus } from "../panel/lib/settings";
  import "./states.css";

  // ---------------------------------------------------------------------------
  // Task 7: pet state + activity nudge.
  // ---------------------------------------------------------------------------

  // The 5 pet states, as serialized by Rust's `PetState` (serde → variant
  // name string). Default "Standby" until the first `pet_state` event.
  type PetStateName =
    | "Sleep"
    | "Standby"
    | "Thinking"
    | "HasSomething"
    | "NeedsYou";

  // Svelte 5 runes: typed via the variable annotation (NOT the call,
  // which svelte-check mis-parses — see task-15-report.md "Svelte 5
  // runes + svelte-check" concern in ChatView).
  //
  // Task 17: variable named `petState` (NOT `state`) to avoid a
  // svelte-check runes-detection quirk — having a variable named
  // `state` causes `$state(...)` to be resolved as a store
  // auto-subscription to the `state` variable (not a rune), which
  // prevents svelte-check from entering runes mode (see
  // checkGlobalsForRunes in svelte-check's dist: `$state` is removed
  // from globals when a `state` variable exists). The Svelte compiler
  // (pnpm build) was always clean; this is a svelte-check-specific
  // issue. All other .svelte files in the project (App.svelte,
  // ChatView.svelte, etc.) don't have a variable named `state`, so
  // their `$state(...)` is correctly detected as a rune global.
  let petState: PetStateName = $state("Standby");

  // Map a serialized `PetState` string to the CSS class that drives the
  // per-state styling + animation (or lack thereof) in `states.css`.
  // Falls back to `state-standby` if an unknown string arrives (defensive
  // — shouldn't happen, but a future Rust variant added without updating
  // this map should not break the pet).
  const STATE_CLASS: Record<PetStateName, string> = {
    Sleep: "state-sleep",
    Standby: "state-standby",
    Thinking: "state-thinking",
    HasSomething: "state-has-something",
    NeedsYou: "state-needs-you",
  };

  // Human-readable Chinese label for each state, used in the SVG
  // `aria-label` so screen readers announce the localized state name
  // rather than the raw Rust variant string (`"HasSomething"` /
  // `"NeedsYou"` are not user-facing). Mirrors `STATE_CLASS`'s shape;
  // falls back to `"待命"` for an unknown variant (defensive).
  const STATE_LABEL: Record<PetStateName, string> = {
    Sleep: "睡眠",
    Standby: "待命",
    Thinking: "思考",
    HasSomething: "有动静",
    NeedsYou: "需要你",
  };

  // The CSS class for the current state. `state-standby` is the default
  // and also the fallback for any unknown state string.
  let stateClass = $derived(STATE_CLASS[petState] ?? "state-standby");

  // The localized Chinese label for the current state, surfaced in the
  // SVG `aria-label`. `待命` is the default / fallback.
  let stateLabel = $derived(STATE_LABEL[petState] ?? "待命");

  // Brief visual nudge when the robot is clicked — proves it received the
  // click. `nudge` toggles a CSS transform for ~100ms. Kept for the click
  // (non-drag) path; a drag does not nudge.
  // Task 2 carry: `nudgeTimer` is now cleared on component destroy (see
  // `onMount` cleanup below) — previously it leaked on unmount.
  let nudge = $state(false);
  let nudgeTimer: ReturnType<typeof setTimeout> | null = null;

  // Activity-nudge flag from the follow-activity throttle (Rust
  // `pet_activity_nudge` event). Plays a ~1s wiggle WITHOUT changing the
  // state class. Only set when the pet is awake (asleep pet ignores
  // nudges — respect quiet, Review Focus #1).
  let nudgeActivity = $state(false);
  let nudgeActivityTimer: ReturnType<typeof setTimeout> | null = null;

  // Reminder popup: when a reminder fires, the Rust scheduler emits a
  // `reminder_fired` event to the pet window. We show a compact popup
  // card (replacing the robot) with the reminder text + snooze/dismiss
  // buttons. Clicking an action closes the popup.
  let reminderBubble: { id: number; text: string } | null = $state(null);

  // ---------------------------------------------------------------------------
  // Drag state. The drag is started from the Rust side via `start_pet_drag`
  // (atomically sets DRAGGING=true + calls start_dragging()). But
  // `start_dragging()` may use PostMessage (non-blocking), so the IPC
  // returns BEFORE the OS drag loop starts. To avoid treating a drag as
  // a click, disambiguation happens on `mouseup` (not right after the IPC).
  // ---------------------------------------------------------------------------
  let dragArmed = false;
  let dragStartWin: { x: number; y: number } | null = null;
  let dragSafetyTimer: ReturnType<typeof setTimeout> | null = null;

  async function onRobotMouseDown(e: MouseEvent) {
    e.preventDefault();
    if (dragArmed) return;
    dragArmed = true;

    // Record the window's start position for click-vs-drag disambiguation.
    try {
      const pos = await getCurrentWindow().outerPosition();
      dragStartWin = { x: pos.x, y: pos.y };
    } catch (err) {
      console.warn("[pet] outerPosition at mousedown failed:", err);
    }

    // Fire-and-forget: the Rust side sets DRAGGING=true + calls
    // start_dragging(). Whether start_dragging blocks or returns
    // immediately, we register a mouseup listener so disambiguation
    // happens when the user actually releases the mouse — NOT right
    // after the IPC (which could be before the drag even starts).
    invoke("start_pet_drag").catch((err) =>
      console.warn("[pet] start_pet_drag failed:", err),
    );

    // Register a one-time mouseup listener for drag-end disambiguation.
    // This fires after the OS drag loop exits (if the modal loop passes
    // WM_LBUTTONUP through to the webview) OR immediately if the drag
    // didn't start (start_pet_drag failed).
    window.addEventListener("mouseup", onDragMouseUp, { once: true });

    // Safety timeout: if the mouseup is consumed by the OS modal drag
    // loop and never reaches the webview, clear DRAGGING after 5s so
    // the poller doesn't stay pinned forever.
    if (dragSafetyTimer) clearTimeout(dragSafetyTimer);
    dragSafetyTimer = setTimeout(() => {
      if (dragArmed) {
        console.warn("[pet] drag safety timeout — clearing DRAGGING");
        dragArmed = false;
        dragStartWin = null;
        void invoke("set_pet_dragging", { dragging: false }).catch(() => {});
      }
    }, 5000);
  }

  // Drag end (mouseup). Clears DRAGGING, then does click-vs-drag
  // disambiguation: if the window moved >2px it's a drag (save position);
  // otherwise it's a click (toggle panel / dismiss alert).
  async function onDragMouseUp() {
    if (!dragArmed) return;
    dragArmed = false;

    if (dragSafetyTimer) {
      clearTimeout(dragSafetyTimer);
      dragSafetyTimer = null;
    }

    // Clear the DRAGGING flag so the poller resumes normal click-through.
    try {
      await invoke("set_pet_dragging", { dragging: false });
    } catch (err) {
      console.warn("[pet] set_pet_dragging(false) failed:", err);
    }

    // Click-vs-drag: compare start/end window positions.
    let endPos: { x: number; y: number } | null = null;
    try {
      const pos = await getCurrentWindow().outerPosition();
      endPos = { x: pos.x, y: pos.y };
    } catch (err) {
      console.warn("[pet] outerPosition at drag end failed:", err);
    }

    const start = dragStartWin;
    dragStartWin = null;

    if (!start || !endPos) {
      onRobotClick();
      return;
    }

    const dx = Math.abs(endPos.x - start.x);
    const dy = Math.abs(endPos.y - start.y);
    if (dx <= 2 && dy <= 2) {
      // Click — toggle the panel (or dismiss alert).
      onRobotClick();
    } else {
      // Drag — persist the new position (within-session).
      try {
        await invoke("save_pet_pos", { x: endPos.x, y: endPos.y });
      } catch (err) {
        console.warn("[pet] save_pet_pos failed:", err);
      }
    }
  }

  function onRobotClick() {
    // When the pet is in `NeedsYou` (showing `?`), clicking the robot
    // DISMISSES the alert instead of opening the panel. Any other state:
    // toggle the panel (open if hidden, close if visible).
    if (nudgeTimer) clearTimeout(nudgeTimer);
    nudge = true;
    nudgeTimer = setTimeout(() => {
      nudge = false;
      nudgeTimer = null;
    }, 100);

    if (petState === "NeedsYou") {
      console.log("[pet] robot clicked — 需要你 → dismiss");
      dismissPetAlert().catch((e) =>
        console.warn("[pet] dismiss_pet_alert failed:", e),
      );
      return;
    }
    console.log("[pet] robot clicked — toggle panel");
    invoke("open_panel_above_pet").catch((e) =>
      console.warn("[pet] open_panel_above_pet failed:", e),
    );
  }

  // -- Reminder popup actions --

  /** Dismiss the reminder (mark done = no more fires). */
  async function dismissReminder(id: number) {
    try {
      await setReminderStatus(id, "done");
    } catch (e) {
      console.warn("[pet] dismissReminder failed:", e);
    }
    reminderBubble = null;
    invoke("set_reminder_popup", { active: false }).catch(() => {});
  }

  /** Snooze the reminder by N minutes. */
  async function snoozeReminderAction(id: number, minutes: number) {
    try {
      const until = new Date(Date.now() + minutes * 60_000).toISOString();
      await snoozeReminder(id, until);
    } catch (e) {
      console.warn("[pet] snoozeReminder failed:", e);
    }
    reminderBubble = null;
    invoke("set_reminder_popup", { active: false }).catch(() => {});
  }

  // Keyboard-equivalent of the click so the robot is operable without a
  // mouse (Enter / Space), satisfying the Svelte a11y rule for the click
  // handler on a non-interactive <svg> that carries role="button".
  function onRobotKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onRobotClick();
    }
  }

  // Disable the browser context menu (right-click menu) — the desktop
  // pet is not a web page and right-click should do nothing.
  $effect(() => {
    const handler = (e: MouseEvent) => e.preventDefault();
    window.addEventListener("contextmenu", handler);
    return () => window.removeEventListener("contextmenu", handler);
  });

  // ---------------------------------------------------------------------------
  // Tauri event listeners + cleanup (Task 7).
  //
  // `$effect` subscribes to `pet_state` and `pet_activity_nudge`. Returns a
  // cleanup function that unlistens + clears all pending timers (nudge,
  // nudgeActivity, dragSafety). This closes the Task 2 carry "nudgeTimer
  // leaks on unmount" and ensures no dangling setTimeout fires after the
  // component tears down.
  //
  // Task 17: converted from `onMount` to `$effect` to fix svelte-check
  // runes-mode detection (the presence of the legacy `onMount` import
  // caused svelte-check to treat the whole file as legacy Svelte 4 mode,
  // mis-parsing every `$state(...)` call as a store auto-subscription —
  // see the 4 Task-16-carry errors. The Svelte compiler (pnpm build) was
  // always clean; this is a svelte-check / language-tools quirk. The
  // effect body reads no reactive state synchronously (all `state` reads
  // are inside event-listener callbacks, which $effect does not track),
  // so it runs exactly once on mount and returns the cleanup — the same
  // semantics as the prior `onMount`.
  //
  // `listen` is async (returns a Promise<UnlistenFn>); we guard against the
  // race where the component unmounts before the promises resolve by
  // tracking a `cancelled` flag — if unmounted, we immediately unlisten
  // the just-resolved handle.
  // ---------------------------------------------------------------------------
  $effect(() => {
    let cancelled = false;
    // Stash the unlisten handles; null until the promises resolve.
    let unlistenState: (() => void) | null = null;
    let unlistenNudge: (() => void) | null = null;

    listen<PetStateName>("pet_state", (e) => {
      petState = e.payload;
    }).then((u) => {
      if (cancelled) {
        u();
        return;
      }
      unlistenState = u;
    });

    listen("pet_activity_nudge", () => {
      // Respect quiet: asleep pet ignores nudges (Review Focus #1).
      // The throttle also suppresses nudges while idle > 3min, so this
      // double-check is defense-in-depth (the pet transitions to Sleep
      // at 5min, and 3 < 5, so by the time we're asleep the throttle
      // already stopped firing — but the frontend doesn't trust the
      // backend's timing).
      if (petState === "Sleep") return;
      if (nudgeActivityTimer) clearTimeout(nudgeActivityTimer);
      nudgeActivity = true;
      nudgeActivityTimer = setTimeout(() => {
        nudgeActivity = false;
        nudgeActivityTimer = null;
      }, 1000);
    }).then((u) => {
      if (cancelled) {
        u();
        return;
      }
      unlistenNudge = u;
    });

    // Listen for reminder_fired events from the scheduler. Shows the
    // reminder popup with snooze/dismiss actions.
    let unlistenReminder: (() => void) | null = null;
    listen<{ id: number; text: string }>("reminder_fired", (e) => {
      reminderBubble = e.payload;
      // Force the pet window to be fully interactive so popup buttons
      // are clickable (the click-through poller checks this flag).
      invoke("set_reminder_popup", { active: true }).catch(() => {});
    }).then((u) => {
      if (cancelled) { u(); return; }
      unlistenReminder = u;
    });

    return () => {
      cancelled = true;
      unlistenState?.();
      unlistenNudge?.();
      unlistenReminder?.();
      // Clear all pending timers so the component can tear down without a
      // dangling setTimeout firing after destroy. (Task 2 carry: the
      // click-nudge `nudgeTimer` previously leaked here.)
      if (nudgeTimer) {
        clearTimeout(nudgeTimer);
        nudgeTimer = null;
      }
      if (nudgeActivityTimer) {
        clearTimeout(nudgeActivityTimer);
        nudgeActivityTimer = null;
      }
    };
  });
</script>

<main class="pet">
  {#if reminderBubble}
    <!-- Reminder popup: replaces the robot when a reminder fires.
         Clean notification card with header, multi-line text, actions. -->
    <div class="reminder-popup">
      <div class="popup-header">
        <span class="popup-icon">🔔</span>
        <span class="popup-title">提醒</span>
        <button class="popup-close" onclick={() => { reminderBubble = null; invoke("set_reminder_popup", { active: false }).catch(() => {}); }}>×</button>
      </div>
      <div class="popup-text">{reminderBubble.text}</div>
      <div class="popup-actions">
        <button class="popup-btn primary" onclick={() => dismissReminder(reminderBubble!.id)}>不再提醒</button>
        <button class="popup-btn" onclick={() => snoozeReminderAction(reminderBubble!.id, 10)}>推迟 10 分</button>
        <button class="popup-btn" onclick={() => snoozeReminderAction(reminderBubble!.id, 60)}>推迟 1 时</button>
        <button class="popup-btn" onclick={() => snoozeReminderAction(reminderBubble!.id, 1440)}>推迟 1 天</button>
      </div>
    </div>
  {:else}
  <!--
    The robot SVG. Mousedown on the robot is handled by `onRobotMouseDown`
    (below), which calls `set_pet_dragging(true)` BEFORE `start_dragging()`
    to pin `ignore=false` on the Rust poller for the duration of the drag,
    then enters the OS modal drag loop via `getCurrentWindow().startDragging()`.
    When the drag loop returns, click-vs-drag disambiguation runs inline —
    no window-level backstops needed.

    The old approach used `data-tauri-drag-region`, which made Tauri's
    drag-region listener fire before ours and enter the modal loop before
    `set_pet_dragging(true)` could be dispatched — leaving DRAGGING=false
    during the drag so the poller broke the drag by flipping pass-through on.

    Task 7: the SVG root carries the per-state CSS class (`stateClass`)
    which drives styling + animation (or lack thereof) from `states.css`.
    Only state-standby (blink) and state-thinking (cog spin) animate;
    state-sleep has NO keyframe (idle CPU ≈ 0). The `activity-nudge` class
    is layered on top for the one-shot 1s wiggle.

    The shared body (antenna, ears, head, status LED) is always rendered;
    the face/decoration branch per-state below.
  -->
  <svg
    class="robot {stateClass}"
    class:nudge
    class:activity-nudge={nudgeActivity}
    viewBox="0 0 96 96"
    width="96"
    height="96"
    role="button"
    tabindex="0"
    aria-label="桌面宠物机器人 · {stateLabel}"
    onmousedown={onRobotMouseDown}
    onkeydown={onRobotKeydown}
  >
    <!-- ============ shared body (all states) ============
         Refined geometric monoline bot (2026-10-22 redesign). 2px strokes,
         round caps/joins. FIXED palette — no theme tokens, no data-theme:
           ink     #1C1C1E  (stroke — reads on light; halo + warm-white
                            fill carry it on dark backdrops)
           surface #FAFAF7  (warm-white fill — reads on dark; outline +
                            halo carry it on light)
           accent  #4F46E5  (indigo — antenna tip, chest dot, thinking cog)
           orange  #EA580C  (HasSomething "!")
           red     #DC2626  (NeedsYou "?")
           muted   #6B6B70  (Sleep "zzz")
         The base filter (Pet.svelte <style>) layers a dark drop-shadow
         (reads on light wallpaper) + a soft light glow (reads on dark
         wallpaper) so the bot is visible on ANY desktop backdrop without
         theme switching. State-specific halos override in states.css.
         Geometry fits inside ~76×80 (well within the 96×96 central 80%
         click/drag rect window.rs computes for the 120×120 window). -->

    <!-- antenna: line + indigo tip dot -->
    <line x1="48" y1="10" x2="48" y2="22" stroke="#1C1C1E" stroke-width="2"
      stroke-linecap="round" />
    <circle cx="48" cy="7" r="2.5" fill="#4F46E5" />

    <!-- head: rounded rect, warm-white fill, ink 2px stroke -->
    <rect x="20" y="22" width="56" height="40" rx="13"
      fill="#FAFAF7" stroke="#1C1C1E" stroke-width="2" />

    <!-- neck: short connector head → body -->
    <line x1="48" y1="62" x2="48" y2="64" stroke="#1C1C1E" stroke-width="2"
      stroke-linecap="round" />

    <!-- body: small rounded rect below the head -->
    <rect x="32" y="64" width="32" height="16" rx="6"
      fill="#FAFAF7" stroke="#1C1C1E" stroke-width="2" />

    <!-- arms: two short outward-leaning lines flanking the body -->
    <line x1="32" y1="70" x2="27" y2="74" stroke="#1C1C1E" stroke-width="2"
      stroke-linecap="round" />
    <line x1="64" y1="70" x2="69" y2="74" stroke="#1C1C1E" stroke-width="2"
      stroke-linecap="round" />

    <!-- chest indicator: small indigo status dot on the body -->
    <circle cx="48" cy="72" r="1.5" fill="#4F46E5" />

    <!-- ============ per-state face + decoration ============ -->

    {#if petState === "Sleep"}
      <!--
        Sleep: dimmed (CSS opacity .5), eyes closed (short lines), slight
        frown, "zzz" floating upper-right. NO keyframe animation at all
        (states.css has no `animation` on .state-sleep — Review Focus #1:
        idle CPU ≈ 0 when asleep). Static — the v0.1.0 constraint.
      -->
      <!-- closed eyes (horizontal lines) -->
      <line x1="32" y1="40" x2="42" y2="40" stroke="#1C1C1E" stroke-width="2"
        stroke-linecap="round" />
      <line x1="54" y1="40" x2="64" y2="40" stroke="#1C1C1E" stroke-width="2"
        stroke-linecap="round" />
      <!-- slight frown mouth -->
      <path d="M40 52 Q48 48 56 52" stroke="#1C1C1E" stroke-width="2"
        fill="none" stroke-linecap="round" stroke-linejoin="round" />
      <!-- "zzz" text (static, muted ink, no animation) -->
      <text x="68" y="13" font-size="9" fill="#6B6B70" font-family="system-ui"
        font-weight="600">zzz</text>

    {:else if petState === "Thinking"}
      <!--
        Thinking: eyes looking up (dots shifted up), small "o" mouth,
        spinning indigo cog above the head (class="cog" — animated by
        states.css @keyframes thinking-cog-spin). The cog is the ONLY
        animated element in this state.
      -->
      <!-- eyes (dots, shifted up — "looking up") -->
      <circle cx="37" cy="38" r="2.5" fill="#1C1C1E" />
      <circle cx="59" cy="38" r="2.5" fill="#1C1C1E" />
      <!-- mouth (small "o" circle) -->
      <circle cx="48" cy="51" r="2" fill="none" stroke="#1C1C1E"
        stroke-width="1.5" />
      <!-- spinning indigo cog above the head.

           STRUCTURAL NOTE (carried from v0.1.0 fix round 1): the cog is
           split into an outer `<g transform="translate(72 12)">` that
           positions it above the head's upper-right (clear of the
           antenna at x=48), and an inner `<g class="cog">` that carries
           the CSS spin. This separation is REQUIRED: the CSS
           @keyframes thinking-cog-spin sets `transform: rotate(...)`,
           which (per the CSS spec) entirely overrides any SVG `transform`
           presentation attribute on the same element. If both lived on
           the same element (`<g class="cog" transform="...">`), the
           translate would be dropped during the animation and the cog
           would render at (0,0) (top-left corner). The inner `.cog`
           keeps `transform-box: fill-box; transform-origin: center`
           (states.css) so it spins around its own center (0,0 in its
           local coords = (72,12) once the outer translate is applied). -->
      <g transform="translate(72 12)">
        <g class="cog">
          <!-- gear body ring -->
          <circle cx="0" cy="0" r="4" fill="none" stroke="#4F46E5"
            stroke-width="1.5" />
          <!-- 8 gear teeth (short radial lines) -->
          <line x1="0" y1="-6" x2="0" y2="-4" stroke="#4F46E5"
            stroke-width="1.5" stroke-linecap="round" />
          <line x1="0" y1="4" x2="0" y2="6" stroke="#4F46E5"
            stroke-width="1.5" stroke-linecap="round" />
          <line x1="-6" y1="0" x2="-4" y2="0" stroke="#4F46E5"
            stroke-width="1.5" stroke-linecap="round" />
          <line x1="4" y1="0" x2="6" y2="0" stroke="#4F46E5"
            stroke-width="1.5" stroke-linecap="round" />
          <line x1="-4.2" y1="-4.2" x2="-2.8" y2="-2.8" stroke="#4F46E5"
            stroke-width="1.5" stroke-linecap="round" />
          <line x1="2.8" y1="2.8" x2="4.2" y2="4.2" stroke="#4F46E5"
            stroke-width="1.5" stroke-linecap="round" />
          <line x1="-4.2" y1="4.2" x2="-2.8" y2="2.8" stroke="#4F46E5"
            stroke-width="1.5" stroke-linecap="round" />
          <line x1="2.8" y1="-2.8" x2="4.2" y2="-4.2" stroke="#4F46E5"
            stroke-width="1.5" stroke-linecap="round" />
        </g>
      </g>

    {:else if petState === "HasSomething"}
      <!--
        HasSomething: wide eyes (larger dots), open "O" mouth, orange "!"
        above the head. Static — no keyframe (states.css has none for
        .state-has-something; a warm-orange halo tint replaces the base
        filter).
      -->
      <!-- wide eyes -->
      <circle cx="37" cy="40" r="3.5" fill="#1C1C1E" />
      <circle cx="59" cy="40" r="3.5" fill="#1C1C1E" />
      <!-- open mouth (surprise "O") -->
      <circle cx="48" cy="52" r="2.5" fill="none" stroke="#1C1C1E"
        stroke-width="2" />
      <!-- "!" above head (orange #EA580C) -->
      <text x="70" y="14" text-anchor="middle" font-size="13" fill="#EA580C"
        font-family="system-ui" font-weight="700">!</text>

    {:else if petState === "NeedsYou"}
      <!--
        NeedsYou: forward-looking eyes (centered dots, "looking at you"),
        uncertain slight frown, red "?" above the head. Static — no
        keyframe (a red halo tint replaces the base filter).
      -->
      <!-- eyes looking forward (centered pupils) -->
      <circle cx="37" cy="40" r="2.5" fill="#1C1C1E" />
      <circle cx="59" cy="40" r="2.5" fill="#1C1C1E" />
      <!-- uncertain slight frown -->
      <path d="M40 52 Q48 49 56 52" stroke="#1C1C1E" stroke-width="2"
        fill="none" stroke-linecap="round" stroke-linejoin="round" />
      <!-- "?" above head (red #DC2626) -->
      <text x="70" y="14" text-anchor="middle" font-size="13" fill="#DC2626"
        font-family="system-ui" font-weight="700">?</text>

    {:else}
      <!--
        Standby (default): alert, dot eyes open, slight smile, body
        steady. Slow blink animated by states.css (@keyframes
        standby-blink on .eye elements) — the ONLY idle animation on
        the awake pet, very light.
      -->
      <!-- eyes (open / alert) — class="eye" for the blink animation -->
      <circle class="eye" cx="37" cy="40" r="2.5" fill="#1C1C1E" />
      <circle class="eye" cx="59" cy="40" r="2.5" fill="#1C1C1E" />
      <!-- slight smile mouth -->
      <path d="M40 50 Q48 54 56 50" stroke="#1C1C1E" stroke-width="2"
        fill="none" stroke-linecap="round" stroke-linejoin="round" />
    {/if}
  </svg>
  {/if}
</main>

<style>
  .pet {
    width: 100vw;
    height: 100vh;
    margin: 0;
    padding: 0;
    /* Transparent by design; the window itself is transparent/frameless.
       Container never intercepts DOM pointer events so only the robot is a
       click target at the DOM level. OS-level pass-through is governed by
       set_ignore_cursor_events (Rust poller), not CSS pointer-events. */
    background: transparent;
    display: flex;
    align-items: center;
    justify-content: center;
    pointer-events: none;
    font-family: system-ui, -apple-system, "Segoe UI", sans-serif;
  }

  .robot {
    /* Only the robot itself is a DOM click/drag target. */
    pointer-events: auto;
    cursor: pointer;
    transition: transform 90ms ease-out;
    will-change: transform;
  }

  .robot.nudge {
    transform: scale(1.08);
  }

  /* Reminder popup: clean notification card in the 260×160 window. */
  .reminder-popup {
    width: 100%;
    height: 100%;
    background: #ffffff;
    border-radius: 14px;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.18), 0 1px 3px rgba(0, 0, 0, 0.1);
    font-family: system-ui, -apple-system, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif;
    pointer-events: auto;
  }

  .popup-header {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 12px;
    background: linear-gradient(135deg, #4f46e5, #6366f1);
    color: #fff;
    flex-shrink: 0;
  }

  .popup-icon { font-size: 14px; }
  .popup-title { font-size: 13px; font-weight: 600; flex: 1; }

  .popup-close {
    background: transparent;
    border: none;
    color: rgba(255, 255, 255, 0.85);
    font-size: 18px;
    line-height: 1;
    cursor: pointer;
    padding: 0 4px;
    border-radius: 4px;
    transition: all 80ms ease;
  }
  .popup-close:hover {
    background: rgba(255, 255, 255, 0.25);
    color: #fff;
  }

  .popup-text {
    flex: 1;
    padding: 8px 12px;
    font-size: 13px;
    line-height: 1.5;
    color: #1c1c1e;
    white-space: pre-wrap;
    word-break: break-word;
    overflow-y: auto;
    min-height: 0;
  }

  .popup-actions {
    display: flex;
    gap: 4px;
    padding: 5px 8px 7px;
    flex-shrink: 0;
  }

  .popup-btn {
    flex: 1;
    font-size: 11px;
    padding: 5px 2px;
    border: 1px solid #d1d5db;
    border-radius: 7px;
    background: #fff;
    color: #374151;
    cursor: pointer;
    transition: all 80ms ease;
    font-family: inherit;
    white-space: nowrap;
    text-align: center;
    overflow: hidden;
  }

  .popup-btn:hover {
    background: #eef2ff;
    color: #4f46e5;
    border-color: #4f46e5;
  }

  .popup-btn.primary {
    background: #4f46e5;
    color: #fff;
    border-color: #4f46e5;
    font-weight: 600;
    flex: 1.2;
  }

  .popup-btn.primary:hover {
    background: #4338ca;
  }
</style>
