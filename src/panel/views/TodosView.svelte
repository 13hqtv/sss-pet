<!-- Task 16: the 代办 (todos) tab. List todos (db todo_list) +
     add (input + button → todo_add) + toggle done (checkbox →
     todo_toggle) + delete (button → todo_delete). Chinese. Pure
     invoke-driven; no Tauri events needed (todos are not a stream). -->
<script lang="ts">
  import {
    listTodos,
    addTodo,
    toggleTodo,
    deleteTodo,
    type Todo,
  } from "../lib/settings";

  let todos: Todo[] = $state([]);
  let draft: string = $state("");
  let errorMsg: string | null = $state(null);

  async function refresh() {
    try {
      todos = await listTodos();
      errorMsg = null;
    } catch (e) {
      errorMsg = `加载代办失败: ${e}`;
    }
  }

  async function add() {
    const text = draft.trim();
    if (!text) return;
    try {
      await addTodo(text);
      draft = "";
      await refresh();
    } catch (e) {
      errorMsg = `添加失败: ${e}`;
    }
  }

  async function toggle(id: number) {
    try {
      await toggleTodo(id);
      await refresh();
    } catch (e) {
      errorMsg = `切换失败: ${e}`;
    }
  }

  async function remove(id: number) {
    try {
      await deleteTodo(id);
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

  // Initial load on mount.
  $effect(() => {
    void refresh();
  });
</script>

<section class="todos">
  <div class="add-row">
    <input
      type="text"
      bind:value={draft}
      onkeydown={onKeydown}
      placeholder="添加一条代办…"
      aria-label="代办内容"
    />
    <button onclick={add} disabled={!draft.trim()}>添加</button>
  </div>
  <ul class="list">
    {#each todos as t (t.id)}
      <li class="todo" class:done={t.done}>
        <label>
          <input
            type="checkbox"
            checked={t.done}
            onchange={() => toggle(t.id)}
            aria-label={`标记完成：${t.text}`}
          />
          <span class="text">{t.text}</span>
        </label>
        <button
          class="del"
          onclick={() => remove(t.id)}
          aria-label={`删除：${t.text}`}
          title="删除"
        >×</button>
      </li>
    {/each}
    {#if todos.length === 0}
      <li class="empty">暂无代办</li>
    {/if}
  </ul>
  {#if errorMsg}<div class="error">{errorMsg}</div>{/if}
</section>

<style>
  /* Task 4: refined restyle — hex → theme.css tokens. Matches the
   * ChatView look: surface fill + bd dividers + acc-soft hover, acc
   * checkboxes (via accent-color), mut → err delete button hover.
   * Radius: inputs → --r-code, button → --r-chip. Spacing: 8px rhythm.
   * No soft shadow (flat tab content, not elevated). */

  .todos {
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
  }

  .add-row input {
    flex: 1;
    font-size: 0.85rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--bg);
    color: var(--ink);
    transition: border-color var(--t-fast);
  }

  .add-row input:focus {
    border-color: var(--acc);
    outline: none;
  }

  .add-row input::placeholder {
    color: var(--mut);
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

  .todo {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--bd);
    background: var(--surface);
    transition: background var(--t-fast);
  }

  .todo:hover {
    background: var(--acc-soft);
  }

  .todo label {
    flex: 1;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    cursor: pointer;
    font-size: 0.88rem;
    color: var(--ink);
  }

  .todo input[type="checkbox"] {
    margin: 0;
    width: 16px;
    height: 16px;
    cursor: pointer;
    /* accent-color themes the native checkbox's checked state to the
       accent indigo — "checked = var(--acc)" per the brief, no custom
       switch markup needed. Works in both light + dark. */
    accent-color: var(--acc);
  }

  .todo .text {
    flex: 1;
    word-break: break-word;
  }

  .todo.done .text {
    color: var(--mut);
    text-decoration: line-through;
  }

  .todo .del {
    background: transparent;
    border: none;
    color: var(--mut);
    font-size: 1rem;
    cursor: pointer;
    padding: 2px var(--space-1);
    border-radius: var(--r-code);
    transition: background var(--t-fast), color var(--t-fast);
  }

  .todo .del:hover {
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
