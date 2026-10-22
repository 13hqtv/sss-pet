<!-- Task 16: the 提醒 (reminders) tab. List reminders (reminder_list) +
     add (text + datetime-local picker → reminder_add as RFC3339) +
     delete. Show due time localized to the user's TZ. Fired reminders
     are dimmed (the scheduler already toast'd + pet-`!` them). -->
<script lang="ts">
  import {
    listReminders,
    addReminder,
    deleteReminder,
    type Reminder,
  } from "../lib/settings";

  let reminders: Reminder[] = $state([]);
  let draftText: string = $state("");
  /** `<input type="datetime-local">` shape: `YYYY-MM-DDTHH:MM` (local).
       We convert to RFC3339 UTC on add. */
  let draftDue: string = $state("");
  let errorMsg: string | null = $state(null);

  async function refresh() {
    try {
      reminders = await listReminders();
      errorMsg = null;
    } catch (e) {
      errorMsg = `加载提醒失败: ${e}`;
    }
  }

  /// Convert a `datetime-local` string (local wall-clock) to an RFC3339
  /// UTC string. `datetime-local` gives `YYYY-MM-DDTHH:MM` in the user's
  /// local TZ (no TZ suffix). We treat it as local wall-clock via
  /// `new Date(...)` which JS parses as local, then `.toISOString()`
  /// emits UTC RFC3339. Returns `null` if the input is invalid.
  function localToRfc3339(local: string): string | null {
    if (!local) return null;
    const d = new Date(local);
    if (isNaN(d.getTime())) return null;
    return d.toISOString();
  }

  /// Format an RFC3339 UTC string as a localized wall-clock string for
  /// display (e.g. `10/21 14:30`). Falls back to the raw string on
  /// parse failure (defensive — shouldn't happen since the db stores
  /// RFC3339).
  function formatDue(rfc3339: string): string {
    const d = new Date(rfc3339);
    if (isNaN(d.getTime())) return rfc3339;
    const mm = String(d.getMonth() + 1).padStart(2, "0");
    const dd = String(d.getDate()).padStart(2, "0");
    const hh = String(d.getHours()).padStart(2, "0");
    const mi = String(d.getMinutes()).padStart(2, "0");
    return `${mm}/${dd} ${hh}:${mi}`;
  }

  async function add() {
    const text = draftText.trim();
    if (!text) return;
    const dueAt = localToRfc3339(draftDue);
    if (!dueAt) {
      errorMsg = "请选择有效的提醒时间";
      return;
    }
    try {
      await addReminder(text, dueAt);
      draftText = "";
      draftDue = "";
      await refresh();
    } catch (e) {
      errorMsg = `添加失败: ${e}`;
    }
  }

  async function remove(id: number) {
    try {
      await deleteReminder(id);
      await refresh();
    } catch (e) {
      errorMsg = `删除失败: ${e}`;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      void add();
    }
  }

  $effect(() => {
    void refresh();
  });
</script>

<section class="reminders">
  <div class="add-row">
    <input
      type="text"
      bind:value={draftText}
      onkeydown={onKeydown}
      placeholder="提醒内容…"
      aria-label="提醒内容"
    />
    <input
      type="datetime-local"
      bind:value={draftDue}
      aria-label="提醒时间"
    />
    <button
      onclick={add}
      disabled={!draftText.trim() || !draftDue}
    >添加</button>
  </div>
  <ul class="list">
    {#each reminders as r (r.id)}
      <li class="reminder" class:fired={r.fired}>
        <div class="content">
          <div class="text">{r.text}</div>
          <div class="due">
            <span class="label">到期:</span>
            <span class="time">{formatDue(r.due_at)}</span>
            {#if r.fired}<span class="badge">已触发</span>{/if}
          </div>
        </div>
        <button
          class="del"
          onclick={() => remove(r.id)}
          aria-label={`删除提醒：${r.text}`}
          title="删除"
        >×</button>
      </li>
    {/each}
    {#if reminders.length === 0}
      <li class="empty">暂无提醒</li>
    {/if}
  </ul>
  {#if errorMsg}<div class="error">{errorMsg}</div>{/if}
</section>

<style>
  /* Task 4: refined restyle — hex → theme.css tokens. Same treatment
   * as TodosView + ChatView: surface fill + bd dividers + acc-soft
   * hover, mut → err delete hover, acc buttons. The "已触发" badge is
   * a muted chip (mut text on bg fill + bd border) — fired = past /
   * dimmed, not an accent state. Due-time display is mut per the
   * brief. Radius: inputs → --r-code, button → --r-chip, badge →
   * --r-chip. Spacing: 8px rhythm. No soft shadow (flat tab content). */

  .reminders {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }

  .add-row {
    display: flex;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--bd);
    background: var(--surface);
    flex-wrap: wrap;
  }

  .add-row input[type="text"] {
    flex: 1;
    min-width: 120px;
    font-size: 0.85rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--bg);
    color: var(--ink);
    transition: border-color var(--t-fast);
  }

  .add-row input[type="text"]:focus {
    border-color: var(--acc);
    outline: none;
  }

  .add-row input[type="text"]::placeholder {
    color: var(--mut);
  }

  .add-row input[type="datetime-local"] {
    font-size: 0.8rem;
    padding: var(--space-1);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--bg);
    color: var(--ink);
    transition: border-color var(--t-fast);
  }

  .add-row input[type="datetime-local"]:focus {
    border-color: var(--acc);
    outline: none;
  }

  .add-row button {
    padding: var(--space-1) var(--space-3);
    font-size: 0.85rem;
    cursor: pointer;
    background: var(--acc);
    color: #fff;
    border: none;
    border-radius: var(--r-chip);
    transition: opacity var(--t-fast);
  }

  .add-row button:hover {
    opacity: 0.9;
  }

  .add-row button:disabled {
    background: var(--mut);
    cursor: not-allowed;
    opacity: 1;
  }

  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow-y: auto;
    flex: 1;
    background: var(--bg);
  }

  .reminder {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--bd);
    background: var(--surface);
    transition: background var(--t-fast);
  }

  .reminder:hover {
    background: var(--acc-soft);
  }

  .reminder .content {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .reminder .text {
    font-size: 0.88rem;
    color: var(--ink);
    word-break: break-word;
  }

  .reminder .due {
    font-size: 0.75rem;
    color: var(--mut);
    display: flex;
    gap: var(--space-1);
    align-items: center;
  }

  .reminder .due .label {
    color: var(--mut);
  }

  .reminder .due .time {
    font-family: var(--mono);
  }

  .reminder.fired .text {
    color: var(--mut);
  }

  /* "已触发" badge — muted neutral chip (fired = past state, not an
   * accent or error state). mut text on bg fill + bd border, full
   * pill radius. */
  .reminder .badge {
    background: var(--bg);
    color: var(--mut);
    border: 1px solid var(--bd);
    padding: 1px var(--space-2);
    border-radius: var(--r-chip);
    font-size: 0.65rem;
    margin-left: var(--space-1);
  }

  .reminder .del {
    background: transparent;
    border: none;
    color: var(--mut);
    font-size: 1rem;
    cursor: pointer;
    padding: 2px var(--space-1);
    border-radius: var(--r-code);
    transition: background var(--t-fast), color var(--t-fast);
  }

  .reminder .del:hover {
    background: var(--err-soft);
    color: var(--err);
  }

  .empty {
    padding: var(--space-5);
    text-align: center;
    color: var(--mut);
    font-size: 0.88rem;
  }

  .error {
    padding: var(--space-1) var(--space-3);
    background: var(--err-soft);
    color: var(--err);
    font-size: 0.8rem;
    border-top: 1px solid var(--err);
  }
</style>
