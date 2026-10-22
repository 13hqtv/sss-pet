<!-- 代办 (todos) tab — full task tracker. Card-based list with status,
     priority, assignee, time range, notes, multi-dimensional filters,
     inline add/edit form, and smart sorting. Chinese UI.

     Data model: Todo { id, text, status, priority, assignee, start_at,
     end_at, notes, created_at }. Status is pending|in_progress|
     completed|cancelled. Priority is high|medium|low. Times are RFC3339
     UTC on disk, converted to local for display/editing.

     The responsible-persons roster is configured in SettingsView as
     comma-separated text and read here via getSettings(). -->
<script lang="ts">
  import {
    listTodos,
    addTodo,
    updateTodo,
    setTodoStatus,
    deleteTodo,
    getSettings,
    type Todo,
    type TodoStatus,
    type TodoPriority,
    type Settings,
  } from "../lib/settings";

  // -- core state --

  let todos: Todo[] = $state([]);
  let settings: Settings | null = $state(null);
  let errorMsg: string | null = $state(null);

  // -- filters --

  let searchText: string = $state("");
  let statusFilter: string = $state("all");
  let priorityFilter: string = $state("all");
  let assigneeFilter: string = $state("all");
  let showAdvanced: boolean = $state(false);

  // -- add/edit form --

  // formOpen=true with editingId=null → "add" mode (shown at top).
  // formOpen=true with editingId=<number> → "edit" mode (shown in card).
  let formOpen: boolean = $state(false);
  let editingId: number | null = $state(null);
  let formText: string = $state("");
  let formPriority: TodoPriority = $state("medium");
  let formAssignee: string = $state("");
  let formStartAt: string = $state("");
  let formEndAt: string = $state("");
  let formNotes: string = $state("");

  // -- delete confirmation (two-step) --

  let pendingDeleteId: number | null = $state(null);
  let deleteTimer: ReturnType<typeof setTimeout> | null = null;

  // ===========================================================================
  // Helpers
  // ===========================================================================

  /** True if the todo is past its end_at and not completed/cancelled. */
  function isOverdue(todo: Todo): boolean {
    if (!todo.end_at) return false;
    const eff = effectiveStatus(todo);
    if (eff === "completed" || eff === "cancelled") return false;
    return new Date(todo.end_at) < new Date();
  }

  /** Derive the effective display status from the stored status + time
   *  range. A "pending" todo whose start_at has passed (and end_at
   *  hasn't passed yet) is shown as "in_progress" automatically — the
   *  user doesn't need to click 开始. Explicitly set statuses
   *  (in_progress, completed, cancelled) are always honored as-is. */
  function effectiveStatus(todo: Todo): TodoStatus {
    if (todo.status !== "pending") return todo.status;
    const now = new Date();
    const start = todo.start_at ? new Date(todo.start_at) : null;
    const end = todo.end_at ? new Date(todo.end_at) : null;
    // Auto in_progress: we're past the start AND (no end OR before end).
    if (start && now >= start && (!end || now <= end)) {
      return "in_progress";
    }
    return todo.status;
  }

  /** Convert RFC3339 UTC → "YYYY-MM-DDTHH:MM" (local, for datetime-local input). */
  function rfc3339ToLocalInput(rfc: string): string {
    const d = new Date(rfc);
    if (isNaN(d.getTime())) return "";
    const pad = (n: number) => String(n).padStart(2, "0");
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
  }

  /** Convert datetime-local "YYYY-MM-DDTHH:MM" → RFC3339 UTC. null if empty/invalid. */
  function localInputToRfc3339(local: string): string | null {
    if (!local) return null;
    const d = new Date(local);
    if (isNaN(d.getTime())) return null;
    return d.toISOString();
  }

  /** Format a time range for display. Uses full date with year:
   *  "2026/10/21 → 2026/10/24". Same day: just "2026/10/21". */
  function formatTimeRange(start: string | null, end: string | null): string {
    if (!start && !end) return "";
    const pad = (n: number) => String(n).padStart(2, "0");
    const fmtDate = (rfc: string) => {
      const d = new Date(rfc);
      return `${d.getFullYear()}/${pad(d.getMonth() + 1)}/${pad(d.getDate())}`;
    };
    if (start && end) {
      const sd = new Date(start);
      const ed = new Date(end);
      if (sd.toDateString() === ed.toDateString()) {
        return fmtDate(start);
      }
      return `${fmtDate(start)} → ${fmtDate(end)}`;
    }
    if (start) return `${fmtDate(start)} →`;
    return `→ ${fmtDate(end!)}`;
  }

  const STATUS_LABEL: Record<TodoStatus, string> = {
    pending: "待办",
    in_progress: "进行中",
    completed: "完成",
    cancelled: "取消",
  };

  const PRIORITY_LABEL: Record<TodoPriority, string> = {
    high: "高",
    medium: "中",
    low: "低",
  };

  const STATUS_ORDER: Record<string, number> = {
    pending: 0,
    in_progress: 1,
    completed: 2,
    cancelled: 3,
  };

  const PRIORITY_ORDER: Record<string, number> = {
    high: 0,
    medium: 1,
    low: 2,
  };

  // ===========================================================================
  // Derived: filtered + sorted todos, summary counts
  // ===========================================================================

  let filteredTodos = $derived.by(() => {
    let result = todos;

    // Text search: title + notes (case-insensitive)
    const q = searchText.trim().toLowerCase();
    if (q) {
      result = result.filter(
        (t) =>
          t.text.toLowerCase().includes(q) ||
          (t.notes ?? "").toLowerCase().includes(q),
      );
    }

    // Status / overdue filter (uses effective status so auto-in_progress
    // items show up under 进行中, not 待办)
    if (statusFilter === "overdue") {
      result = result.filter((t) => isOverdue(t));
    } else if (statusFilter !== "all") {
      result = result.filter((t) => effectiveStatus(t) === statusFilter);
    }

    // Priority filter
    if (priorityFilter !== "all") {
      result = result.filter((t) => t.priority === priorityFilter);
    }

    // Assignee filter
    if (assigneeFilter !== "all") {
      result = result.filter((t) => t.assignee === assigneeFilter);
    }

    // Smart sort: active first (pending → in_progress), then completed,
    // then cancelled. Within each group: priority high→low, then end_at
    // earliest-first (nulls last). Uses effective status so auto-in_progress
    // items sort with other in_progress items.
    return [...result].sort((a, b) => {
      const sa = STATUS_ORDER[effectiveStatus(a)] ?? 99;
      const sb = STATUS_ORDER[effectiveStatus(b)] ?? 99;
      if (sa !== sb) return sa - sb;
      const pa = PRIORITY_ORDER[a.priority] ?? 99;
      const pb = PRIORITY_ORDER[b.priority] ?? 99;
      if (pa !== pb) return pa - pb;
      if (a.end_at && b.end_at) return a.end_at.localeCompare(b.end_at);
      if (a.end_at && !b.end_at) return -1;
      if (!a.end_at && b.end_at) return 1;
      return 0;
    });
  });

  let summary = $derived.by(() => {
    let pending = 0,
      inProgress = 0,
      completed = 0,
      overdue = 0;
    for (const t of todos) {
      const eff = effectiveStatus(t);
      if (eff === "pending") pending++;
      else if (eff === "in_progress") inProgress++;
      else if (eff === "completed") completed++;
      if (isOverdue(t)) overdue++;
    }
    return { pending, inProgress, completed, overdue };
  });

  /** Assignee dropdown options: roster from settings + current form value
   *  (in case it's not in the roster anymore). */
  let assigneeOptions = $derived.by(() => {
    const roster = settings?.responsible_persons ?? [];
    const set = new Set(roster);
    if (formAssignee && !set.has(formAssignee)) set.add(formAssignee);
    return [...set];
  });

  /** Distinct assignees from existing todos (for the filter dropdown). */
  let allAssignees = $derived.by(() => {
    const set = new Set<string>();
    for (const t of todos) {
      if (t.assignee) set.add(t.assignee);
    }
    return [...set].sort();
  });

  // ===========================================================================
  // Actions
  // ===========================================================================

  async function refresh() {
    try {
      const [list, s] = await Promise.all([listTodos(), getSettings()]);
      todos = list;
      settings = s;
      errorMsg = null;
    } catch (e) {
      errorMsg = `加载代办失败: ${e}`;
    }
  }

  function startAdd() {
    formOpen = true;
    editingId = null;
    formText = "";
    formPriority = "medium";
    formAssignee = "";
    formStartAt = "";
    formEndAt = "";
    formNotes = "";
  }

  function startEdit(todo: Todo) {
    formOpen = true;
    editingId = todo.id;
    formText = todo.text;
    formPriority = todo.priority;
    formAssignee = todo.assignee ?? "";
    formStartAt = todo.start_at ? rfc3339ToLocalInput(todo.start_at) : "";
    formEndAt = todo.end_at ? rfc3339ToLocalInput(todo.end_at) : "";
    formNotes = todo.notes ?? "";
  }

  function cancelForm() {
    formOpen = false;
    editingId = null;
  }

  async function saveForm() {
    const text = formText.trim();
    if (!text) {
      errorMsg = "标题不能为空";
      return;
    }
    try {
      const params = {
        text,
        priority: formPriority,
        assignee: formAssignee || null,
        startAt: localInputToRfc3339(formStartAt),
        endAt: localInputToRfc3339(formEndAt),
        notes: formNotes.trim() || null,
      };
      if (editingId !== null) {
        await updateTodo({ id: editingId, ...params });
      } else {
        await addTodo(params);
      }
      formOpen = false;
      editingId = null;
      errorMsg = null;
      await refresh();
    } catch (e) {
      errorMsg = `保存失败: ${e}`;
    }
  }

  async function setStatus(id: number, status: TodoStatus) {
    try {
      await setTodoStatus(id, status);
      await refresh();
    } catch (e) {
      errorMsg = `状态更新失败: ${e}`;
    }
  }

  /** First click: arm the confirm state. Second click within 3s: delete. */
  function requestDelete(id: number) {
    if (pendingDeleteId === id) {
      // Second click — confirm.
      if (deleteTimer) {
        clearTimeout(deleteTimer);
        deleteTimer = null;
      }
      pendingDeleteId = null;
      void confirmDelete(id);
      return;
    }
    // First click — arm.
    pendingDeleteId = id;
    if (deleteTimer) clearTimeout(deleteTimer);
    deleteTimer = setTimeout(() => {
      pendingDeleteId = null;
      deleteTimer = null;
    }, 3000);
  }

  async function confirmDelete(id: number) {
    try {
      await deleteTodo(id);
      await refresh();
    } catch (e) {
      errorMsg = `删除失败: ${e}`;
    }
  }

  // Initial load on mount.
  $effect(() => {
    void refresh();
    return () => {
      if (deleteTimer) clearTimeout(deleteTimer);
    };
  });
</script>

<section class="todos">
  <!-- Filter bar -->
  <div class="filter-bar">
    <div class="search-row">
      <input
        type="text"
        bind:value={searchText}
        placeholder="搜索标题或备注…"
        aria-label="搜索代办"
      />
      <button class="add-btn" onclick={startAdd} disabled={formOpen}>
        + 新增
      </button>
    </div>

    <div class="chips">
      {#each [["all","全部"],["pending","待办"],["in_progress","进行中"],["completed","完成"],["cancelled","取消"],["overdue","逾期"]] as [val,label] (val)}
        <button
          class="chip"
          class:active={statusFilter === val}
          onclick={() => (statusFilter = val)}
        >
          {label}
          {#if val === "pending" && summary.pending > 0}
            <span class="count">{summary.pending}</span>
          {:else if val === "in_progress" && summary.inProgress > 0}
            <span class="count">{summary.inProgress}</span>
          {:else if val === "completed" && summary.completed > 0}
            <span class="count">{summary.completed}</span>
          {:else if val === "overdue" && summary.overdue > 0}
            <span class="count warn">{summary.overdue}</span>
          {/if}
        </button>
      {/each}
    </div>

    <button class="toggle-advanced" onclick={() => (showAdvanced = !showAdvanced)}>
      {showAdvanced ? "收起筛选" : "更多筛选"}
    </button>

    {#if showAdvanced}
      <div class="advanced">
        <select bind:value={priorityFilter} aria-label="优先级筛选">
          <option value="all">全部优先级</option>
          <option value="high">高</option>
          <option value="medium">中</option>
          <option value="low">低</option>
        </select>
        <select bind:value={assigneeFilter} aria-label="责任人筛选">
          <option value="all">全部责任人</option>
          {#each allAssignees as name (name)}
            <option value={name}>{name}</option>
          {/each}
        </select>
      </div>
    {/if}
  </div>

  <!-- Summary bar -->
  {#if todos.length > 0}
    <div class="summary">
      待办 {summary.pending}
      · 进行中 {summary.inProgress}
      · 完成 {summary.completed}
      {#if summary.overdue > 0}
        · <span class="overdue-text">逾期 {summary.overdue}</span>
      {/if}
    </div>
  {/if}

  <!-- Add form (shown at top when in add mode) -->
  {#if formOpen && editingId === null}
    {@render formFields()}
  {/if}

  <!-- Todo list -->
  <div class="list">
    {#each filteredTodos as t (t.id)}
      {#if formOpen && editingId === t.id}
        {@render formFields()}
      {:else}
        <div
          class="todo-card"
          data-status={effectiveStatus(t)}
          class:overdue={isOverdue(t)}
        >
          <div class="card-head">
            <span class="status-dot" data-status={effectiveStatus(t)}></span>
            <span class="title" class:done={effectiveStatus(t) === "completed" || effectiveStatus(t) === "cancelled"}>
              {t.text}
            </span>
            {#if t.assignee}
              <span class="assignee">@{t.assignee}</span>
            {/if}
            {#if t.start_at || t.end_at}
              <span class="time-range">{formatTimeRange(t.start_at, t.end_at)}</span>
            {/if}
            <span class="priority-badge" data-priority={t.priority}>
              {PRIORITY_LABEL[t.priority as TodoPriority]}
            </span>
            {#if isOverdue(t)}
              <span class="overdue-badge">逾期</span>
            {/if}
          </div>

          <div class="card-bottom">
            <span class="card-notes" title={t.notes ?? ""}>{t.notes ?? ""}</span>
            <div class="card-actions">
              {#if effectiveStatus(t) === "pending"}
                <button class="act" onclick={() => setStatus(t.id, "in_progress")}>开始</button>
              {/if}
              {#if effectiveStatus(t) === "pending" || effectiveStatus(t) === "in_progress"}
                <button class="act" onclick={() => setStatus(t.id, "completed")}>完成</button>
                <button class="act" onclick={() => setStatus(t.id, "cancelled")}>取消</button>
              {/if}
              <button class="act" onclick={() => startEdit(t)}>编辑</button>
              <button
                class="act del"
                class:confirm={pendingDeleteId === t.id}
                onclick={() => requestDelete(t.id)}
              >
                {pendingDeleteId === t.id ? "确认?" : "删除"}
              </button>
            </div>
          </div>
        </div>
      {/if}
    {/each}

    {#if filteredTodos.length === 0 && !formOpen}
      <div class="empty">
        {#if todos.length === 0}
          <p>还没有代办事项</p>
          <button class="add-btn" onclick={startAdd}>+ 添加第一条</button>
        {:else}
          <p>没有符合条件的代办</p>
        {/if}
      </div>
    {/if}
  </div>

  {#if errorMsg}
    <div class="error">{errorMsg}</div>
  {/if}
</section>

<!-- Snippet: the add/edit form fields. Rendered either at the top (add)
     or inline in a card (edit). -->
{#snippet formFields()}
  <div class="todo-form">
    <input
      type="text"
      bind:value={formText}
      placeholder="标题（必填）"
      aria-label="代办标题"
      class="form-title"
    />
    <div class="form-row">
      <select bind:value={formPriority} aria-label="优先级">
        <option value="high">优先级：高</option>
        <option value="medium">优先级：中</option>
        <option value="low">优先级：低</option>
      </select>
      <select bind:value={formAssignee} aria-label="责任人">
        <option value="">责任人（无）</option>
        {#each assigneeOptions as name (name)}
          <option value={name}>{name}</option>
        {/each}
      </select>
    </div>
    <div class="form-row">
      <label>
        <span>开始</span>
        <input
          type="datetime-local"
          value={formStartAt}
          oninput={(e) => (formStartAt = e.currentTarget.value)}
        />
      </label>
      <label>
        <span>结束</span>
        <input
          type="datetime-local"
          value={formEndAt}
          oninput={(e) => (formEndAt = e.currentTarget.value)}
        />
      </label>
    </div>
    <textarea
      bind:value={formNotes}
      placeholder="备注（可选）"
      aria-label="备注"
      rows="2"
    ></textarea>
    <div class="form-actions">
      <button class="act cancel" onclick={cancelForm}>取消</button>
      <button class="act save" onclick={saveForm} disabled={!formText.trim()}>
        保存
      </button>
    </div>
  </div>
{/snippet}

<style>
  .todos {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    background: var(--bg);
  }

  /* -- filter bar -- */

  .filter-bar {
    flex-shrink: 0;
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--bd);
    background: var(--surface);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .search-row {
    display: flex;
    gap: var(--space-2);
  }

  .search-row input {
    flex: 1;
    font-size: 0.8rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--bg);
    color: var(--ink);
    transition: border-color var(--t-fast);
  }

  .search-row input:focus {
    border-color: var(--acc);
    outline: none;
  }

  .search-row input::placeholder {
    color: var(--mut);
  }

  .add-btn {
    padding: var(--space-1) var(--space-3);
    font-size: 0.8rem;
    cursor: pointer;
    background: var(--acc);
    color: #fff;
    border: none;
    border-radius: var(--r-chip);
    transition: opacity var(--t-fast);
    white-space: nowrap;
  }

  .add-btn:hover {
    opacity: 0.9;
  }

  .add-btn:disabled {
    background: var(--mut);
    cursor: not-allowed;
    opacity: 1;
  }

  .chips {
    display: flex;
    gap: var(--space-1);
    overflow-x: auto;
    scrollbar-width: none;
    -webkit-overflow-scrolling: touch;
  }

  .chips::-webkit-scrollbar {
    display: none;
  }

  .chip {
    white-space: nowrap;
    padding: 3px var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-chip);
    background: var(--surface);
    color: var(--mut);
    font-size: 0.72rem;
    cursor: pointer;
    transition: all var(--t-fast);
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }

  .chip:hover {
    border-color: var(--acc);
    color: var(--acc);
  }

  .chip.active {
    background: var(--acc);
    color: #fff;
    border-color: var(--acc);
  }

  .chip .count {
    font-size: 0.65rem;
    opacity: 0.8;
  }

  .chip .count.warn {
    color: var(--warn);
  }

  .chip.active .count {
    opacity: 0.95;
  }

  .toggle-advanced {
    align-self: flex-start;
    background: transparent;
    border: none;
    color: var(--mut);
    font-size: 0.72rem;
    cursor: pointer;
    padding: 0;
  }

  .toggle-advanced:hover {
    color: var(--acc);
  }

  .advanced {
    display: flex;
    gap: var(--space-2);
  }

  .advanced select {
    flex: 1;
    font-size: 0.78rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--surface);
    color: var(--ink);
    transition: border-color var(--t-fast);
  }

  .advanced select:focus {
    border-color: var(--acc);
    outline: none;
  }

  /* -- summary bar -- */

  .summary {
    flex-shrink: 0;
    padding: var(--space-1) var(--space-3);
    font-size: 0.72rem;
    color: var(--mut);
    background: var(--bg);
    border-bottom: 1px solid var(--bd);
  }

  .overdue-text {
    color: var(--err);
    font-weight: 500;
  }

  /* -- list -- */

  .list {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-2) var(--space-3);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  /* -- todo card -- */

  .todo-card {
    background: var(--surface);
    border-radius: var(--r-bubble);
    padding: var(--space-2) var(--space-3);
    border-left: 3px solid var(--mut);
    transition: opacity var(--t-base);
  }

  .todo-card[data-status="in_progress"] {
    background: var(--warn-soft);
    border-left-color: var(--warn);
  }

  .todo-card[data-status="completed"] {
    background: var(--ok-soft);
    border-left-color: var(--ok);
  }

  .todo-card[data-status="cancelled"] {
    opacity: 0.55;
    border-left-color: var(--mut);
  }

  .todo-card.overdue {
    border-left-color: var(--err);
  }

  .card-head {
    display: flex;
    align-items: center;
    gap: var(--space-1);
  }

  .status-dot {
    flex-shrink: 0;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--mut);
  }

  .status-dot[data-status="in_progress"] {
    background: var(--warn);
  }

  .status-dot[data-status="completed"] {
    background: var(--ok);
  }

  .status-dot[data-status="cancelled"] {
    background: var(--mut);
  }

  .title {
    flex: 1;
    min-width: 0;
    font-size: 0.85rem;
    color: var(--ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    line-height: 1.4;
  }

  .title.done {
    text-decoration: line-through;
    color: var(--mut);
  }

  .priority-badge {
    flex-shrink: 0;
    font-size: 0.65rem;
    padding: 1px 6px;
    border-radius: var(--r-chip);
    font-weight: 500;
  }

  .priority-badge[data-priority="high"] {
    background: var(--err-soft);
    color: var(--err);
  }

  .priority-badge[data-priority="medium"] {
    background: var(--warn-soft);
    color: var(--warn);
  }

  .priority-badge[data-priority="low"] {
    color: var(--mut);
  }

  .overdue-badge {
    flex-shrink: 0;
    font-size: 0.62rem;
    padding: 1px 6px;
    border-radius: var(--r-chip);
    background: var(--err);
    color: #fff;
    font-weight: 500;
  }

  /* Time range — lives in card-head after assignee, compact muted text. */
  .time-range {
    flex-shrink: 0;
    font-size: 0.68rem;
    color: var(--mut);
    white-space: nowrap;
  }

  /* Assignee chip — lives in card-head, before the time range. */
  .assignee {
    flex-shrink: 0;
    font-size: 0.68rem;
    color: var(--acc);
    background: var(--acc-soft);
    padding: 1px 6px;
    border-radius: var(--r-chip);
    white-space: nowrap;
  }

  /* Notes + actions share one line. Notes truncates with ellipsis if too
   * long; actions are always fully visible on the right. */
  .card-bottom {
    margin-top: var(--space-1);
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .card-notes {
    flex: 1;
    min-width: 0;
    font-size: 0.72rem;
    color: var(--mut);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .card-actions {
    flex-shrink: 0;
    display: flex;
    gap: var(--space-1);
    flex-wrap: wrap;
    justify-content: flex-end;
  }

  .act {
    background: transparent;
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    color: var(--mut);
    padding: 2px var(--space-2);
    font-size: 0.7rem;
    cursor: pointer;
    transition: all var(--t-fast);
  }

  .act:hover {
    background: var(--acc-soft);
    color: var(--acc);
    border-color: var(--acc);
  }

  .act.del:hover {
    background: var(--err-soft);
    color: var(--err);
    border-color: var(--err);
  }

  .act.del.confirm {
    background: var(--err);
    color: #fff;
    border-color: var(--err);
  }

  .act.save {
    background: var(--acc);
    color: #fff;
    border-color: var(--acc);
  }

  .act.save:disabled {
    background: var(--mut);
    border-color: var(--mut);
    cursor: not-allowed;
    opacity: 1;
  }

  /* -- add/edit form -- */

  .todo-form {
    background: var(--surface);
    border: 1px solid var(--bd);
    border-radius: var(--r-bubble);
    padding: var(--space-2) var(--space-3);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .form-title {
    font-size: 0.85rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--bg);
    color: var(--ink);
    transition: border-color var(--t-fast);
  }

  .form-title:focus {
    border-color: var(--acc);
    outline: none;
  }

  .form-row {
    display: flex;
    gap: var(--space-2);
  }

  .form-row select,
  .form-row input[type="datetime-local"] {
    flex: 1;
    font-size: 0.78rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--surface);
    color: var(--ink);
    transition: border-color var(--t-fast);
    min-width: 0;
  }

  .form-row select:focus,
  .form-row input[type="datetime-local"]:focus {
    border-color: var(--acc);
    outline: none;
  }

  .form-row label {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .form-row label span {
    font-size: 0.68rem;
    color: var(--mut);
  }

  .todo-form textarea {
    font-size: 0.78rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--bg);
    color: var(--ink);
    resize: vertical;
    transition: border-color var(--t-fast);
  }

  .todo-form textarea:focus {
    border-color: var(--acc);
    outline: none;
  }

  .form-actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }

  /* -- empty state -- */

  .empty {
    padding: var(--space-5);
    text-align: center;
    color: var(--mut);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-3);
  }

  .empty p {
    font-size: 0.85rem;
    margin: 0;
  }

  /* -- error -- */

  .error {
    flex-shrink: 0;
    padding: var(--space-1) var(--space-3);
    background: var(--err-soft);
    color: var(--err);
    font-size: 0.78rem;
    border-top: 1px solid var(--err);
  }
</style>
