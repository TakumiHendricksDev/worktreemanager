<script lang="ts">
  /**
   * Database explorer and query console.
   *
   * Worktree profiles are keyed by worktree; project profiles are keyed by project. Keeping both
   * in this always-mounted surface means a local database never bleeds into another worktree while
   * shared TEST/STAGING/PROD sessions survive ordinary worktree navigation.
   */
  import { onMount, tick, untrack } from 'svelte';

  import { commands } from '../ipc/commands';
  import {
    errorMessage,
    type DatabaseColumn,
    type DatabaseConnection,
    type DatabaseRelation,
    type DatabaseSession,
    type QueryResult,
  } from '../ipc/types';
  import {
    quoteIdentifier,
    sortClause,
    sortOf,
    toCsv,
    type SelectedCells,
  } from '../database-grid';
  import { databaseConsole } from '../state/database-console.svelte';
  import { sessions } from '../state/sessions.svelte';
  import { workspace } from '../state/workspace.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';
  import DatabaseFilters from './DatabaseFilters.svelte';
  import DatabaseResult from './DatabaseResult.svelte';
  import SqlEditor from './SqlEditor.svelte';

  const {
    visible,
    onsessions,
  }: {
    /** Hidden rather than unmounted so shared consoles and local table positions remain intact. */
    visible: boolean;
    /** Show the worktree's panes — where a selection sent to an agent lands. */
    onsessions: () => void;
  } = $props();

  type Mode = 'data' | 'query';

  interface ConnectionState {
    session: DatabaseSession | null;
    schemas: string[];
    expandedSchemas: string[];
    relations: Record<string, DatabaseRelation[]>;
    loadingSchemas: string[];
    selectedRelation: DatabaseRelation | null;
    columns: DatabaseColumn[];
    mode: Mode;
    tableResult: QueryResult | null;
    queryResult: QueryResult | null;
    /** The statement `queryResult` came from, which the editor may no longer hold. */
    querySql: string;
    /** Whether `queryResult` came from a read-only run — a reply's SQL — so its footer can say so. */
    queryReadOnly: boolean;
    /** A reply's SQL that arrived before Connect was pressed, to run once it has been. */
    pendingRun: string | null;
    sql: string;
    relationFilter: string;
    offset: number;
    limit: number;
    /** The WHERE and ORDER BY the page on screen was fetched with. Empty when there is none. */
    filter: string;
    orderBy: string;
    connecting: boolean;
    loadingTable: boolean;
    runningQuery: boolean;
    productionConsoleUnlocked: boolean;
    error: string | null;
  }

  let connectionLists = $state<Record<string, DatabaseConnection[]>>({});
  let selectedProfiles = $state<Record<string, string>>({});
  let listLoading = $state<Record<string, boolean>>({});
  let listErrors = $state<Record<string, string | null>>({});
  let states = $state<Record<string, ConnectionState>>({});
  let selectedSql = $state('');
  const fetchingLists = new Set<string>();
  const exampleConfig = `[database.local]
engine = "postgres"
host = "127.0.0.1"
port = "{{ env.DB_PORT }}"
name = "{{ env.DB_NAME }}"
user = "{{ env.DB_USER }}"
password = "{{ env.DB_PASSWORD }}"`;

  const projectId = $derived(workspace.activeProjectId);
  const worktreeId = $derived(workspace.selectedWorktreeId);
  const contextKey = $derived(
    projectId && worktreeId ? `${projectId}:${worktreeId}` : null,
  );
  const connections = $derived(contextKey ? (connectionLists[contextKey] ?? []) : []);
  const selectedProfile = $derived(contextKey ? (selectedProfiles[contextKey] ?? '') : '');
  const connection = $derived(
    connections.find((candidate) => candidate.id === selectedProfile) ?? null,
  );
  const stateKey = $derived(
    connection && projectId && worktreeId
      ? connection.scope === 'project'
        ? `${projectId}:project:${connection.id}`
        : `${projectId}:${worktreeId}:${connection.id}`
      : null,
  );
  const current = $derived(stateKey ? (states[stateKey] ?? null) : null);
  const productionLocked = $derived(
    connection?.environment === 'production' &&
      current?.session?.access === 'read_write' &&
      !current.productionConsoleUnlocked,
  );
  const runShortcut =
    document.documentElement.dataset.platform === 'linux' ? 'Ctrl Enter' : '⌘ Enter';

  function newState(): ConnectionState {
    return {
      session: null,
      schemas: [],
      expandedSchemas: [],
      relations: {},
      loadingSchemas: [],
      selectedRelation: null,
      columns: [],
      mode: 'data',
      tableResult: null,
      queryResult: null,
      querySql: '',
      queryReadOnly: false,
      pendingRun: null,
      sql: '',
      relationFilter: '',
      offset: 0,
      limit: 100,
      filter: '',
      orderBy: '',
      connecting: false,
      loadingTable: false,
      runningQuery: false,
      productionConsoleUnlocked: false,
      error: null,
    };
  }

  function ensureState(key: string): ConnectionState {
    if (!states[key]) states[key] = newState();
    return states[key];
  }

  function keyFor(
    candidate: DatabaseConnection,
    project: string,
    worktree: string,
  ): string {
    return candidate.scope === 'project'
      ? `${project}:project:${candidate.id}`
      : `${project}:${worktree}:${candidate.id}`;
  }

  async function loadConnections(
    project: string,
    worktree: string,
    key: string,
  ): Promise<void> {
    if (fetchingLists.has(key)) return;
    fetchingLists.add(key);
    listLoading[key] = true;
    listErrors[key] = null;
    try {
      const loaded = await commands.listDatabaseConnections(project, worktree);
      connectionLists[key] = loaded;
      const prior = selectedProfiles[key] ?? '';
      const chosen = loaded.some((item) => item.id === prior)
        ? prior
        : (loaded.find((item) => item.available)?.id ?? loaded[0]?.id ?? '');
      selectedProfiles[key] = chosen;
      const selected = loaded.find((item) => item.id === chosen);
      if (selected) ensureState(keyFor(selected, project, worktree));
    } catch (error) {
      listErrors[key] = errorMessage(error);
    } finally {
      fetchingLists.delete(key);
      listLoading[key] = false;
    }
  }

  function refreshConnections(): void {
    if (!projectId || !worktreeId || !contextKey) return;
    void loadConnections(projectId, worktreeId, contextKey);
  }

  $effect(() => {
    const project = projectId;
    const worktree = worktreeId;
    const key = contextKey;
    if (!visible || !project || !worktree || !key) return;
    void loadConnections(project, worktree, key);
  });

  onMount(() => {
    const onFocus = () => {
      if (visible) refreshConnections();
    };
    window.addEventListener('focus', onFocus);
    return () => window.removeEventListener('focus', onFocus);
  });

  let tableGrid = $state<ReturnType<typeof DatabaseResult> | null>(null);
  let queryGrid = $state<ReturnType<typeof DatabaseResult> | null>(null);

  /*
   * ⌘F finds in the grid while this view is showing.
   *
   * The sidebar has owned ⌘F everywhere, focusing its worktree filter — but with a table on screen
   * that is not what anyone reaching for find means. Capture phase, so this runs before the
   * sidebar's listener and can stop it. Two places keep their own: the SQL editor, where
   * CodeMirror's search panel is find, and the sidebar itself.
   */
  onMount(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!visible || !(event.metaKey || event.ctrlKey) || event.key !== 'f') return;
      if (event.shiftKey || event.altKey) return;
      const target = event.target as Element | null;
      if (target?.closest?.('.cm-editor, .c-sidebar')) return;
      if (document.querySelector('[aria-modal="true"]')) return;
      const grid = current?.mode === 'query' ? queryGrid : tableGrid;
      if (!grid) return;
      event.preventDefault();
      event.stopPropagation();
      void grid.openFind();
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  });

  function chooseProfile(event: Event): void {
    const id = (event.currentTarget as HTMLSelectElement).value;
    if (!contextKey || !projectId || !worktreeId) return;
    selectedProfiles[contextKey] = id;
    const selected = connections.find((item) => item.id === id);
    if (selected) ensureState(keyFor(selected, projectId, worktreeId));
  }

  async function connect(): Promise<void> {
    if (!connection || !current || !projectId || !worktreeId || !connection.available)
      return;
    current.connecting = true;
    current.error = null;
    try {
      current.session = await commands.connectDatabase(
        projectId,
        worktreeId,
        connection.id,
      );
      current.schemas = (await commands.databaseSchemas(current.session.id)).map(
        (schema) => schema.name,
      );
    } catch (error) {
      current.error = errorMessage(error);
    } finally {
      current.connecting = false;
    }
    // The Run pressed in the reply, carried out now that there is a connection. Connect was the
    // click that crossed the trust boundary; asking for a second one would add nothing.
    const pending = current.pendingRun;
    if (current.session && pending) {
      current.pendingRun = null;
      current.mode = 'query';
      await runQuery(pending, true);
    }
  }

  async function disconnect(): Promise<void> {
    if (!current?.session) return;
    const session = current.session.id;
    current.error = null;
    try {
      await commands.disconnectDatabase(session);
      current.session = null;
      current.schemas = [];
      current.expandedSchemas = [];
      current.relations = {};
      current.selectedRelation = null;
      current.columns = [];
      current.tableResult = null;
      current.queryResult = null;
      current.relationFilter = '';
      current.productionConsoleUnlocked = false;
    } catch (error) {
      current.error = errorMessage(error);
    }
  }

  async function loadSchemaRelations(
    state: ConnectionState,
    schema: string,
  ): Promise<void> {
    const sessionId = state.session?.id;
    if (!sessionId || state.relations[schema] || state.loadingSchemas.includes(schema))
      return;

    state.loadingSchemas = [...state.loadingSchemas, schema];
    state.error = null;
    try {
      const relations = await commands.databaseRelations(sessionId, schema);
      if (state.session?.id === sessionId) state.relations[schema] = relations;
    } catch (error) {
      if (state.session?.id === sessionId) state.error = errorMessage(error);
    } finally {
      state.loadingSchemas = state.loadingSchemas.filter((name) => name !== schema);
    }
  }

  async function toggleSchema(schema: string): Promise<void> {
    const state = current;
    if (!state?.session) return;
    if (state.expandedSchemas.includes(schema)) {
      state.expandedSchemas = state.expandedSchemas.filter((name) => name !== schema);
      return;
    }
    state.expandedSchemas = [...state.expandedSchemas, schema];
    await loadSchemaRelations(state, schema);
  }

  function filterRelations(event: Event): void {
    const state = current;
    if (!state) return;
    state.relationFilter = (event.currentTarget as HTMLInputElement).value;
    if (!state.relationFilter.trim()) return;

    // Searching must cover collapsed schemas too; otherwise an apparently global filter would
    // quietly omit every table the user had not manually opened first.
    void Promise.all(state.schemas.map((schema) => loadSchemaRelations(state, schema)));
  }

  function filteredRelations(state: ConnectionState, schema: string): DatabaseRelation[] {
    const query = state.relationFilter.trim().toLowerCase();
    const relations = state.relations[schema] ?? [];
    if (!query) return relations;
    if (schema.toLowerCase().includes(query)) return relations;
    return relations.filter((relation) =>
      `${relation.schema}.${relation.name} ${relation.kind}`.toLowerCase().includes(query),
    );
  }

  function filteredSchemas(state: ConnectionState): string[] {
    if (!state.relationFilter.trim()) return state.schemas;
    return state.schemas.filter(
      (schema) =>
        state.loadingSchemas.includes(schema) ||
        !state.relations[schema] ||
        filteredRelations(state, schema).length > 0,
    );
  }

  function clearRelationFilter(): void {
    if (current) current.relationFilter = '';
  }

  function relationFilterKeydown(event: KeyboardEvent): void {
    if (event.key !== 'Escape' || !current?.relationFilter) return;
    event.preventDefault();
    clearRelationFilter();
  }

  async function selectRelation(relation: DatabaseRelation): Promise<void> {
    if (!current?.session) return;
    current.selectedRelation = relation;
    current.mode = 'data';
    current.offset = 0;
    current.filter = '';
    current.orderBy = '';
    if (!current.sql.trim()) {
      current.sql = `SELECT *\nFROM "${relation.schema.replaceAll('"', '""')}"."${relation.name.replaceAll('"', '""')}"\nLIMIT 100;`;
    }
    current.loadingTable = true;
    current.error = null;
    try {
      const [columns, result] = await Promise.all([
        commands.databaseColumns(current.session.id, relation.schema, relation.name),
        commands.databaseTablePage(current.session.id, {
          schema: relation.schema,
          table: relation.name,
          offset: 0,
          limit: current.limit,
          filter: null,
          orderBy: null,
        }),
      ]);
      current.columns = columns;
      current.tableResult = result;
    } catch (error) {
      current.error = errorMessage(error);
    } finally {
      current.loadingTable = false;
    }
  }

  /**
   * Fetch a page, with any of its WHERE, ORDER BY or offset replaced.
   *
   * The replacements are recorded only once a page has come back with them. A fragment the server
   * refuses then leaves the last good rows on screen, still described by the filter that fetched
   * them, and the refused text stays in its field to be fixed.
   */
  async function loadTable(
    next: { filter?: string; orderBy?: string; offset?: number } = {},
  ): Promise<void> {
    const state = current;
    const relation = state?.selectedRelation;
    if (!state?.session || !relation) return;
    const filter = next.filter ?? state.filter;
    const orderBy = next.orderBy ?? state.orderBy;
    const offset = next.offset ?? state.offset;
    state.loadingTable = true;
    state.error = null;
    try {
      state.tableResult = await commands.databaseTablePage(state.session.id, {
        schema: relation.schema,
        table: relation.name,
        offset,
        limit: state.limit,
        filter: filter || null,
        orderBy: orderBy || null,
      });
      state.filter = filter;
      state.orderBy = orderBy;
      state.offset = offset;
    } catch (error) {
      state.error = errorMessage(error);
    } finally {
      state.loadingTable = false;
    }
  }

  /** The header's toggle: ascending, descending, then no order — written into ORDER BY. */
  function sortTable(column: string): void {
    if (!current) return;
    const sort = sortOf(current.orderBy);
    const next =
      sort?.column !== column
        ? sortClause(column, 'asc')
        : sort.direction === 'asc'
          ? sortClause(column, 'desc')
          : '';
    void loadTable({ orderBy: next, offset: 0 });
  }

  /** "Filter by this value", added to whatever the page is already filtered by. */
  function filterBy(condition: string): void {
    if (!current) return;
    const filter = current.filter ? `(${current.filter}) AND ${condition}` : condition;
    void loadTable({ filter, offset: 0 });
  }

  function page(delta: number): void {
    if (!current) return;
    void loadTable({ offset: Math.max(0, current.offset + delta * current.limit) });
  }

  const tableSort = $derived(current ? sortOf(current.orderBy) : null);

  /** `readOnly` for a reply's SQL — see `QueryMode::ReadOnly` for what it guarantees. */
  async function runQuery(candidate?: string, readOnly = false): Promise<void> {
    const state = current;
    if (!state?.session || productionLocked) return;
    const sql = candidate?.trim() || state.sql.trim();
    if (!sql) return;
    state.runningQuery = true;
    state.error = null;
    try {
      state.queryResult = await commands.runDatabaseQuery(state.session.id, sql, readOnly);
      state.querySql = sql;
      state.queryReadOnly = readOnly;
    } catch (error) {
      // The refusal is the guarantee working, so say what to do next rather than only what failed.
      state.error = readOnly
        ? `${errorMessage(error)} — SQL from a reply runs read-only, one statement at a time. It is in the editor: press Run to execute it as written.`
        : errorMessage(error);
    } finally {
      state.runningQuery = false;
    }
  }

  let editor = $state<ReturnType<typeof SqlEditor> | null>(null);
  /** The last request acted on. Not state: it only stops one request being acted on twice. */
  let handledRequest = 0;

  /*
   * A reply's Run, arriving through `databaseConsole` once `App` has brought this worktree up.
   *
   * It waits for the worktree's profiles to have loaded, so that "no databases configured" is a
   * known answer rather than a list that has not arrived yet. The profile is whichever this
   * worktree has selected, which is its local one unless the user chose otherwise.
   */
  $effect(() => {
    const request = databaseConsole.request;
    if (!request || request.id === handledRequest) return;
    if (request.projectId !== projectId || request.worktreeId !== worktreeId) return;
    if (!contextKey || !(contextKey in connectionLists)) return;
    handledRequest = request.id;
    const state = current;
    if (!state) return;
    untrack(() => void receive(state, request.sql));
  });

  async function receive(state: ConnectionState, sql: string): Promise<void> {
    state.mode = 'query';
    state.error = null;
    if (!state.session) {
      // No editor is on screen without a connection, so the text goes into what it will open with.
      state.sql = state.sql.trim() ? `${state.sql.trimEnd()}\n\n${sql}` : sql;
      state.pendingRun = sql;
      return;
    }
    await tick();
    editor?.append(sql);
    // Locked, the SQL waits in the editor and the lock's banner says why nothing ran.
    if (!productionLocked) await runQuery(sql, true);
  }

  async function cancelQuery(): Promise<void> {
    if (!current?.session) return;
    try {
      await commands.cancelDatabaseQuery(current.session.id);
    } catch (error) {
      current.error = errorMessage(error);
    }
  }

  /** `postgres 16.4`: the engine, and the server's version without its build note. */
  function serverLabel(session: DatabaseSession): string {
    const version = session.serverVersion?.split(' ')[0];
    return version ? `${session.engine} ${version}` : session.engine;
  }

  /**
   * The page with each column's type from the relation's schema. Postgres results arrive untyped —
   * the simple query protocol carries names only — so without this the header tooltips and the
   * value panel had nothing to say about what a column holds.
   */
  const typedTable = $derived.by(() => {
    const result = current?.tableResult;
    if (!result) return null;
    const types = new Map(current.columns.map((column) => [column.name, column.typeName]));
    return {
      ...result,
      columns: result.columns.map((column) => ({
        ...column,
        typeName: column.typeName ?? types.get(column.name) ?? null,
      })),
    };
  });

  /** The agent a selection would go to: the focused one in this worktree, else its first. */
  const destination = $derived(worktreeId ? sessions.draftDestination(worktreeId) : null);

  /** More than this is a wall no one reads in a composer, and a context window's worth of rows. */
  const SEND_ROWS = 200;
  const SEND_CHARS = 30_000;

  /** The page on screen as a statement an agent can read and rerun. */
  function tablePageSql(state: ConnectionState, relation: DatabaseRelation): string {
    let sql = `SELECT *\nFROM ${quoteIdentifier(relation.schema)}.${quoteIdentifier(relation.name)}`;
    if (state.filter) sql += `\nWHERE ${state.filter}`;
    if (state.orderBy) sql += `\nORDER BY ${state.orderBy}`;
    return `${sql}\nLIMIT ${state.limit} OFFSET ${state.offset};`;
  }

  /**
   * Put a selection into the agent's composer — the statement it came from and the cells as CSV —
   * and show the pane. Nothing is sent: the user adds the question and presses Enter, which is the
   * direct action that sends database rows to a model provider.
   */
  function sendToAgent(selection: SelectedCells, from: Mode): void {
    const state = current;
    const target = destination;
    if (!state?.session || !target || !connection) return;

    let rows = selection.rows.slice(0, SEND_ROWS);
    let csv = toCsv([selection.header, ...rows]);
    while (csv.length > SEND_CHARS && rows.length > 1) {
      rows = rows.slice(0, Math.ceil(rows.length / 2));
      csv = toCsv([selection.header, ...rows]);
    }
    const total = selection.rows.length;
    const relation = from === 'data' ? state.selectedRelation : null;
    const source = relation ? `\`${relation.schema}.${relation.name}\`` : 'a query';
    const sql = relation ? tablePageSql(state, relation) : state.querySql;
    const shown =
      rows.length < total
        ? `the first ${rows.length.toLocaleString()} of ${total.toLocaleString()} selected rows`
        : `${total.toLocaleString()} row${total === 1 ? '' : 's'}`;

    const text = [
      `Here are ${shown} from ${source} on ${connection.label} (${connection.environment}):`,
      '',
      '```sql',
      sql.trim(),
      '```',
      '',
      '```csv',
      csv,
      '```',
      '',
      '',
    ].join('\n');
    sessions.insertDraft(target.id, text);
    onsessions();
  }

  function relationLabel(kind: DatabaseRelation['kind']): string {
    if (kind === 'materialized_view') return 'materialized view';
    return kind;
  }
</script>

<section
  class="c-database"
  class:is-hidden={!visible}
  id="database-view"
  aria-label="Database"
>
  {#if !projectId || !worktreeId}
    <div class="c-database__empty">Select a worktree to inspect its databases.</div>
  {:else if listLoading[contextKey ?? ''] && connections.length === 0}
    <div class="c-database__empty">Looking for database profiles…</div>
  {:else if listErrors[contextKey ?? '']}
    <div class="c-database__empty">
      <p>{listErrors[contextKey ?? '']}</p>
      <Button variant="neutral" size="sm" onclick={refreshConnections}>Retry</Button>
    </div>
  {:else if connections.length === 0}
    <div class="c-database__empty">
      <h2>No databases configured</h2>
      <p>
        Add a <code>[database.local]</code> profile to this repository's
        <code>wtm.toml</code>. Host, port, database name and credentials may use the same
        worktree environment tokens as the rest of Worktree Manager.
      </p>
      <pre class="c-database__example">{exampleConfig}</pre>
      <Button variant="neutral" size="sm" onclick={refreshConnections}
        >Reload profiles</Button
      >
    </div>
  {:else}
    <header class="c-database__toolbar">
      <label class="c-database__connection-picker">
        <span class="u-visually-hidden">Database connection</span>
        <select class="c-select" value={selectedProfile} onchange={chooseProfile}>
          {#each connections as candidate (candidate.id)}
            <option value={candidate.id} disabled={!candidate.available}>
              {candidate.label} — {candidate.target}
              {candidate.available ? '' : ' (unavailable)'}
            </option>
          {/each}
        </select>
      </label>

      {#if connection}
        <span class="c-badge c-badge--accent">{connection.environment}</span>
        <span class="c-badge">{connection.scope}</span>
        <span class="c-badge"
          >{connection.access === 'read_only' ? 'read only' : 'read/write'}</span
        >
        <!-- With the badges rather than beside Disconnect: it describes this connection, the way
             they do, and next to the button it read as the button's label. -->
        <span
          class="c-database__status"
          class:is-connected={!!current?.session}
          class:is-connecting={!current?.session && current?.connecting}
          title={current?.session ? serverLabel(current.session) : undefined}
        >
          {current?.session
            ? 'connected'
            : current?.connecting
              ? 'connecting…'
              : 'not connected'}
        </span>
      {/if}

      <span class="c-database__toolbar-spacer"></span>
      {#if current?.session}
        <Button variant="quiet" size="sm" onclick={() => void disconnect()}
          >Disconnect</Button
        >
      {:else}
        <Button
          variant="accent"
          size="sm"
          disabled={!connection?.available || current?.connecting}
          title={connection?.problem ?? undefined}
          onclick={() => void connect()}
        >
          {current?.connecting ? 'Connecting…' : 'Connect'}
        </Button>
      {/if}
    </header>

    {#if connection?.problem}
      <div class="c-database__notice">{connection.problem}</div>
    {/if}
    {#if current?.error}
      <div class="c-database__error" role="alert">
        <span>{current.error}</span>
        <Button
          variant="inline"
          size="sm"
          ariaLabel="Dismiss"
          onclick={() => (current.error = null)}
        >
          <Icon name="close" size={12} />
        </Button>
      </div>
    {/if}

    {#if current?.session}
      <div class="c-database__body">
        <aside class="c-database__explorer" aria-label="Database objects">
          <div class="c-database__explorer-title">
            <strong>{current.session.label}</strong>
            <span>{serverLabel(current.session)}</span>
          </div>
          <div class="c-database__search" role="search">
            <span class="c-database__search-icon"><Icon name="search" size={14} /></span>
            <label class="u-visually-hidden" for="database-relation-search"
              >Filter tables and views</label
            >
            <input
              id="database-relation-search"
              class="c-database__search-input"
              type="search"
              value={current.relationFilter}
              oninput={filterRelations}
              onkeydown={relationFilterKeydown}
              placeholder="Filter tables and views"
              autocomplete="off"
              spellcheck="false"
            />
            {#if current.relationFilter}
              <button
                class="c-database__search-clear"
                title="Clear the filter"
                onclick={clearRelationFilter}
              >
                <Icon name="close" size={12} />
                <span class="u-visually-hidden">Clear the filter</span>
              </button>
            {/if}
          </div>
          <div class="c-database__tree">
            {#each filteredSchemas(current) as schema (schema)}
              {@const expanded = current.expandedSchemas.includes(schema)}
              {@const filtering = current.relationFilter.trim().length > 0}
              {@const shownExpanded = expanded || filtering}
              {@const relations = filteredRelations(current, schema)}
              <button
                class="c-database__tree-item c-database__tree-item--schema"
                aria-expanded={shownExpanded}
                onclick={() => void toggleSchema(schema)}
              >
                <Icon name={shownExpanded ? 'chevron-down' : 'chevron-right'} size={12} />
                <span>{schema}</span>
              </button>
              {#if shownExpanded}
                <div class="c-database__relations">
                  {#if current.loadingSchemas.includes(schema)}
                    <span class="c-database__tree-note">Loading…</span>
                  {:else if relations.length === 0}
                    <span class="c-database__tree-note">No tables or views</span>
                  {:else}
                    {#each relations as relation (`${relation.schema}.${relation.name}`)}
                      <button
                        class="c-database__tree-item c-database__tree-item--relation"
                        class:is-active={current.selectedRelation?.schema ===
                          relation.schema &&
                          current.selectedRelation?.name === relation.name}
                        title={`${relationLabel(relation.kind)} ${relation.schema}.${relation.name}`}
                        onclick={() => void selectRelation(relation)}
                      >
                        <span class="c-database__relation-mark" aria-hidden="true"></span>
                        <span>{relation.name}</span>
                      </button>
                    {/each}
                  {/if}
                </div>
              {/if}
            {:else}
              <span class="c-database__tree-note">No matching tables or views</span>
            {/each}
          </div>
        </aside>

        <div class="c-database__workspace">
          <nav class="c-tabs" aria-label="Database workspace">
            <button
              class="c-tabs__tab"
              class:is-active={current.mode === 'data'}
              aria-pressed={current.mode === 'data'}
              disabled={!current.selectedRelation}
              onclick={() => (current.mode = 'data')}
            >
              Table data
            </button>
            <button
              class="c-tabs__tab"
              class:is-active={current.mode === 'query'}
              aria-pressed={current.mode === 'query'}
              onclick={() => (current.mode = 'query')}
            >
              Query console
            </button>
          </nav>

          {#if current.mode === 'data'}
            {#if current.selectedRelation}
              <div class="c-database__object-head">
                <div>
                  <strong
                    >{current.selectedRelation.schema}.{current.selectedRelation
                      .name}</strong
                  >
                  <span>{relationLabel(current.selectedRelation.kind)}</span>
                </div>
                <Button variant="quiet" size="sm" onclick={() => void loadTable()}
                  >Refresh</Button
                >
              </div>

              <div class="c-database__columns" aria-label="Columns">
                {#each current.columns as column (column.name)}
                  <div class="c-database__column" title={column.default ?? undefined}>
                    <code>{column.name}</code>
                    <span>{column.typeName}</span>
                    {#if column.primaryKey}<span class="c-badge c-badge--accent">PK</span
                      >{/if}
                    {#if column.nullable}<span class="c-status--subtle">nullable</span>{/if}
                  </div>
                {/each}
              </div>

              <DatabaseFilters
                filter={current.filter}
                orderBy={current.orderBy}
                onapply={(next) => void loadTable({ ...next, offset: 0 })}
              />

              <div class="c-database__results">
                {#if current.loadingTable}
                  <div class="c-database__result-empty">Loading table rows…</div>
                {:else if typedTable}
                  <DatabaseResult
                    bind:this={tableGrid}
                    findPlaceholder="Find on this page"
                    result={typedTable}
                    rowOffset={current.offset}
                    sortable={true}
                    sortColumn={tableSort?.column ?? null}
                    sortDirection={tableSort?.direction ?? null}
                    onfilter={filterBy}
                    onsort={sortTable}
                    relation={current.selectedRelation}
                    sendLabel={destination?.label ?? null}
                    onsend={(selection) => sendToAgent(selection, 'data')}
                  />
                {/if}
              </div>
              <footer class="c-database__pager">
                <Button
                  variant="quiet"
                  size="sm"
                  disabled={current.offset === 0 || current.loadingTable}
                  onclick={() => page(-1)}>Previous</Button
                >
                <span>
                  rows {current.offset + 1}–{current.offset +
                    (current.tableResult?.rows.length ?? 0)}
                </span>
                <Button
                  variant="quiet"
                  size="sm"
                  disabled={(current.tableResult?.rows.length ?? 0) < current.limit ||
                    current.loadingTable}
                  onclick={() => page(1)}>Next</Button
                >
              </footer>
            {:else}
              <div class="c-database__empty">
                Choose a table or view from the schema tree.
              </div>
            {/if}
          {:else}
            <div class="c-database__query-toolbar">
              <Button
                variant="accent"
                size="sm"
                disabled={current.runningQuery || !current.sql.trim() || productionLocked}
                title={`Run selected SQL, or the whole editor (${runShortcut})`}
                onclick={() => void runQuery(selectedSql)}>Run</Button
              >
              {#if current.runningQuery}
                <Button
                  variant="danger-outline"
                  size="sm"
                  onclick={() => void cancelQuery()}
                >
                  Cancel
                </Button>
              {/if}
              <span class="c-database__query-hint">
                {runShortcut} runs · Tab indents · Esc then Tab leaves
              </span>
              <span class="c-database__toolbar-spacer"></span>
              <span class="c-status--subtle">up to 500 rows</span>
            </div>

            {#if productionLocked}
              <div class="c-database__production">
                <div>
                  <strong>Production query console locked</strong>
                  <p>
                    Table browsing remains available. Unlock only if you intend to run
                    arbitrary SQL.
                  </p>
                </div>
                <Button
                  variant="danger-outline"
                  size="sm"
                  onclick={() => (current.productionConsoleUnlocked = true)}
                  >Enable for this session</Button
                >
              </div>
            {/if}

            <SqlEditor
              bind:this={editor}
              bind:value={current.sql}
              bind:selection={selectedSql}
              onrun={(sql) => void runQuery(sql)}
            />
            <div class="c-database__results c-database__results--query">
              {#if current.runningQuery}
                <div class="c-database__result-empty">Running query…</div>
              {:else if current.queryResult}
                <DatabaseResult
                  bind:this={queryGrid}
                  result={current.queryResult}
                  note={current.queryReadOnly ? 'read-only run' : null}
                  sendLabel={destination?.label ?? null}
                  onsend={(selection) => sendToAgent(selection, 'query')}
                />
              {:else}
                <div class="c-database__result-empty">Query results appear here.</div>
              {/if}
            </div>
          {/if}
        </div>
      </div>
    {:else if connection?.available}
      <div class="c-database__empty">
        {#if current?.pendingRun}
          <p>Connect to run the query from the agent's reply. It runs read-only.</p>
        {:else}
          <p>Connect to inspect schemas, browse table rows, or open a query console.</p>
        {/if}
        {#if connection.scope === 'worktree'}
          <span class="c-status--subtle"
            >This connection belongs only to the selected worktree.</span
          >
        {:else}
          <span class="c-status--subtle"
            >This connection persists while you move between this project's worktrees.</span
          >
        {/if}
      </div>
    {/if}
  {/if}
</section>
