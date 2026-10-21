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
  // This component only renders the static robot and reacts to clicks on it.

  // Brief visual nudge when the robot is clicked — proves it received the
  // click. `nudge` toggles a CSS transform for ~100ms.
  let nudge = $state(false);
  let nudgeTimer: ReturnType<typeof setTimeout> | null = null;

  function onRobotClick() {
    console.log("[pet] robot clicked — 待命 → 收到点击");
    if (nudgeTimer) clearTimeout(nudgeTimer);
    nudge = true;
    nudgeTimer = setTimeout(() => {
      nudge = false;
      nudgeTimer = null;
    }, 100);
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
</script>

<main class="pet">
  {#snippet robot()}
    <!--
      待命 (standby) pose: antenna up, eyes open, alert. One static pose.
      Task 7 will branch this into per-state variants; `data-pose` tags the
      current pose for that future work. No state machinery here (YAGNI).
    -->
    <svg
      class="robot"
      class:nudge
      data-pose="standby"
      viewBox="0 0 96 96"
      width="96"
      height="96"
      role="button"
      tabindex="0"
      aria-label="桌面宠物机器人 · 待命"
      onclick={onRobotClick}
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
    /* Only the robot itself is a DOM click target. */
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
