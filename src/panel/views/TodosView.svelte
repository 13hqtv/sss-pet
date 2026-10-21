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
  .todos {
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
  }

  .add-row input {
    flex: 1;
    font-size: 0.85rem;
    padding: 4px 8px;
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

  .todo {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    border-bottom: 1px solid #e2e8f0;
  }

  .todo label {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 8px;
    cursor: pointer;
    font-size: 0.88rem;
    color: #1f2937;
  }

  .todo input[type="checkbox"] {
    margin: 0;
    width: 16px;
    height: 16px;
    cursor: pointer;
  }

  .todo .text {
    flex: 1;
    word-break: break-word;
  }

  .todo.done .text {
    color: #94a3b8;
    text-decoration: line-through;
  }

  .todo .del {
    background: transparent;
    border: none;
    color: #94a3b8;
    font-size: 1rem;
    cursor: pointer;
    padding: 2px 6px;
    border-radius: 4px;
  }

  .todo .del:hover {
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
