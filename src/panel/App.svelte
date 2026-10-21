<script lang="ts">
  // Task 15: panel tab shell. Task 4's draggable top bar STAYS at the
  // very top (the bar's `data-tauri-drag-region` makes Tauri call
  // `start_dragging` on mousedown anywhere in it, so the panel is
  // movable by grabbing the bar). Below the bar, a pill-style tab
  // strip switches between four views: 对话 (Task 15 ChatView),
  // 代办 / 提醒 / 设置 (Task 16 placeholders).
  //
  // Persistence (Task 4, unchanged): `onMoved`/`onResized` fire on
  // every tick of a drag/resize; we debounce by 200ms so the
  // `save_panel_state` invoke only fires once the user has stopped
  // moving (the brief says "on move/resize end"). Task 14 backed this
  // with a real TOML file next to the exe (or %APPDATA% fallback) so
  // the rect survives restarts.
  //
  // The bar is `data-tauri-drag-region` so Tauri's start_dragging
  // handles the OS-level drag; we don't call startDragging ourselves.

  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";

  import ChatView from "./views/ChatView.svelte";
  import TodosView from "./views/TodosView.svelte";
  import RemindersView from "./views/RemindersView.svelte";
  import SettingsView from "./views/SettingsView.svelte";

  type TabId = "chat" | "todos" | "reminders" | "settings";

  interface Tab {
    id: TabId;
    label: string;
  }

  const TABS: Tab[] = [
    { id: "chat", label: "对话" },
    { id: "todos", label: "代办" },
    { id: "reminders", label: "提醒" },
    { id: "settings", label: "设置" },
  ];

  // Active tab — defaults to 对话 (the main view). Svelte 5 runes:
  // typed via the variable annotation (NOT `$state<T>(...)`, which
  // svelte-check mis-parses — see task-15-report.md "Svelte 5 runes
  // + svelte-check" concern).
  let activeTab: TabId = $state("chat");

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

  function switchTab(id: TabId) {
    activeTab = id;
  }
</script>

<main class="panel">
  <!--
    Draggable top bar. `data-tauri-drag-region` makes Tauri call
    start_dragging on mousedown anywhere in this bar, so the user can
    move the panel by grabbing the bar. The bar stays at the very top
    (Task 4); the Task 15 tab strip lives below it.
  -->
  <header class="dragbar" data-tauri-drag-region>
    <span class="title">桌面宠物</span>
  </header>

  <!-- Pill-style tab strip. Each pill switches the active view below.
       The strip is NOT a drag-region so the pills are clickable. -->
  <nav class="tabs">
    {#each TABS as tab (tab.id)}
      <button
        class="tab"
        class:active={activeTab === tab.id}
        onclick={() => switchTab(tab.id)}
      >
        {tab.label}
      </button>
    {/each}
  </nav>

  <!-- Active view fills the remaining space. Each view owns its own
       layout (ChatView is a two-pane sidebar + main; the placeholders
       are centered labels). -->
  <section class="body">
    {#if activeTab === "chat"}
      <ChatView />
    {:else if activeTab === "todos"}
      <TodosView />
    {:else if activeTab === "reminders"}
      <RemindersView />
    {:else if activeTab === "settings"}
      <SettingsView />
    {/if}
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
    flex: 0 0 28px;
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

  .tabs {
    flex: 0 0 auto;
    display: flex;
    gap: 2px;
    padding: 6px 8px;
    background: #e2e8f0;
    border-bottom: 1px solid #94a3b8;
  }

  .tab {
    padding: 4px 12px;
    font-size: 0.8rem;
    border: none;
    border-radius: 12px;
    background: transparent;
    color: #475569;
    cursor: pointer;
    transition: background 0.1s, color 0.1s;
  }

  .tab:hover {
    background: #cbd5e1;
  }

  .tab.active {
    background: #2563eb;
    color: white;
  }

  .body {
    flex: 1;
    min-height: 0;
    /* Body is not a drag-region so the views' buttons / inputs work
       normally. Each view manages its own overflow (ChatView scrolls
       its message list internally). */
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
</style>
