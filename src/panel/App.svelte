<script lang="ts">
  // Task 4 placeholder panel UI with a draggable top bar.
  //
  // Task 15 will build the real tab shell (conversation / todos / reminders
  // / settings) below this top bar — for Task 4 only the bar itself is
  // needed so the panel is draggable and so we can persist its rect on
  // move/resize end.
  //
  // Persistence: `onMoved`/`onResized` fire on every tick of a drag/resize;
  // we debounce by 200ms so the `save_panel_state` invoke only fires once
  // the user has stopped moving (the brief says "on move/resize end"). The
  // store is in-memory (Task 4); Task 15 swaps it for a TOML file so the
  // rect survives restarts. Within a session, Tauri's own hide/show
  // already preserves the rect, so the in-memory copy is mostly for
  // forward-compatibility — but the brief requires the wiring now.
  //
  // The bar is `data-tauri-drag-region` so Tauri's start_dragging handles
  // the OS-level drag; we don't call startDragging ourselves. The bar's
  // pointer cursor and slight gradient hint at "draggable" affordance.

  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";

  let saveTimer: ReturnType<typeof setTimeout> | null = null;

  async function persistPanelState() {
    try {
      const win = getCurrentWindow();
      const pos = await win.outerPosition();
      const size = await win.outerSize();
      await invoke("save_panel_state", {
        x: pos.x,
        y: pos.y,
        w: size.width,
        h: size.height,
      });
    } catch (e) {
      console.warn("[panel] save_panel_state failed:", e);
    }
  }

  // Schedule a persist 200ms in the future; if another move/resize event
  // arrives before the timer fires, the previous timer is cleared and a
  // new one is scheduled. This collapses a stream of move/resize events
  // (Tauri fires one per pixel-of-movement during a drag) into a single
  // `save_panel_state` invoke ~200ms after the user stops moving.
  function schedulePersist() {
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      saveTimer = null;
      void persistPanelState();
    }, 200);
  }

  // Register Tauri window-event listeners on mount. Both return an unlisten
  // fn; we don't bother unlistening because the panel lives for the whole
  // app session (the window is hidden, never destroyed). $effect ensures
  // this only runs in the browser, not during SSR (Tauri has no SSR but
  // Svelte 5's $effect is the documented lifecycle for side effects).
  $effect(() => {
    let unlistenMoved: (() => void) | null = null;
    let unlistenResized: (() => void) | null = null;
    (async () => {
      try {
        unlistenMoved = await getCurrentWindow().onMoved(() => schedulePersist());
        unlistenResized = await getCurrentWindow().onResized(() => schedulePersist());
      } catch (e) {
        console.warn("[panel] onMoved/onResized subscribe failed:", e);
      }
    })();
    return () => {
      // Svelte 5 cleanup: called on component destroy. Best-effort.
      unlistenMoved?.();
      unlistenResized?.();
      // Clear any pending debounce timer so a destroy-after-move-event
      // doesn't fire save_panel_state on a torn-down component (avoids
      // a "state set on unmounted component" warning + a dangling IPC).
      if (saveTimer) {
        clearTimeout(saveTimer);
        saveTimer = null;
      }
    };
  });
</script>

<main class="panel">
  <!--
    Draggable top bar. `data-tauri-drag-region` makes Tauri call
    start_dragging on mousedown anywhere in this bar, so the user can move
    the panel by grabbing the bar. Task 15 will add the tab strip below
    this bar (the bar stays).
  -->
  <header class="dragbar" data-tauri-drag-region>
    <span class="title">桌面宠物</span>
  </header>

  <!-- Placeholder body — Task 15 replaces this with the tab shell. -->
  <section class="body">
    <p>面板占位</p>
  </section>
</main>

<style>
  .panel {
    margin: 0;
    height: 100vh;
    display: flex;
    flex-direction: column;
    font-family: system-ui, -apple-system, "Segoe UI", sans-serif;
    color: #1f2937;
    background: #f8fafc;
    /* OS provides the window chrome (resizable border, etc.) — this just
       fills the client area. */
  }

  .dragbar {
    /* A fixed-height bar that reads as a title strip. Tall enough to grab
       easily (~28px); short enough not to eat the body. */
    height: 28px;
    display: flex;
    align-items: center;
    padding: 0 12px;
    background: linear-gradient(180deg, #e2e8f0 0%, #cbd5e1 100%);
    border-bottom: 1px solid #94a3b8;
    /* Grab cursor hints the bar is draggable. Tauri's start_dragging is
       triggered by `data-tauri-drag-region`, not by this cursor, but the
       visual affordance matters. */
    cursor: grab;
    user-select: none;
    /* Drag-region must not have descendant pointer-events intercepting the
       mousedown — the bar itself is the only child here. */
  }

  .dragbar:active {
    cursor: grabbing;
  }

  .title {
    font-size: 0.8rem;
    font-weight: 600;
    color: #334155;
    /* `pointer-events: none` so the mousedown lands on the bar (the
       drag-region) and not on the text label. */
    pointer-events: none;
  }

  .body {
    flex: 1;
    padding: 1rem;
    /* Body is not a drag-region so future widgets (buttons, inputs) work
       normally. */
    overflow: auto;
  }

  .body p {
    margin: 0;
    color: #64748b;
    font-size: 0.9rem;
  }
</style>
