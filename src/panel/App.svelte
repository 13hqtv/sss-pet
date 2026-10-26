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
  import { listen } from "@tauri-apps/api/event";

  import ChatView from "./views/ChatView.svelte";
  import TodosView from "./views/TodosView.svelte";
  import RemindersView from "./views/RemindersView.svelte";
  import KpisView from "./views/KpisView.svelte";
  import SettingsView from "./views/SettingsView.svelte";
  import AgentsView from "./views/AgentsView.svelte";

  type TabId = "chat" | "agents" | "todos" | "reminders" | "kpi" | "settings";

  interface Tab {
    id: TabId;
    label: string;
  }

  const TABS: Tab[] = [
    { id: "todos", label: "代办" },
    { id: "reminders", label: "提醒" },
    { id: "kpi", label: "KPI" },
    { id: "chat", label: "对话" },
    { id: "agents", label: "智能体" },
    { id: "settings", label: "设置" },
  ];

  // Active tab — defaults to 代办.
  let activeTab: TabId = $state("todos");

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

  // Panel drag: call startDragging() on mousedown in the tab strip's
  // non-button area. data-tauri-drag-region is unreliable on some
  // Windows + Tauri 2 builds, so we also wire a JS handler as the
  // primary mechanism. Tab buttons are excluded so clicking a tab
  // switches the view instead of dragging the window.
  function onTabsMouseDown(e: MouseEvent) {
    if ((e.target as HTMLElement).closest(".tab")) return;
    getCurrentWindow().startDragging().catch((err) =>
      console.warn("[panel] startDragging failed:", err),
    );
  }

  // Disable the browser context menu (right-click menu) — the panel is
  // a desktop app, not a web page.
  $effect(() => {
    const handler = (e: MouseEvent) => e.preventDefault();
    window.addEventListener("contextmenu", handler);
    return () => window.removeEventListener("contextmenu", handler);
  });

  // Listen for the `switch_tab` event from Rust (e.g. the tray's 设置 menu
  // item emits this to switch the panel to the settings tab before showing
  // it, so the user sees the settings view immediately rather than whatever
  // tab was last active).
  $effect(() => {
    let unlisten: (() => void) | null = null;
    (async () => {
      try {
        unlisten = await listen<string>("switch_tab", (e) => {
          const tab = e.payload as TabId;
          if (
            tab === "chat" ||
            tab === "agents" ||
            tab === "todos" ||
            tab === "reminders" ||
            tab === "kpi" ||
            tab === "settings"
          ) {
            activeTab = tab;
          }
        });
      } catch (e) {
        console.warn("[panel] switch_tab listen failed:", e);
      }
    })();
    return () => unlisten?.();
  });
</script>

<main class="panel">
  <!-- Pill-style tab strip. Also serves as the drag region (replaces
       the old separate dragbar header). data-tauri-drag-region makes
       Tauri call start_dragging on mousedown in the bar area, but the
       tab pills are still clickable because they're above the drag layer. -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <nav class="tabs" data-tauri-drag-region onmousedown={onTabsMouseDown}>
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
    {:else if activeTab === "agents"}
      <AgentsView />
    {:else if activeTab === "todos"}
      <TodosView />
    {:else if activeTab === "reminders"}
      <RemindersView />
    {:else if activeTab === "kpi"}
      <KpisView />
    {:else if activeTab === "settings"}
      <SettingsView />
    {/if}
  </section>
</main>

<style>
  /* Task 4: refined restyle. Every color is a theme.css token so the
   * shell flips with data-theme; no hardcoded hex anywhere (the only
   * fixed-color exceptions are the mac-style window dots, which are
   * product chrome — same exclusion as ChatView's `#fff`
   * accent-foreground). Radius ladder: tabs → --r-chip (full pill).
   * Spacing follows the 8px rhythm (--space-1..5). The dragbar keeps
   * its `data-tauri-drag-region` attribute (Tauri's start_dragging). */

  .panel {
    margin: 0;
    height: 100vh;
    display: flex;
    flex-direction: column;
    font-family: var(--font);
    color: var(--ink);
    background: var(--bg);
  }

  /* Pill-style tab strip. Active pill = acc-soft fill + acc text;
   * inactive = mut text; hover = acc-soft fill (subtle). Full pill
   * radius (--r-chip = 999px). The strip is also the drag region
   * (data-tauri-drag-region) — the tab pills are still clickable
   * because they sit above the drag layer. */
  .tabs {
    flex: 0 0 auto;
    display: flex;
    gap: var(--space-1);
    padding: var(--space-2) var(--space-3);
    background: var(--surface);
    border-bottom: 1px solid var(--bd);
    cursor: grab;
    user-select: none;
  }

  .tabs:active {
    cursor: grabbing;
  }

  .tab {
    padding: var(--space-1) var(--space-3);
    font-size: 0.8rem;
    border: none;
    border-radius: var(--r-chip);
    background: transparent;
    color: var(--mut);
    cursor: pointer;
    transition: background var(--t-fast), color var(--t-fast);
  }

  .tab:hover {
    background: var(--acc-soft);
  }

  .tab.active {
    background: var(--acc-soft);
    color: var(--acc);
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
