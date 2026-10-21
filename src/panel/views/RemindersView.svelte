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
  .reminders {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }

  .add-row {
    display: flex;
    gap: 6px;
    padding: 8px;
    border-bottom: 1px solid #cbd5e1;
    background: #f1f5f9;
    flex-wrap: wrap;
  }

  .add-row input[type="text"] {
    flex: 1;
    min-width: 120px;
    font-size: 0.85rem;
    padding: 4px 8px;
    border: 1px solid #cbd5e1;
    border-radius: 4px;
    font-family: inherit;
  }

  .add-row input[type="datetime-local"] {
    font-size: 0.8rem;
    padding: 4px;
    border: 1px solid #cbd5e1;
    border-radius: 4px;
    font-family: inherit;
  }

  .add-row button {
    padding: 4px 12px;
    font-size: 0.85rem;
    cursor: pointer;
    background: #2563eb;
    color: white;
    border: none;
    border-radius: 4px;
  }

  .add-row button:disabled {
    background: #94a3b8;
    cursor: not-allowed;
  }

  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow-y: auto;
    flex: 1;
    background: #f8fafc;
  }

  .reminder {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    border-bottom: 1px solid #e2e8f0;
  }

  .reminder .content {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .reminder .text {
    font-size: 0.88rem;
    color: #1f2937;
    word-break: break-word;
  }

  .reminder .due {
    font-size: 0.75rem;
    color: #64748b;
    display: flex;
    gap: 4px;
    align-items: center;
  }

  .reminder .due .label {
    color: #94a3b8;
  }

  .reminder .due .time {
    font-family: ui-monospace, "Cascadia Code", monospace;
  }

  .reminder.fired .text {
    color: #94a3b8;
  }

  .reminder .badge {
    background: #fef3c7;
    color: #92400e;
    padding: 1px 5px;
    border-radius: 8px;
    font-size: 0.65rem;
    margin-left: 4px;
  }

  .reminder .del {
    background: transparent;
    border: none;
    color: #94a3b8;
    font-size: 1rem;
    cursor: pointer;
    padding: 2px 6px;
    border-radius: 4px;
  }

  .reminder .del:hover {
    background: #fee2e2;
    color: #dc2626;
  }

  .empty {
    padding: 1.5rem;
    text-align: center;
    color: #64748b;
    font-size: 0.88rem;
  }

  .error {
    padding: 6px 10px;
    background: #fee2e2;
    color: #991b1b;
    font-size: 0.8rem;
    border-top: 1px solid #fecaca;
  }
</style>
