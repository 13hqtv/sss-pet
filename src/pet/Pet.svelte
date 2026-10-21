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
  import { onMount } from "svelte";
  import "./states.css";

  // ---------------------------------------------------------------------------
  // Task 7: pet state + activity nudge.
  // ---------------------------------------------------------------------------

  /// The 5 pet states, as serialized by Rust's `PetState` (serde → variant
  /// name string). Default "Standby" until the first `pet_state` event.
  type PetStateName =
    | "Sleep"
    | "Standby"
    | "Thinking"
    | "HasSomething"
    | "NeedsYou";

  let state = $state<PetStateName>("Standby");

  /// Map a serialized `PetState` string to the CSS class that drives the
  /// per-state styling + animation (or lack thereof) in `states.css`.
  /// Falls back to `state-standby` if an unknown string arrives (defensive
  /// — shouldn't happen, but a future Rust variant added without updating
  /// this map should not break the pet).
  const STATE_CLASS: Record<PetStateName, string> = {
    Sleep: "state-sleep",
    Standby: "state-standby",
    Thinking: "state-thinking",
    HasSomething: "state-has-something",
    NeedsYou: "state-needs-you",
  };

  /// The CSS class for the current state. `state-standby` is the default
  /// and also the fallback for any unknown state string.
  let stateClass = $derived(STATE_CLASS[state] ?? "state-standby");

  /// Brief visual nudge when the robot is clicked — proves it received the
  /// click. `nudge` toggles a CSS transform for ~100ms. Kept for the click
  /// (non-drag) path; a drag does not nudge.
  /// Task 2 carry: `nudgeTimer` is now cleared on component destroy (see
  /// `onMount` cleanup below) — previously it leaked on unmount.
  let nudge = $state(false);
  let nudgeTimer: ReturnType<typeof setTimeout> | null = null;

  /// Activity-nudge flag from the follow-activity throttle (Rust
  /// `pet_activity_nudge` event). Plays a ~1s wiggle WITHOUT changing the
  /// state class. Only set when the pet is awake (asleep pet ignores
  /// nudges — respect quiet, Review Focus #1).
  let nudgeActivity = $state(false);
  let nudgeActivityTimer: ReturnType<typeof setTimeout> | null = null;

  // ---------------------------------------------------------------------------
  // Drag tracking state. `dragStart` records the pet's outer position at
  // mousedown; `dragArmed` tracks whether a drag is in-flight (so the
  // window-level mouseup/pointerup + safety timeout + blur backstop can all
  // funnel through a single idempotent endDrag that clears the Rust-side
  // DRAGGING flag and — if the pet moved more than 2px — invokes
  // `save_pet_pos`. Plain `let` (not `$state`) because none of these are
  // read by the template — they're only touched in event handlers.
  // ---------------------------------------------------------------------------
  let dragStart: { x: number; y: number } | null = null;
  let dragArmed = false;
  let dragSafetyTimer: ReturnType<typeof setTimeout> | null = null;

  // ---------------------------------------------------------------------------
  // Tauri event listeners + cleanup (Task 7).
  //
  // `onMount` subscribes to `pet_state` and `pet_activity_nudge`. Returns a
  // cleanup function that unlistens + clears all pending timers (nudge,
  // nudgeActivity, dragSafety). This closes the Task 2 carry "nudgeTimer
  // leaks on unmount" and ensures no dangling setTimeout fires after the
  // component tears down.
  //
  // `listen` is async (returns a Promise<UnlistenFn>); we guard against the
  // race where the component unmounts before the promises resolve by
  // tracking a `cancelled` flag — if unmounted, we immediately unlisten
  // the just-resolved handle.
  // ---------------------------------------------------------------------------
  onMount(() => {
    let cancelled = false;
    // Stash the unlisten handles; null until the promises resolve.
    let unlistenState: (() => void) | null = null;
    let unlistenNudge: (() => void) | null = null;

    listen<PetStateName>("pet_state", (e) => {
      state = e.payload;
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
      if (state === "Sleep") return;
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

    return () => {
      cancelled = true;
      unlistenState?.();
      unlistenNudge?.();
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

  // The single funnel for "drag end". Idempotent: the first call does the
  // work, subsequent calls (e.g. mouseup + pointerup + safety timeout all
  // firing) are no-ops because `dragArmed` was cleared. Always releases the
  // Rust-side DRAGGING flag (the hard guarantee). The `fromRelease` arg
  // distinguishes a genuine user release (which should disambiguate
  // click-vs-drag) from a recovery path (which should only clear the flag —
  // opening a panel on a stuck-drag recovery would be wrong, since the user
  // didn't click).
  async function endDrag(fromRelease: boolean) {
    if (!dragArmed) return;
    dragArmed = false;
    if (dragSafetyTimer) {
      clearTimeout(dragSafetyTimer);
      dragSafetyTimer = null;
    }

    // Always release the Rust-side DRAGGING flag — even if the mousedown
    // invoke failed, the poller must not stay pinned forever (the brief's
    // hard guarantee).
    try {
      await invoke("set_pet_dragging", { dragging: false });
    } catch (e) {
      console.warn("[pet] set_pet_dragging(false) failed:", e);
    }

    // Recovery paths (safety timeout, blur) stop here — they only
    // guarantee DRAGGING is cleared. The click-vs-drag disambiguation
    // runs only on a genuine user release so a stuck-drag recovery
    // doesn't pop a panel the user didn't ask for.
    if (!fromRelease) return;

    let end: { x: number; y: number } | null = null;
    try {
      const pos = await getCurrentWindow().outerPosition();
      end = { x: pos.x, y: pos.y };
    } catch (e) {
      console.warn("[pet] outerPosition at drag end failed:", e);
    }

    const start = dragStart;
    dragStart = null;

    if (!start || !end) {
      // Couldn't read either position (e.g. the bridge hiccupped). Fall
      // back to the click behavior so the user still gets a panel.
      void onRobotClick();
      return;
    }

    // Threshold for "this was a click, not a drag": 2px in any axis.
    // Tauri's start_dragging consumes tiny movements as part of a click
    // (the OS drag threshold), but a true click should produce identical
    // start/end positions. Use a small epsilon to be safe under DPI.
    const dx = Math.abs(end.x - start.x);
    const dy = Math.abs(end.y - start.y);
    if (dx <= 2 && dy <= 2) {
      void onRobotClick();
    } else {
      // Drag → persist the new position (within-session).
      try {
        await invoke("save_pet_pos", { x: end.x, y: end.y });
      } catch (e) {
        console.warn("[pet] save_pet_pos failed:", e);
      }
    }
  }

  // Mousedown on the robot: arm the drag. DRAGGING=true is dispatched
  // fire-and-forget (no await) at the very start, BEFORE Tauri's
  // drag-region listener calls `start_dragging` and blocks the JS event
  // loop for the duration of the OS modal drag. If the IPC ordering races
  // (Tauri's listener is registered before ours) and Rust processes
  // `start_dragging` first, the flag may be set late — but the safety
  // timeout + blur backstop in `endDrag` guarantee it never sticks true.
  function onRobotMouseDown() {
    // Arm first so any race that fires endDrag before the invoke lands
    // still has a drag to end. Synchronous, no await.
    dragArmed = true;
    // Fire-and-forget the DRAGGING=true IPC so it's dispatched before
    // Tauri's drag-region listener takes over. Errors are logged, not
    // awaited — the drag still works without it (natural cursor-on-
    // robot behavior keeps ignore=false during the drag).
    void invoke("set_pet_dragging", { dragging: true }).catch((e) =>
      console.warn("[pet] set_pet_dragging(true) failed:", e),
    );
    // Record the start position asynchronously. If the OS drag starts
    // before this resolves, dragStart stays null and endDrag's
    // click-vs-drag path falls back to onRobotClick (acceptable — the
    // click is the friendlier fallback).
    void (async () => {
      try {
        const pos = await getCurrentWindow().outerPosition();
        if (dragArmed) dragStart = { x: pos.x, y: pos.y };
      } catch (e) {
        console.warn("[pet] outerPosition at mousedown failed:", e);
      }
    })();
    // Safety timeout: if no release arrives within 5s (e.g. the OS modal
    // drag loop swallowed the mouseup entirely), clear DRAGGING anyway so
    // the click-through poller never stays pinned. 5s is well above any
    // reasonable drag duration; the safety only fires on a genuinely
    // stuck state. Cleared on a real release in endDrag.
    if (dragSafetyTimer) clearTimeout(dragSafetyTimer);
    dragSafetyTimer = setTimeout(() => {
      console.warn("[pet] drag safety timeout — clearing DRAGGING");
      void endDrag(false);
    }, 5000);
  }

  function onRobotClick() {
    console.log("[pet] robot clicked — 待命 → 打开面板");
    if (nudgeTimer) clearTimeout(nudgeTimer);
    nudge = true;
    nudgeTimer = setTimeout(() => {
      nudge = false;
      nudgeTimer = null;
    }, 100);
    // Open the panel above/below the pet (flip direction decided in
    // window.rs::open_panel_above_pet). Best-effort: errors are logged
    // to the dev console rather than surfaced to the user (a click that
    // fails to open the panel is recoverable on the next click).
    invoke("open_panel_above_pet").catch((e) =>
      console.warn("[pet] open_panel_above_pet failed:", e),
    );
  }

  // Keyboard-equivalent of the click so the robot is operable without a
  // mouse (Enter / Space), satisfying the Svelte a11y rule for the click
  // handler on a non-interactive <svg> that carries role="button". A
  // keyboard activation is always a click, never a drag, so it goes straight
  // to onRobotClick.
  function onRobotKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onRobotClick();
    }
  }

  // Register window-level drag-end listeners (mouseup, pointerup) and a
  // blur backstop. These catch the drag release even when the OS modal drag
  // loop steals the SVG element-level mouseup — the primary fix for the
  // Task 4 review's Important finding. All funnel through endDrag, which is
  // idempotent so duplicate firing (mouseup + pointerup both firing, or a
  // late safety-timeout after a real release) is harmless.
  //
  // `pointerup` covers touch/pen in addition to mouse; registering both is
  // belt-and-suspenders since some OS drag-loop paths may swallow one but
  // not the other. `blur` is the focus-loss backstop: if the pet window
  // loses focus mid-drag (e.g. the user alt-tabbed, or the OS stole focus),
  // the drag is definitively over and DRAGGING must clear.
  //
  // $effect ensures the listeners are registered after mount (window is
  // available) and cleaned up on component destroy. The effect body reads
  // no reactive state during setup (only `window` and the stable function
  // refs), so it runs exactly once on mount.
  $effect(() => {
    const handleUp = () => void endDrag(true);
    const handleBlur = () => void endDrag(false);
    window.addEventListener("mouseup", handleUp);
    window.addEventListener("pointerup", handleUp);
    window.addEventListener("blur", handleBlur);
    return () => {
      window.removeEventListener("mouseup", handleUp);
      window.removeEventListener("pointerup", handleUp);
      window.removeEventListener("blur", handleBlur);
      // Clear any in-flight safety timer so the component can tear down
      // without a dangling setTimeout firing after destroy.
      if (dragSafetyTimer) {
        clearTimeout(dragSafetyTimer);
        dragSafetyTimer = null;
      }
      // Defensive: if a drag was armed when the component unmounted (not
      // expected in normal use — the pet window lives for the whole
      // session), release the Rust-side DRAGGING flag so the poller
      // doesn't stay pinned. This is the last-resort backstop.
      if (dragArmed) {
        dragArmed = false;
        void invoke("set_pet_dragging", { dragging: false }).catch(() => {});
      }
    };
  });
</script>

<main class="pet">
  <!--
    The robot SVG. `data-tauri-drag-region` on the root makes Tauri call
    `start_dragging` on mousedown, so grabbing the robot body drags the pet
    window. The click-through poller keeps ignore=false while the cursor is
    on the robot, so the mousedown reaches us. We pair it with our own
    onmousedown to (a) pin the Rust-side DRAGGING flag and (b) record the
    drag start position. The drag-end (DRAGGING release + save_pet_pos /
    click-vs-drag) is handled by WINDOW-level mouseup/pointerup + blur
    listeners registered in the $effect above.

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
    data-tauri-drag-region
    viewBox="0 0 96 96"
    width="96"
    height="96"
    role="button"
    tabindex="0"
    aria-label="桌面宠物机器人 · {state}"
    onmousedown={onRobotMouseDown}
    onkeydown={onRobotKeydown}
  >
    <!-- ============ shared body (all states) ============ -->

    <!-- antenna -->
    <line x1="48" y1="10" x2="48" y2="22" stroke="#334155" stroke-width="2.5"
      stroke-linecap="round" />
    <circle cx="48" cy="7" r="4" fill="#22d3ee" />

    <!-- ears -->
    <rect x="9" y="36" width="7" height="18" rx="3.5" fill="#94a3b8" />
    <rect x="80" y="36" width="7" height="18" rx="3.5" fill="#94a3b8" />

    <!-- head -->
    <rect
      x="15"
      y="22"
      width="66"
      height="54"
      rx="16"
      fill="#e2e8f0"
      stroke="#334155"
      stroke-width="2.5"
    />

    <!-- ============ per-state face + decoration ============ -->

    {#if state === "Sleep"}
      <!--
        Sleep: dimmed (CSS opacity), eyes closed (short lines), slight
        frown mouth, Zzz floating above. NO animation (states.css has no
        keyframe for .state-sleep — Review Focus #1: idle CPU ≈ 0).
      -->
      <!-- closed eyes (horizontal lines) -->
      <line x1="29" y1="46" x2="43" y2="46" stroke="#0f172a" stroke-width="2.5"
        stroke-linecap="round" />
      <line x1="53" y1="46" x2="67" y2="46" stroke="#0f172a" stroke-width="2.5"
        stroke-linecap="round" />
      <!-- slight frown mouth -->
      <path d="M 36 64 Q 48 60 60 64" stroke="#0f172a" stroke-width="2.5"
        fill="none" stroke-linecap="round" />
      <!-- Zzz text (floating, no animation) -->
      <text x="68" y="14" font-size="10" fill="#64748b" font-family="system-ui"
        font-weight="600">Zzz</text>

    {:else if state === "Thinking"}
      <!--
        Thinking: eyes looking up (pupils shifted), spinning cog above head
        (class="cog" — animated by states.css @keyframes thinking-cog-spin).
        The cog is the ONLY animated element in this state.
      -->
      <!-- eyes with pupils looking up -->
      <circle cx="36" cy="46" r="6.5" fill="#f8fafc" stroke="#0f172a" stroke-width="1.5" />
      <circle cx="60" cy="46" r="6.5" fill="#f8fafc" stroke="#0f172a" stroke-width="1.5" />
      <circle cx="36" cy="43" r="3" fill="#0f172a" />
      <circle cx="60" cy="43" r="3" fill="#0f172a" />
      <!-- mouth (thinking: small circle, "o") -->
      <circle cx="48" cy="64" r="3" fill="#0f172a" />
      <!-- spinning cog above head -->
      <g class="cog" transform="translate(48 14)">
        <circle cx="0" cy="0" r="5" fill="none" stroke="#64748b" stroke-width="1.5" />
        <rect x="-1" y="-7" width="2" height="4" fill="#64748b" />
        <rect x="-1" y="3" width="2" height="4" fill="#64748b" />
        <rect x="-7" y="-1" width="4" height="2" fill="#64748b" />
        <rect x="3" y="-1" width="4" height="2" fill="#64748b" />
      </g>

    {:else if state === "HasSomething"}
      <!--
        HasSomething: perked up, wide eyes, open mouth (O), `!` above head
        (amber). Static — no animation (states.css has no keyframe).
      -->
      <!-- wide eyes -->
      <circle cx="36" cy="46" r="7" fill="#0f172a" />
      <circle cx="60" cy="46" r="7" fill="#0f172a" />
      <circle cx="38" cy="44" r="2.2" fill="#f8fafc" />
      <circle cx="62" cy="44" r="2.2" fill="#f8fafc" />
      <!-- open mouth (surprise circle) -->
      <circle cx="48" cy="64" r="4" fill="#0f172a" />
      <!-- `!` above head (amber) -->
      <text x="44" y="14" font-size="14" fill="#f59e0b" font-family="system-ui"
        font-weight="700">!</text>

    {:else if state === "NeedsYou"}
      <!--
        NeedsYou: forward-looking eyes (pupils centered, "looking at
        you"), `?` above head (red). Static — no animation.
      -->
      <!-- eyes looking forward (centered pupils) -->
      <circle cx="36" cy="46" r="6.5" fill="#f8fafc" stroke="#0f172a" stroke-width="1.5" />
      <circle cx="60" cy="46" r="6.5" fill="#f8fafc" stroke="#0f172a" stroke-width="1.5" />
      <circle cx="36" cy="46" r="3" fill="#0f172a" />
      <circle cx="60" cy="46" r="3" fill="#0f172a" />
      <!-- mouth (slight frown, uncertain) -->
      <path d="M 38 64 Q 48 62 58 64" stroke="#0f172a" stroke-width="2.5"
        fill="none" stroke-linecap="round" />
      <!-- `?` above head (red) -->
      <text x="43" y="15" font-size="14" fill="#ef4444" font-family="system-ui"
        font-weight="700">?</text>

    {:else}
      <!--
        Standby (default): alert, eyes open with highlights, neutral
        mouth bar. Slow blink animated by states.css
        (@keyframes standby-blink on .eye elements).
      -->
      <!-- eyes (open / alert) — class="eye" for the blink animation -->
      <circle class="eye" cx="36" cy="46" r="6.5" fill="#0f172a" />
      <circle class="eye" cx="60" cy="46" r="6.5" fill="#0f172a" />
      <circle cx="38" cy="44" r="2" fill="#f8fafc" />
      <circle cx="62" cy="44" r="2" fill="#f8fafc" />
      <!-- mouth (neutral alert bar) -->
      <rect x="34" y="62" width="28" height="4.5" rx="2.25" fill="#0f172a" />
    {/if}

    <!-- status LED (shared, always rendered; green for OK states) -->
    <circle cx="72" cy="32" r="3" fill="#34d399" />
  </svg>
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
    /* Subtle drop shadow so the bot reads against any backdrop.
       Per-state tints (sleep dim, has-something amber, needs-you red) are
       applied in states.css, overriding this base. */
    filter: drop-shadow(0 2px 3px rgba(15, 23, 42, 0.35));
    transition: transform 90ms ease-out;
    will-change: transform;
  }

  .robot.nudge {
    transform: scale(1.08);
  }
</style>
