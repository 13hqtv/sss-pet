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
  // - On `mouseup` (drag end) we save the pet's new position via
  //   `save_pet_pos` so the within-session hide+show cycle (tray 显示/隐藏)
  //   returns the pet to the dragged spot. Task 15 will make this durable
  //   across restarts by swapping the in-memory store for a TOML file.
  // - A click *without* movement (mousedown+mouseup at the same screen
  //   position) is interpreted as a click rather than a drag → invoke
  //   `open_panel_above_pet` so the panel pops above (or below, if there's
  //   no room) the pet.

  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";

  // Brief visual nudge when the robot is clicked — proves it received the
  // click. `nudge` toggles a CSS transform for ~100ms. Kept for the click
  // (non-drag) path; a drag does not nudge.
  let nudge = $state(false);
  let nudgeTimer: ReturnType<typeof setTimeout> | null = null;

  // Drag tracking: we record the pet's outer position on mousedown and again
  // on mouseup. If they differ, it was a drag → save the new position; if
  // they match, it was a click → open the panel + nudge. `data-tauri-drag-
  // region` triggers Tauri's start_dragging on mousedown *for us*; we don't
  // call start_dragging ourselves.
  let dragStart: { x: number; y: number } | null = null;

  async function onRobotMouseDown() {
    // Pin the Rust-side DRAGGING flag so the click-through poller keeps
    // ignore=false for the whole drag (prevents a mid-drag flip if the
    // cursor momentarily leaves the stale robot rect between OS drag
    // ticks). Best-effort: if invoke fails (e.g. the bridge is mid-init),
    // drag still works — the poller would just be racy for that drag.
    try {
      await invoke("set_pet_dragging", { dragging: true });
    } catch (e) {
      console.warn("[pet] set_pet_dragging(true) failed:", e);
    }
    try {
      const pos = await getCurrentWindow().outerPosition();
      dragStart = { x: pos.x, y: pos.y };
    } catch (e) {
      console.warn("[pet] outerPosition at mousedown failed:", e);
      dragStart = null;
    }
  }

  async function onRobotMouseUp() {
    // Always release the DRAGGING flag — even if the mousedown invoke
    // failed, the poller must not stay pinned forever.
    try {
      await invoke("set_pet_dragging", { dragging: false });
    } catch (e) {
      console.warn("[pet] set_pet_dragging(false) failed:", e);
    }

    let end: { x: number; y: number } | null = null;
    try {
      const pos = await getCurrentWindow().outerPosition();
      end = { x: pos.x, y: pos.y };
    } catch (e) {
      console.warn("[pet] outerPosition at mouseup failed:", e);
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
</script>

<main class="pet">
  {#snippet robot()}
    <!--
      待命 (standby) pose: antenna up, eyes open, alert. One static pose.
      Task 7 will branch this into per-state variants; `data-pose` tags the
      current pose for that future work. No state machinery here (YAGNI).

      `data-tauri-drag-region` makes Tauri call start_dragging on mousedown,
      so grabbing the robot body drags the pet window. The click-through
      poller keeps ignore=false while the cursor is on the robot, so the
      mousedown reaches us. We pair it with our own onmousedown/onmouseup
      to (a) pin the Rust-side DRAGGING flag and (b) save the resulting
      position on drag end, or open the panel on a click without movement.
    -->
    <svg
      class="robot"
      class:nudge
      data-pose="standby"
      data-tauri-drag-region
      viewBox="0 0 96 96"
      width="96"
      height="96"
      role="button"
      tabindex="0"
      aria-label="桌面宠物机器人 · 待命"
      onmousedown={onRobotMouseDown}
      onmouseup={onRobotMouseUp}
      onkeydown={onRobotKeydown}
    >
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

      <!-- eyes (open / alert) -->
      <circle cx="36" cy="46" r="6.5" fill="#0f172a" />
      <circle cx="60" cy="46" r="6.5" fill="#0f172a" />
      <circle cx="38" cy="44" r="2" fill="#f8fafc" />
      <circle cx="62" cy="44" r="2" fill="#f8fafc" />

      <!-- mouth (neutral alert bar) -->
      <rect x="34" y="62" width="28" height="4.5" rx="2.25" fill="#0f172a" />

      <!-- status LED -->
      <circle cx="72" cy="32" r="3" fill="#34d399" />
    </svg>
  {/snippet}

  {@render robot()}
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
    /* Subtle drop shadow so the bot reads against any backdrop. */
    filter: drop-shadow(0 2px 3px rgba(15, 23, 42, 0.35));
    transition: transform 90ms ease-out;
    will-change: transform;
  }

  .robot.nudge {
    transform: scale(1.08);
  }
</style>
