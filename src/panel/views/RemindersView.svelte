<!-- 提醒 (reminders) tab — v2. Card-based list with status, recurrence,
     snooze, inline add/edit form. Reminders can be one-time (specific
     datetime) or recurring (every N minutes/hours/days/weeks). When a
     reminder fires, the scheduler emits a `reminder_fired` event to the
     pet window which shows a popup with snooze/dismiss actions. -->
<script lang="ts">
  import {
    listReminders,
    addReminder,
    updateReminder,
    setReminderStatus,
    snoozeReminder,
    deleteReminder,
    type Reminder,
    type ReminderStatus,
    type ReminderType,
    type IntervalUnit,
  } from "../lib/settings";

  let reminders: Reminder[] = $state([]);
  let errorMsg: string | null = $state(null);

  // -- filters --
  let searchText: string = $state("");
  let statusFilter: string = $state("all");

  // -- add/edit form --
  let formOpen: boolean = $state(false);
  let editingId: number | null = $state(null);
  let formText: string = $state("");
  let formType: ReminderType = $state("once");
  let formIntervalValue: number = $state(30);
  let formIntervalUnit: IntervalUnit = $state("minutes");
  let formFireAt: string = $state(""); // datetime-local for once type

  // -- delete confirmation --
  let pendingDeleteId: number | null = $state(null);
  let deleteTimer: ReturnType<typeof setTimeout> | null = null;

  const STATUS_LABEL: Record<string, string> = {
    pending: "待触发",
    active: "已触发",
    done: "已完成",
  };

  const UNIT_LABEL: Record<IntervalUnit, string> = {
    minutes: "分钟",
    hours: "小时",
    days: "天",
    weeks: "周",
  };

  // ===========================================================================
  // Helpers
  // ===========================================================================

  function rfc3339ToLocalInput(rfc: string): string {
    const d = new Date(rfc);
    if (isNaN(d.getTime())) return "";
    const pad = (n: number) => String(n).padStart(2, "0");
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
  }

  function localInputToRfc3339(local: string): string | null {
    if (!local) return null;
    const d = new Date(local);
    if (isNaN(d.getTime())) return null;
    return d.toISOString();
  }

  function formatFireTime(rfc: string | null): string {
    if (!rfc) return "";
    const d = new Date(rfc);
    if (isNaN(d.getTime())) return rfc;
    const pad = (n: number) => String(n).padStart(2, "0");
    return `${d.getFullYear()}/${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  }

  function formatSnooze(rfc: string | null): string {
    if (!rfc) return "";
    return `推迟至 ${formatFireTime(rfc)}`;
  }

  function snoozeFromNow(minutes: number): string {
    const d = new Date(Date.now() + minutes * 60 * 1000);
    return d.toISOString();
  }

  // ===========================================================================
  // Derived: filtered reminders
  // ===========================================================================

  let filteredReminders = $derived.by(() => {
    let result = reminders;
    const q = searchText.trim().toLowerCase();
    if (q) {
      result = result.filter((r) => r.text.toLowerCase().includes(q));
    }
    if (statusFilter !== "all") {
      result = result.filter((r) => r.status === statusFilter);
    }
    // Sort: pending/active first, done last; within group by next_fire_at
    return [...result].sort((a, b) => {
      const sa = a.status === "done" ? 2 : a.status === "active" ? 1 : 0;
      const sb = b.status === "done" ? 2 : b.status === "active" ? 1 : 0;
      if (sa !== sb) return sa - sb;
      if (a.next_fire_at && b.next_fire_at) return a.next_fire_at.localeCompare(b.next_fire_at);
      if (a.next_fire_at && !b.next_fire_at) return -1;
      if (!a.next_fire_at && b.next_fire_at) return 1;
      return 0;
    });
  });

  // ===========================================================================
  // Actions
  // ===========================================================================

  async function refresh() {
    try {
      reminders = await listReminders();
      errorMsg = null;
    } catch (e) {
      errorMsg = `加载提醒失败: ${e}`;
    }
  }

  function startAdd() {
    formOpen = true;
    editingId = null;
    formText = "";
    formType = "once";
    formIntervalValue = 30;
    formIntervalUnit = "minutes";
    formFireAt = "";
  }

  function startEdit(r: Reminder) {
    formOpen = true;
    editingId = r.id;
    formText = r.text;
    formType = r.reminder_type;
    formIntervalValue = r.interval_value || 30;
    formIntervalUnit = r.interval_unit;
    formFireAt = r.next_fire_at ? rfc3339ToLocalInput(r.next_fire_at) : "";
  }

  function cancelForm() {
    formOpen = false;
    editingId = null;
  }

  async function saveForm() {
    const text = formText.trim();
    if (!text) {
      errorMsg = "内容不能为空";
      return;
    }
    if (formType === "once" && !formFireAt) {
      errorMsg = "请设置提醒时间";
      return;
    }
    try {
      const nextFireAt = formType === "once"
        ? localInputToRfc3339(formFireAt)
        : localInputToRfc3339(formFireAt) || new Date(Date.now() + formIntervalValue * 60000).toISOString();
      const params = {
        text,
        reminderType: formType,
        intervalValue: formType === "recurring" ? formIntervalValue : 0,
        intervalUnit: formIntervalUnit,
        nextFireAt,
      };
      if (editingId !== null) {
        await updateReminder({ id: editingId, ...params });
      } else {
        await addReminder(params);
      }
      formOpen = false;
      editingId = null;
      errorMsg = null;
      await refresh();
    } catch (e) {
      errorMsg = `保存失败: ${e}`;
    }
  }

  async function snooze(id: number, minutes: number) {
    try {
      const until = snoozeFromNow(minutes);
      await snoozeReminder(id, until);
      await refresh();
    } catch (e) {
      errorMsg = `推迟失败: ${e}`;
    }
  }

  async function complete(id: number) {
    try {
      await setReminderStatus(id, "done");
      await refresh();
    } catch (e) {
      errorMsg = `完成失败: ${e}`;
    }
  }

  function requestDelete(id: number) {
    if (pendingDeleteId === id) {
      if (deleteTimer) { clearTimeout(deleteTimer); deleteTimer = null; }
      pendingDeleteId = null;
      void confirmDelete(id);
      return;
    }
    pendingDeleteId = id;
    if (deleteTimer) clearTimeout(deleteTimer);
    deleteTimer = setTimeout(() => { pendingDeleteId = null; deleteTimer = null; }, 3000);
  }

  async function confirmDelete(id: number) {
    try {
      await deleteReminder(id);
      await refresh();
    } catch (e) {
      errorMsg = `删除失败: ${e}`;
    }
  }

  $effect(() => {
    void refresh();
    return () => { if (deleteTimer) clearTimeout(deleteTimer); };
  });
</script>

<section class="reminders">
  <!-- Filter bar -->
  <div class="filter-bar">
    <div class="search-row">
      <input type="text" bind:value={searchText} placeholder="搜索提醒内容…" aria-label="搜索提醒" />
      <button class="add-btn" onclick={startAdd} disabled={formOpen}>+ 新增</button>
    </div>
    <div class="chips">
      {#each [["all","全部"],["pending","待触发"],["active","已触发"],["done","已完成"]] as [val,label] (val)}
        <button class="chip" class:active={statusFilter === val} onclick={() => (statusFilter = val)}>
          {label}
        </button>
      {/each}
    </div>
  </div>

  <!-- Add form -->
  {#if formOpen && editingId === null}
    {@render formFields()}
  {/if}

  <!-- List -->
  <div class="list">
    {#each filteredReminders as r (r.id)}
      {#if formOpen && editingId === r.id}
        {@render formFields()}
      {:else}
        <div class="reminder-card" data-status={r.status}>
          <div class="card-head">
            <span class="status-dot" data-status={r.status}></span>
            <span class="content" class:done={r.status === "done"}>{r.text}</span>
            {#if r.reminder_type === "recurring"}
              <span class="type-badge">周期</span>
            {:else}
              <span class="type-badge">一次</span>
            {/if}
            <div class="card-spacer"></div>
            <button class="act" onclick={() => startEdit(r)}>编辑</button>
            <button class="act del" class:confirm={pendingDeleteId === r.id} onclick={() => requestDelete(r.id)}>
              {pendingDeleteId === r.id ? "确认?" : "删除"}
            </button>
          </div>

          <div class="card-meta">
            {#if r.reminder_type === "recurring"}
              <span class="interval">每 {r.interval_value} {UNIT_LABEL[r.interval_unit as IntervalUnit]}</span>
            {/if}
            {#if r.next_fire_at}
              <span class="fire-time">{formatFireTime(r.next_fire_at)}</span>
            {/if}
            {#if r.snoozed_until}
              <span class="snoozed">{formatSnooze(r.snoozed_until)}</span>
            {/if}
          </div>

          {#if r.status !== "done"}
            <div class="card-actions">
              <button class="act" onclick={() => snooze(r.id, 10)}>推迟10分</button>
              <button class="act" onclick={() => snooze(r.id, 60)}>推迟1时</button>
              <button class="act" onclick={() => snooze(r.id, 1440)}>推迟1天</button>
              <button class="act done-btn" onclick={() => complete(r.id)}>不再提醒</button>
            </div>
          {/if}
        </div>
      {/if}
    {/each}

    {#if filteredReminders.length === 0 && !formOpen}
      <div class="empty">
        <p>还没有提醒</p>
        <button class="add-btn" onclick={startAdd}>+ 添加第一条</button>
      </div>
    {/if}
  </div>

  {#if errorMsg}<div class="error">{errorMsg}</div>{/if}
</section>

<!-- Snippet: the add/edit form. -->
{#snippet formFields()}
  <div class="reminder-form">
    <textarea bind:value={formText} placeholder="提醒内容（必填，支持多行）" class="form-text" rows="3"></textarea>
    <div class="form-row">
      <select bind:value={formType} aria-label="提醒类型">
        <option value="once">一次性</option>
        <option value="recurring">周期性</option>
      </select>
    </div>
    {#if formType === "once"}
      <div class="form-row">
        <input type="datetime-local" bind:value={formFireAt} class="form-datetime" />
      </div>
    {:else}
      <div class="form-row">
        <span class="interval-label">每</span>
        <input type="number" bind:value={formIntervalValue} min="1" class="interval-input" />
        <select bind:value={formIntervalUnit} aria-label="周期单位">
          <option value="minutes">分钟</option>
          <option value="hours">小时</option>
          <option value="days">天</option>
          <option value="weeks">周</option>
        </select>
        <span class="interval-label">触发一次</span>
        <input type="datetime-local" bind:value={formFireAt} class="form-datetime" title="首次触发时间（可选，留空=立即开始周期）" />
      </div>
    {/if}
    <div class="form-actions">
      <button class="act cancel" onclick={cancelForm}>取消</button>
      <button class="act save" onclick={saveForm} disabled={!formText.trim()}>保存</button>
    </div>
  </div>
{/snippet}

<style>
  .reminders {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    background: var(--bg);
  }

  .filter-bar {
    flex-shrink: 0;
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--bd);
    background: var(--surface);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .search-row { display: flex; gap: var(--space-2); }

  .search-row input {
    flex: 1; font-size: 0.8rem; padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd); border-radius: var(--r-code);
    font-family: inherit; background: var(--bg); color: var(--ink);
    transition: border-color var(--t-fast);
  }
  .search-row input:focus { border-color: var(--acc); outline: none; }
  .search-row input::placeholder { color: var(--mut); }

  .add-btn {
    padding: var(--space-1) var(--space-3); font-size: 0.8rem; cursor: pointer;
    background: var(--acc); color: #fff; border: none; border-radius: var(--r-chip);
    transition: opacity var(--t-fast); white-space: nowrap;
  }
  .add-btn:hover { opacity: 0.9; }
  .add-btn:disabled { background: var(--mut); cursor: not-allowed; opacity: 1; }

  .chips { display: flex; gap: var(--space-1); overflow-x: auto; scrollbar-width: none; }
  .chips::-webkit-scrollbar { display: none; }

  .chip {
    white-space: nowrap; padding: 3px var(--space-2); border: 1px solid var(--bd);
    border-radius: var(--r-chip); background: var(--surface); color: var(--mut);
    font-size: 0.72rem; cursor: pointer; transition: all var(--t-fast);
  }
  .chip:hover { border-color: var(--acc); color: var(--acc); }
  .chip.active { background: var(--acc); color: #fff; border-color: var(--acc); }

  .list {
    flex: 1; overflow-y: auto; padding: var(--space-2) var(--space-3);
    display: flex; flex-direction: column; gap: var(--space-2);
  }

  /* -- reminder card -- */
  .reminder-card {
    background: var(--surface); border-radius: var(--r-bubble);
    padding: var(--space-2) var(--space-3); border-left: 3px solid var(--mut);
  }
  .reminder-card[data-status="active"] {
    background: var(--warn-soft); border-left-color: var(--warn);
  }
  .reminder-card[data-status="done"] {
    background: var(--ok-soft); border-left-color: var(--ok);
  }

  .card-head { display: flex; align-items: center; gap: var(--space-1); }

  .status-dot {
    flex-shrink: 0; width: 8px; height: 8px; border-radius: 50%; background: var(--mut);
  }
  .status-dot[data-status="active"] { background: var(--warn); }
  .status-dot[data-status="done"] { background: var(--ok); }

  .content {
    flex: 1; min-width: 0; font-size: 0.85rem; color: var(--ink);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    max-height: 3.4rem; /* ~2 lines */
  }
  .content:not(.done) { white-space: pre-wrap; }
  .content.done { text-decoration: line-through; color: var(--mut); }

  .type-badge {
    flex-shrink: 0; font-size: 0.62rem; padding: 1px 5px; border-radius: var(--r-chip);
    color: var(--mut);
  }

  .card-spacer { flex: 1; }

  .card-meta {
    margin-top: var(--space-1); font-size: 0.72rem; color: var(--mut);
    display: flex; align-items: center; gap: var(--space-2); flex-wrap: wrap;
  }
  .interval { color: var(--acc); }
  .snoozed { color: var(--warn); }

  .card-actions {
    margin-top: var(--space-2); display: flex; gap: var(--space-1);
    justify-content: flex-end; flex-wrap: wrap;
  }

  .act {
    background: transparent; border: 1px solid var(--bd); border-radius: var(--r-code);
    color: var(--mut); padding: 2px var(--space-2); font-size: 0.68rem; cursor: pointer;
    transition: all var(--t-fast); flex-shrink: 0;
  }
  .act:hover { background: var(--acc-soft); color: var(--acc); border-color: var(--acc); }
  .act.del:hover { background: var(--err-soft); color: var(--err); border-color: var(--err); }
  .act.del.confirm { background: var(--err); color: #fff; border-color: var(--err); }
  .act.done-btn:hover { background: var(--ok-soft); color: var(--ok); border-color: var(--ok); }
  .act.save { background: var(--acc); color: #fff; border-color: var(--acc); }
  .act.save:disabled { background: var(--mut); border-color: var(--mut); cursor: not-allowed; }

  /* -- form -- */
  .reminder-form {
    background: var(--surface); border: 1px solid var(--bd);
    border-radius: var(--r-bubble); padding: var(--space-3);
    display: flex; flex-direction: column; gap: var(--space-2);
  }
  .form-text {
    font-size: 0.82rem; padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd); border-radius: var(--r-code);
    font-family: inherit; background: var(--bg); color: var(--ink);
    resize: vertical; white-space: pre-wrap;
  }
  .form-text:focus { border-color: var(--acc); outline: none; }

  .form-row { display: flex; gap: var(--space-2); align-items: center; }
  .form-row select, .form-datetime, .interval-input {
    font-size: 0.78rem; padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd); border-radius: var(--r-code);
    font-family: inherit; background: var(--surface); color: var(--ink); min-width: 0;
  }
  .form-row select:focus, .form-datetime:focus, .interval-input:focus {
    border-color: var(--acc); outline: none;
  }
  .interval-label { font-size: 0.75rem; color: var(--mut); white-space: nowrap; }
  .interval-input { width: 60px; flex: 0 0 60px; }

  .form-actions { display: flex; justify-content: flex-end; gap: var(--space-2); }

  .empty {
    padding: var(--space-5); text-align: center; color: var(--mut);
    display: flex; flex-direction: column; align-items: center; gap: var(--space-3);
  }
  .empty p { font-size: 0.85rem; margin: 0; }

  .error {
    flex-shrink: 0; padding: var(--space-1) var(--space-3);
    background: var(--err-soft); color: var(--err); font-size: 0.78rem;
    border-top: 1px solid var(--err);
  }
</style>
