<!-- KPI 管理 tab — team KPI tracking. Year-scoped card list with per-type
     progress bars (completed/target, can exceed 100%), status management
     (not_started/in_progress/completed), inline add/edit form, filters.
     Chinese UI.

     Data model: Kpi { id, year, kpi_type, title, details, planned_at,
     status, created_at, updated_at }. KpiTarget { year, kpi_type,
     target_count }. The kpi_types roster is configured in SettingsView
     as comma-separated text. Per-type annual targets are edited inline
     here (small number input next to each progress bar). -->

<script lang="ts">
  import {
    listKpis,
    addKpi,
    updateKpi,
    setKpiStatus,
    deleteKpi,
    listKpiTargets,
    upsertKpiTarget,
    getSettings,
    type Kpi,
    type KpiStatus,
    type KpiTarget,
    type Settings,
  } from "../lib/settings";

  // -- core state --

  let kpis: Kpi[] = $state([]);
  let targets: KpiTarget[] = $state([]);
  let settings: Settings | null = $state(null);
  let errorMsg: string | null = $state(null);
  let selectedYear: number = $state(new Date().getFullYear());

  // -- filters --

  let searchText: string = $state("");
  let statusFilter: string = $state("all");
  let typeFilter: string = $state("all");

  // -- add/edit form --

  let formOpen: boolean = $state(false);
  let editingId: number | null = $state(null);
  let formYear: number = $state(new Date().getFullYear());
  let formType: string = $state("");
  let formTitle: string = $state("");
  let formDetails: string = $state("");
  let formPlannedAt: string = $state("");

  // -- delete confirmation (two-step) --

  let pendingDeleteId: number | null = $state(null);
  let deleteTimer: ReturnType<typeof setTimeout> | null = null;

  // ===========================================================================
  // Helpers
  // ===========================================================================

  const STATUS_LABEL: Record<string, string> = {
    not_started: "未进行",
    in_progress: "进行中",
    completed: "已完成",
  };

  const STATUS_ORDER: Record<string, number> = {
    not_started: 0,
    in_progress: 1,
    completed: 2,
  };

  /** Convert RFC3339 → "YYYY-MM-DD" (for date input). */
  function rfc3339ToDateInput(rfc: string): string {
    const d = new Date(rfc);
    if (isNaN(d.getTime())) return "";
    const pad = (n: number) => String(n).padStart(2, "0");
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
  }

  /** Convert "YYYY-MM-DD" → RFC3339 (midnight UTC). null if empty. */
  function dateInputToRfc3339(date: string): string | null {
    if (!date) return null;
    return `${date}T00:00:00Z`;
  }

  /** Format a planned_at date for display: "2026/10/21". */
  function formatDate(rfc: string | null): string {
    if (!rfc) return "";
    const d = new Date(rfc);
    if (isNaN(d.getTime())) return "";
    const pad = (n: number) => String(n).padStart(2, "0");
    return `${d.getFullYear()}/${pad(d.getMonth() + 1)}/${pad(d.getDate())}`;
  }

  // ===========================================================================
  // Derived
  // ===========================================================================

  /** Distinct years from data + current year, sorted DESC. */
  let availableYears = $derived.by(() => {
    const set = new Set<number>();
    for (const k of kpis) set.add(k.year);
    set.add(new Date().getFullYear());
    return [...set].sort((a, b) => b - a);
  });

  /** KPIs for the selected year only. */
  let yearKpis = $derived.by(() => kpis.filter((k) => k.year === selectedYear));

  /** Per-type progress data for the selected year.
   *  Each entry: { type, completed, total, target, percent }. */
  let progressByType = $derived.by(() => {
    const types = settings?.kpi_types ?? [];
    return types.map((type) => {
      let completed = 0;
      let total = 0;
      for (const k of yearKpis) {
        if (k.kpi_type !== type) continue;
        total++;
        if (k.status === "completed") completed++;
      }
      const target = targets.find((t) => t.kpi_type === type)?.target_count ?? 0;
      const percent = target > 0 ? Math.round((completed / target) * 100) : 0;
      return { type, completed, total, target, percent };
    });
  });

  /** Filtered + sorted KPIs for the selected year. */
  let filteredKpis = $derived.by(() => {
    let result = yearKpis;
    const q = searchText.trim().toLowerCase();
    if (q) {
      result = result.filter(
        (k) =>
          k.title.toLowerCase().includes(q) ||
          (k.details ?? "").toLowerCase().includes(q),
      );
    }
    if (statusFilter !== "all") {
      result = result.filter((k) => k.status === statusFilter);
    }
    if (typeFilter !== "all") {
      result = result.filter((k) => k.kpi_type === typeFilter);
    }
    return [...result].sort((a, b) => {
      const sa = STATUS_ORDER[a.status] ?? 99;
      const sb = STATUS_ORDER[b.status] ?? 99;
      if (sa !== sb) return sa - sb;
      if (a.planned_at && b.planned_at) return a.planned_at.localeCompare(b.planned_at);
      if (a.planned_at && !b.planned_at) return -1;
      if (!a.planned_at && b.planned_at) return 1;
      return a.id - b.id;
    });
  });

  let summary = $derived.by(() => {
    let notStarted = 0,
      inProgress = 0,
      completed = 0;
    for (const k of yearKpis) {
      if (k.status === "not_started") notStarted++;
      else if (k.status === "in_progress") inProgress++;
      else if (k.status === "completed") completed++;
    }
    return { notStarted, inProgress, completed };
  });

  let typeOptions = $derived.by(() => {
    const types = settings?.kpi_types ?? [];
    const set = new Set(types);
    if (formType && !set.has(formType)) set.add(formType);
    return [...set];
  });

  /** Distinct types from existing KPIs (for the filter dropdown). */
  let allTypes = $derived.by(() => {
    const set = new Set<string>();
    for (const k of kpis) set.add(k.kpi_type);
    return [...set].sort();
  });

  // ===========================================================================
  // Actions
  // ===========================================================================

  async function refresh() {
    try {
      const [list, s, t] = await Promise.all([
        listKpis(),
        getSettings(),
        listKpiTargets(selectedYear),
      ]);
      kpis = list;
      settings = s;
      targets = t;
      errorMsg = null;
    } catch (e) {
      errorMsg = `加载KPI失败: ${e}`;
    }
  }

  /** Reload targets when the year changes. */
  async function onYearChange() {
    try {
      targets = await listKpiTargets(selectedYear);
    } catch (e) {
      errorMsg = `加载目标失败: ${e}`;
    }
  }

  function startAdd() {
    formOpen = true;
    editingId = null;
    formYear = selectedYear;
    formType = settings?.kpi_types[0] ?? "";
    formTitle = "";
    formDetails = "";
    formPlannedAt = "";
  }

  function startEdit(kpi: Kpi) {
    formOpen = true;
    editingId = kpi.id;
    formYear = kpi.year;
    formType = kpi.kpi_type;
    formTitle = kpi.title;
    formDetails = kpi.details ?? "";
    formPlannedAt = kpi.planned_at ? rfc3339ToDateInput(kpi.planned_at) : "";
  }

  function cancelForm() {
    formOpen = false;
    editingId = null;
  }

  async function saveForm() {
    const title = formTitle.trim();
    if (!title) {
      errorMsg = "标题不能为空";
      return;
    }
    if (!formType) {
      errorMsg = "请选择KPI类型（在设置里配置）";
      return;
    }
    try {
      const params = {
        year: formYear,
        kpiType: formType,
        title,
        details: formDetails.trim() || null,
        plannedAt: dateInputToRfc3339(formPlannedAt),
      };
      if (editingId !== null) {
        await updateKpi({ id: editingId, ...params });
      } else {
        await addKpi(params);
      }
      formOpen = false;
      editingId = null;
      errorMsg = null;
      await refresh();
    } catch (e) {
      errorMsg = `保存失败: ${e}`;
    }
  }

  async function setStatus(id: number, status: KpiStatus) {
    try {
      await setKpiStatus(id, status);
      await refresh();
    } catch (e) {
      errorMsg = `状态更新失败: ${e}`;
    }
  }

  /** Save the target number for a type when the input loses focus. */
  async function saveTarget(type: string, value: string) {
    const count = parseInt(value, 10) || 0;
    try {
      await upsertKpiTarget({ year: selectedYear, kpiType: type, targetCount: count });
      targets = await listKpiTargets(selectedYear);
    } catch (e) {
      errorMsg = `目标保存失败: ${e}`;
    }
  }

  function requestDelete(id: number) {
    if (pendingDeleteId === id) {
      if (deleteTimer) {
        clearTimeout(deleteTimer);
        deleteTimer = null;
      }
      pendingDeleteId = null;
      void confirmDelete(id);
      return;
    }
    pendingDeleteId = id;
    if (deleteTimer) clearTimeout(deleteTimer);
    deleteTimer = setTimeout(() => {
      pendingDeleteId = null;
      deleteTimer = null;
    }, 3000);
  }

  async function confirmDelete(id: number) {
    try {
      await deleteKpi(id);
      await refresh();
    } catch (e) {
      errorMsg = `删除失败: ${e}`;
    }
  }

  $effect(() => {
    void refresh();
    return () => {
      if (deleteTimer) clearTimeout(deleteTimer);
    };
  });
</script>

<section class="kpis">
  <!-- Year selector + add button -->
  <div class="filter-bar">
    <div class="top-row">
      <select bind:value={selectedYear} onchange={onYearChange} aria-label="年度">
        {#each availableYears as yr (yr)}
          <option value={yr}>{yr} 年</option>
        {/each}
      </select>
      <input
        type="text"
        bind:value={searchText}
        placeholder="搜索标题或详情…"
        aria-label="搜索KPI"
      />
      <button class="add-btn" onclick={startAdd} disabled={formOpen}>
        + 新增
      </button>
    </div>

    <!-- Status chips -->
    <div class="chips">
      {#each [["all","全部"],["not_started","未进行"],["in_progress","进行中"],["completed","已完成"]] as [val,label] (val)}
        <button
          class="chip"
          class:active={statusFilter === val}
          onclick={() => (statusFilter = val)}
        >
          {label}
          {#if val === "not_started" && summary.notStarted > 0}
            <span class="count">{summary.notStarted}</span>
          {:else if val === "in_progress" && summary.inProgress > 0}
            <span class="count">{summary.inProgress}</span>
          {:else if val === "completed" && summary.completed > 0}
            <span class="count">{summary.completed}</span>
          {/if}
        </button>
      {/each}
      <select bind:value={typeFilter} aria-label="类型筛选" class="type-filter">
        <option value="all">全部类型</option>
        {#each allTypes as t (t)}
          <option value={t}>{t}</option>
        {/each}
      </select>
    </div>
  </div>

  <!-- Progress overview: per-type progress bars with editable targets -->
  {#if progressByType.length > 0}
    <div class="progress-section">
      {#each progressByType as p (p.type)}
        <div class="prog-row">
          <span class="prog-type">{p.type}</span>
          <div class="prog-bar-wrap">
            <div class="prog-bar" data-pct={p.percent}>
              <div
                class="prog-fill"
                style="width: {Math.min(p.percent, 100)}%"
                data-level={p.percent >= 100 ? "full" : p.percent >= 50 ? "mid" : "low"}
              ></div>
            </div>
            {#if p.target > 0}
              <span class="prog-text">
                {p.completed}/{p.target}
                ({p.percent}%)
                {#if p.percent > 100}<span class="over-badge">超额</span>{/if}
              </span>
            {:else}
              <span class="prog-text muted">已完成 {p.completed} · 未设置目标</span>
            {/if}
          </div>
          <input
            type="number"
            min="0"
            class="prog-target-input"
            value={p.target || ""}
            placeholder="目标"
            onchange={(e) => saveTarget(p.type, e.currentTarget.value)}
            aria-label="{p.type} 目标数"
          />
        </div>
      {/each}
    </div>
  {/if}

  <!-- Summary bar -->
  {#if yearKpis.length > 0}
    <div class="summary">
      未进行 {summary.notStarted}
      · 进行中 {summary.inProgress}
      · 已完成 {summary.completed}
    </div>
  {/if}

  <!-- Add form (shown at top when in add mode) -->
  {#if formOpen && editingId === null}
    {@render formFields()}
  {/if}

  <!-- KPI card list -->
  <div class="list">
    {#each filteredKpis as k (k.id)}
      {#if formOpen && editingId === k.id}
        {@render formFields()}
      {:else}
        <div class="kpi-card" data-status={k.status}>
          <div class="card-head">
            <span class="status-dot" data-status={k.status}></span>
            <span class="title" class:done={k.status === "completed"}>
              {k.title}
            </span>
            <span class="type-badge">{k.kpi_type}</span>
            {#if k.planned_at}
              <span class="planned-date">{formatDate(k.planned_at)}</span>
            {/if}
          </div>

          <div class="card-bottom">
            <span class="card-details" title={k.details ?? ""}>{k.details ?? ""}</span>
            <div class="card-actions">
              {#if k.status === "not_started"}
                <button class="act" onclick={() => setStatus(k.id, "in_progress")}>开始</button>
              {/if}
              {#if k.status !== "completed"}
                <button class="act" onclick={() => setStatus(k.id, "completed")}>完成</button>
              {/if}
              <button class="act" onclick={() => startEdit(k)}>编辑</button>
              <button
                class="act del"
                class:confirm={pendingDeleteId === k.id}
                onclick={() => requestDelete(k.id)}
              >
                {pendingDeleteId === k.id ? "确认?" : "删除"}
              </button>
            </div>
          </div>
        </div>
      {/if}
    {/each}

    {#if filteredKpis.length === 0 && !formOpen}
      <div class="empty">
        {#if yearKpis.length === 0}
          <p>{selectedYear} 年还没有KPI事项</p>
          <button class="add-btn" onclick={startAdd}>+ 添加第一条</button>
        {:else}
          <p>没有符合条件的KPI</p>
        {/if}
      </div>
    {/if}
  </div>

  {#if errorMsg}
    <div class="error">{errorMsg}</div>
  {/if}
</section>

{#snippet formFields()}
  <div class="kpi-form">
    <div class="form-row">
      <label>
        <span>年度</span>
        <input
          type="number"
          bind:value={formYear}
          aria-label="年度"
          class="form-year"
        />
      </label>
      <label>
        <span>KPI类型</span>
        <select bind:value={formType} aria-label="KPI类型">
          <option value="">请选择</option>
          {#each typeOptions as t (t)}
            <option value={t}>{t}</option>
          {/each}
        </select>
      </label>
    </div>
    <input
      type="text"
      bind:value={formTitle}
      placeholder="事项标题（必填）"
      aria-label="KPI标题"
      class="form-title"
    />
    <textarea
      bind:value={formDetails}
      placeholder="事项详情（可选）"
      aria-label="事项详情"
      rows="2"
    ></textarea>
    <div class="form-row">
      <label>
        <span>计划完成时间</span>
        <input
          type="date"
          value={formPlannedAt}
          oninput={(e) => (formPlannedAt = e.currentTarget.value)}
        />
      </label>
    </div>
    <div class="form-actions">
      <button class="act cancel" onclick={cancelForm}>取消</button>
      <button class="act save" onclick={saveForm} disabled={!formTitle.trim()}>
        保存
      </button>
    </div>
  </div>
{/snippet}

<style>
  .kpis {
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

  .top-row {
    display: flex;
    gap: var(--space-2);
    align-items: center;
  }

  .top-row select {
    flex-shrink: 0;
    font-size: 0.8rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--bg);
    color: var(--ink);
    transition: border-color var(--t-fast);
  }

  .top-row select:focus {
    border-color: var(--acc);
    outline: none;
  }

  .top-row input {
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

  .top-row input:focus {
    border-color: var(--acc);
    outline: none;
  }

  .top-row input::placeholder {
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

  .chip.active .count {
    opacity: 0.95;
  }

  .type-filter {
    flex-shrink: 0;
    font-size: 0.72rem;
    padding: 3px var(--space-2);
    border: 1px solid var(--bd);
    border-radius: var(--r-chip);
    font-family: inherit;
    background: var(--surface);
    color: var(--ink);
    transition: border-color var(--t-fast);
  }

  .type-filter:focus {
    border-color: var(--acc);
    outline: none;
  }

  /* -- progress section -- */

  .progress-section {
    flex-shrink: 0;
    padding: var(--space-2) var(--space-3);
    background: var(--bg);
    border-bottom: 1px solid var(--bd);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .prog-row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .prog-type {
    flex-shrink: 0;
    width: 80px;
    font-size: 0.72rem;
    color: var(--ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .prog-bar-wrap {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .prog-bar {
    height: 8px;
    background: var(--bd);
    border-radius: 4px;
    overflow: hidden;
  }

  .prog-fill {
    height: 100%;
    border-radius: 4px;
    transition: width var(--t-base);
  }

  .prog-fill[data-level="full"] {
    background: var(--ok);
  }

  .prog-fill[data-level="mid"] {
    background: var(--acc);
  }

  .prog-fill[data-level="low"] {
    background: var(--warn);
  }

  .prog-text {
    font-size: 0.65rem;
    color: var(--mut);
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .prog-text.muted {
    font-style: italic;
  }

  .over-badge {
    font-size: 0.6rem;
    padding: 0 4px;
    border-radius: var(--r-chip);
    background: var(--ok);
    color: #fff;
    font-weight: 500;
  }

  .prog-target-input {
    flex-shrink: 0;
    width: 56px;
    font-size: 0.72rem;
    padding: 2px 4px;
    border: 1px solid var(--bd);
    border-radius: var(--r-code);
    font-family: inherit;
    background: var(--surface);
    color: var(--ink);
    text-align: center;
    transition: border-color var(--t-fast);
  }

  .prog-target-input:focus {
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

  /* -- list -- */

  .list {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-2) var(--space-3);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  /* -- KPI card -- */

  .kpi-card {
    background: var(--surface);
    border-radius: var(--r-bubble);
    padding: var(--space-2) var(--space-3);
    border-left: 3px solid var(--mut);
    transition: opacity var(--t-base);
  }

  .kpi-card[data-status="in_progress"] {
    background: var(--warn-soft);
    border-left-color: var(--warn);
  }

  .kpi-card[data-status="completed"] {
    background: var(--ok-soft);
    border-left-color: var(--ok);
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

  .type-badge {
    flex-shrink: 0;
    font-size: 0.65rem;
    padding: 1px 6px;
    border-radius: var(--r-chip);
    background: var(--acc-soft);
    color: var(--acc);
    font-weight: 500;
  }

  .planned-date {
    flex-shrink: 0;
    font-size: 0.68rem;
    color: var(--mut);
    white-space: nowrap;
  }

  .card-bottom {
    margin-top: var(--space-1);
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .card-details {
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

  .kpi-form {
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

  .form-row select,
  .form-row input[type="number"],
  .form-row input[type="date"] {
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
  .form-row input:focus {
    border-color: var(--acc);
    outline: none;
  }

  .kpi-form textarea {
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

  .kpi-form textarea:focus {
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
