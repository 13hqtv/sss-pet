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
  import { dismissPetAlert } from "../panel/lib/settings";
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
    // Task 16: when the pet is in `NeedsYou` (showing `?`), clicking
    // the robot DISMISSES the alert (applies `Signal::Dismissed` via
    // the `dismiss_pet_alert` Tauri command) instead of opening the
    // panel — so the `?` clears on the same click. Any other state:
    // open the panel above/below the pet (Task 2/4 behavior).
    //
    // Reading `petState` here is safe: it's the subscribed `pet_state`
    // event's latest payload (kept up to date by the listen handler
    // in onMount). The dismiss command applies `Signal::Dismissed` +
    // emits the resulting state immediately, so the frontend's `?`
    // clears without waiting for the next 1s brain tick.
    if (petState === "NeedsYou") {
      console.log("[pet] robot clicked — 需要你 → dismiss");
      if (nudgeTimer) clearTimeout(nudgeTimer);
      nudge = true;
      nudgeTimer = setTimeout(() => {
        nudge = false;
        nudgeTimer = null;
      }, 100);
      dismissPetAlert().catch((e) =>
        console.warn("[pet] dismiss_pet_alert failed:", e),
      );
      return;
    }
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
    /* FIXED palette — no theme tokens, no data-theme. The robot must read
       on ANY desktop backdrop (light or dark wallpaper). A dual halo
       achieves this without theme switching:
         - dark drop-shadow (rgba(0,0,0,.35)): gives contrast on LIGHT
           wallpapers (the bot casts a soft shadow).
         - soft light glow (rgba(255,255,255,.4)): gives contrast on DARK
           wallpapers (a faint light halo outlines the ink strokes + the
           warm-white fill, which would otherwise blend into black).
       Both layers are cheap (filter, not animation) and don't tick rAF.
       Per-state halos (sleep dim, has-something orange, needs-you red)
       override this base in states.css, keeping the dark shadow so the
       bot still reads on light backdrops in those states. */
    filter:
      drop-shadow(0 2px 3px rgba(0, 0, 0, 0.35))
      drop-shadow(0 0 2px rgba(255, 255, 255, 0.4));
    transition: transform 90ms ease-out;
    will-change: transform;
  }

  .robot.nudge {
    transform: scale(1.08);
  }
</style>
