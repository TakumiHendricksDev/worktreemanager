<script lang="ts">
  /**
   * Home's left column: every project, its worktrees, and every agent session running in them, as
   * one compact location tree.
   *
   * It takes the sidebar's place while Home is up rather than sitting beside it. The sidebar lists
   * one project's worktrees; this lists all of them, so the two side by side would be two trees of
   * the same checkouts. The sidebar stays mounted underneath, hidden, and comes back exactly as it
   * was.
   *
   * What goes where is `fleet.ts`; this renders its rows and owns only what needs the DOM — the
   * roving focus and the menus. Like the sidebar it is a
   * genuine `role="tree"`: focus lives on the rows, and the tree listens for the keys that bubble.
   */
  import { onMount, tick, untrack } from 'svelte';

  import { HOME_KEY, type FleetRow, type FleetCounts, type FleetOnly } from '../fleet';
  import { heading, item, popUp, separator, under, type MenuEntry } from '../native-menu';
  import { treePrefs } from '../state/tree-prefs.svelte';
  import { attention } from '../state/attention.svelte';
  import { fleet } from '../state/fleet.svelte';
  import { worktreeRemoval } from '../state/worktree-removal.svelte';
  import { sessions, type Pane } from '../state/sessions.svelte';
  import { view } from '../state/view.svelte';
  import { workspace } from '../state/workspace.svelte';
  import { STATUS_WORD, type PaneStatus } from '../status';
  import CloseSessionDialog from './CloseSessionDialog.svelte';
  import { resolveWorktreeTarget, worktreeActions } from '../worktree-menu';
  import { worktreeLabel } from '../worktree-label';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';
  import SessionDot from './ui/SessionDot.svelte';

  const {
    visible,
    onhome,
    onopenworktree,
    onnewworktree,
    onaddproject,
  }: {
    visible: boolean;
    /** The Home row was chosen: back to Home's own conversation. */
    onhome: () => void;
    /** Leave Home for a worktree. */
    onopenworktree: (projectId: string, worktreeId: string) => void | Promise<void>;
    onnewworktree: (projectId: string) => void;
    onaddproject: () => void;
  } = $props();

  const arrangement = $derived(fleet.arrangement);
  const rows = $derived(arrangement.rows);

  /** The row that holds the tab stop. Falls back to Home when its row goes away. */
  let focusKey = $state<string>(HOME_KEY);
  const tabKey = $derived(rows.some((row) => row.key === focusKey) ? focusKey : HOME_KEY);

  let treeEl = $state<HTMLDivElement | null>(null);
  let searchEl = $state<HTMLInputElement | null>(null);

  let previousRows: FleetRow[] = [];
  let treeOwnedFocus = false;
  $effect.pre(() => {
    const nextRows = rows;
    untrack(() => {
      if (!nextRows.some((row) => row.key === focusKey)) {
        const index = previousRows.findIndex((row) => row.key === focusKey);
        const level = previousRows[index]?.level ?? 1;
        const exists = (row: FleetRow) => nextRows.some((next) => next.key === row.key);
        const ancestor = previousRows
          .slice(0, index)
          .reverse()
          .find((row) => row.level < level && exists(row));
        const neighbor = previousRows.slice(index + 1).find(exists);
        const heldFocus = treeOwnedFocus;
        focusKey = ancestor?.key ?? neighbor?.key ?? HOME_KEY;
        if (heldFocus) focusRow(focusKey);
      }
      previousRows = nextRows;
    });
  });

  /** The tree's own ⌘F, while it is the one on screen. The sidebar's is gated the other way. */
  onMount(() => {
    const onKey = (event: KeyboardEvent) => {
      if (
        !visible ||
        event.defaultPrevented ||
        (event.target instanceof Element &&
          event.target.closest('input, textarea, [contenteditable], .cm-editor, .xterm'))
      )
        return;
      if ((event.metaKey || event.ctrlKey) && event.key === 'f') {
        event.preventDefault();
        searchEl?.focus();
        searchEl?.select();
      }
    };
    const onFocus = (event: FocusEvent) => {
      treeOwnedFocus = event.target instanceof Node && !!treeEl?.contains(event.target);
    };
    window.addEventListener('focusin', onFocus);
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('keydown', onKey);
      window.removeEventListener('focusin', onFocus);
    };
  });

  function selected(row: FleetRow): boolean {
    if (row.kind === 'home') return view.peeked === null && fleet.jobShown === null;
    if (row.kind === 'creation') return fleet.jobShown === row.job.id;
    return row.kind === 'session' && row.session.id === view.peeked;
  }

  function countDetail(row: Extract<FleetRow, { kind: 'project' | 'worktree' }>): string {
    const c: FleetCounts = row.counts;
    const visibleCount = rows.filter(
      (candidate) =>
        candidate.kind === 'session' &&
        (row.kind === 'project'
          ? candidate.session.projectId === row.project.id
          : candidate.session.worktreeId === row.worktree.id),
    ).length;
    const filtered = fleet.query.trim() !== '' || fleet.only !== null;
    return `${filtered ? `${visibleCount} of ` : ''}${c.total} sessions · ${c.attention} need you · ${c.working} running · ${c.done} unread completions · ${c.failed} failed`;
  }

  function statusOf(row: FleetRow): PaneStatus | null {
    if (row.kind === 'session') return row.session.status;
    if (row.kind === 'project') return row.project.usable ? row.status : 'attention';
    if (row.kind === 'worktree') return row.status;
    if (row.kind === 'creation')
      return row.job.phase === 'running'
        ? 'starting'
        : row.job.phase === 'created' || row.job.phase === 'removed'
          ? 'done'
          : 'failed';
    return null;
  }

  function titleOf(row: FleetRow): string {
    if (row.kind === 'worktree') return worktreeLabel(row.worktree).detail;
    if (row.kind === 'session')
      return [
        row.session.title,
        row.session.agent,
        row.session.model,
        STATUS_WORD[row.session.status],
        row.session.detail,
        row.session.openedFromHome ? 'Opened by Home' : null,
      ]
        .filter(Boolean)
        .join(' · ');
    if (row.kind === 'creation') return row.job.detail;
    if (row.kind === 'project')
      return row.project.usable ? countDetail(row) : 'Repository needs attention';
    return '';
  }

  function peek(paneId: string) {
    view.peek(paneId);
    // Looking at it, in the only sense Home has: the peek panel is now showing its transcript.
    sessions.markPaneSeen(paneId);
    attention.clearPane(paneId);
  }

  function activate(row: FleetRow) {
    focusKey = row.key;
    switch (row.kind) {
      case 'home':
        view.peek(null);
        fleet.jobShown = null;
        onhome();
        return;
      case 'project':
        fleet.toggle(row.key, !row.expanded);
        return;
      case 'settled':
      case 'idle':
        fleet.toggle(row.key, !row.expanded);
        return;
      case 'worktree':
        if (row.expanded !== null) fleet.toggle(row.key, !row.expanded);
        else onopenworktree(row.projectId, row.worktree.id);
        return;
      case 'session':
        if (row.session.kind === 'shell') {
          void Promise.resolve(
            onopenworktree(row.session.projectId, row.session.worktreeId),
          ).then(() => sessions.focus(row.session.worktreeId, row.session.id));
        } else peek(row.session.id);
        return;
      case 'creation':
        fleet.showJob(row.job.id);
        return;
    }
  }

  // ─────────────────────────────── menus ───────────────────────────────

  /**
   * The session whose "Close session?" is open, and the row focus goes to once it has gone.
   *
   * The neighbour is chosen when the menu is used, while the row is still in the tree: by the time
   * the close returns, it is not, and the focus it held would otherwise fall out of the tree.
   */
  let closing = $state<{ pane: Pane; next: string } | null>(null);

  function askToClose(pane: Pane, rowKey: string) {
    const at = rows.findIndex((row) => row.key === rowKey);
    const level = rows[at]?.level ?? 0;
    // Past its own delegated children, which the close takes with it.
    const after = rows.slice(at + 1).find((row) => row.level <= level);
    const next = after?.key ?? rows[at - 1]?.key ?? HOME_KEY;
    closing = { pane, next };
  }

  function closed(pane: Pane, next: string) {
    if (view.peeked === pane.id) view.peek(null);
    focusRow(rows.some((row) => row.key === next) ? next : HOME_KEY);
  }

  /** Start an agent in a worktree from Home. The pane is tiled there; Home peeks at it. */
  let menuEpoch = 0;
  async function startIn(
    event: MouseEvent | null,
    projectId: string,
    worktreeId: string,
    anchor: Element,
  ) {
    event?.stopPropagation();
    const epoch = ++menuEpoch;
    const at = event ? { x: event.clientX, y: event.clientY } : under(anchor);
    let options;
    let target;
    try {
      target = await resolveWorktreeTarget(projectId, worktreeId);
      options = await fleet.agentsFor(projectId);
    } catch (e) {
      sessions.error = `Could not open that worktree's menu: ${String(e)}`;
      return;
    }
    if (epoch !== menuEpoch || !visible || !anchor.isConnected) return;
    const entries: MenuEntry[] = [
      ...worktreeActions(target, (captured) => void worktreeRemoval.request(captured)),
      separator,
      heading('Start an agent here'),
      ...options.map((option) =>
        item(
          option.label,
          () =>
            void sessions
              .openAgent(projectId, worktreeId, option.id, 'right', undefined, option)
              .then((paneId) => {
                if (paneId) peek(paneId);
              }),
          option.available && option.offered,
        ),
      ),
      separator,
      item('Open this worktree', () => onopenworktree(projectId, worktreeId)),
    ];
    void popUp(entries, at);
  }

  function rowMenu(event: MouseEvent | null, row: FleetRow, anchor: Element) {
    menuEpoch += 1;
    if (row.kind === 'worktree') {
      void startIn(event, row.projectId, row.worktree.id, anchor);
      return;
    }
    if (row.kind === 'project') {
      void popUp(
        [
          item('New worktree…', () => onnewworktree(row.project.id), row.project.usable),
          item(
            'Open this project',
            () =>
              void workspace
                .selectProject(row.project.id)
                .then(() => view.show('worktree')),
          ),
        ],
        under(anchor),
      );
      return;
    }
    if (row.kind === 'session') {
      const pane = sessions.paneById(row.session.id);
      if (!pane) return;
      void popUp(
        [
          ...(pane.kind.kind === 'agent' ? [item('Peek', () => peek(pane.id))] : []),
          item('Open in its worktree', () =>
            onopenworktree(pane.projectId, pane.worktreeId),
          ),
          separator,
          item('Stop the turn', () => void sessions.interrupt(pane.id), pane.working),
          // Through the pane's own confirmation, which says what a close would end.
          item('Close session…', () => askToClose(pane, row.key)),
        ],
        under(anchor),
      );
    }
  }

  // ─────────────────────────────── keyboard ───────────────────────────────

  function navItems(): HTMLElement[] {
    return [...(treeEl?.querySelectorAll<HTMLElement>('[data-nav]') ?? [])];
  }

  function focusRow(key: string) {
    focusKey = key;
    void tick().then(() => {
      const target = navItems().find((el) => el.dataset.nav === key);
      target?.focus();
      target?.scrollIntoView({ block: 'nearest' });
    });
  }

  /** The nearest row above at a shallower level: the tree's parent, as the keyboard sees it. */
  function parentOf(index: number): FleetRow | null {
    const level = rows[index]?.level ?? 1;
    for (let at = index - 1; at >= 0; at -= 1) {
      if (rows[at]!.level < level) return rows[at]!;
    }
    return null;
  }

  function expandedOf(row: FleetRow): boolean | null {
    if (row.kind === 'home' || row.kind === 'creation') return null;
    return row.expanded;
  }

  function onKeydown(event: KeyboardEvent) {
    const key = (event.target as HTMLElement | null)?.closest<HTMLElement>('[data-nav]')
      ?.dataset.nav;
    const index = rows.findIndex((row) => row.key === key);
    if (index < 0) return;
    const row = rows[index]!;
    const expanded = expandedOf(row);
    switch (event.key) {
      case 'ArrowDown':
        if (index + 1 < rows.length) focusRow(rows[index + 1]!.key);
        break;
      case 'ArrowUp':
        if (index > 0) focusRow(rows[index - 1]!.key);
        break;
      case 'Home':
        focusRow(rows[0]!.key);
        break;
      case 'End':
        focusRow(rows[rows.length - 1]!.key);
        break;
      case 'ArrowRight':
        if (expanded === false) fleet.toggle(row.key, true);
        else if (expanded === true && rows[index + 1]) focusRow(rows[index + 1]!.key);
        else return;
        break;
      case 'ArrowLeft': {
        if (expanded === true) {
          fleet.toggle(row.key, false);
          break;
        }
        const parent = parentOf(index);
        if (parent) focusRow(parent.key);
        else return;
        break;
      }
      case 'Enter':
      case ' ':
        activate(row);
        break;
      case 'ContextMenu':
      case 'F10':
        if (event.key === 'F10' && !event.shiftKey) return;
        rowMenu(null, row, event.target as Element);
        break;
      default:
        return;
    }
    event.preventDefault();
  }
</script>

{#snippet twisty(row: FleetRow)}
  {@const expanded = expandedOf(row)}
  {#if expanded === null}
    <span class="c-fleet__twisty" aria-hidden="true"></span>
  {:else}
    <span class="c-fleet__twisty" aria-hidden="true">
      <Icon name={expanded ? 'chevron-down' : 'chevron-right'} size={12} />
    </span>
  {/if}
{/snippet}

<nav
  style:--fleet-row-height={treePrefs.density === 'compact' ? '26px' : '30px'}
  class="c-fleet"
  class:is-hidden={!visible}
  aria-label="Sessions everywhere"
>
  <div class="c-fleet__controls">
    <div class="c-search" role="search">
      <span class="c-search__icon"><Icon name="search" size={14} /></span>
      <label class="u-visually-hidden" for="fleet-search">Filter sessions</label>
      <input
        id="fleet-search"
        class="c-search__input"
        type="search"
        bind:this={searchEl}
        bind:value={fleet.query}
        placeholder="Filter sessions"
        autocomplete="off"
        spellcheck="false"
      />
      {#if fleet.query !== ''}
        <button
          class="c-search__clear"
          onclick={() => (fleet.query = '')}
          title="Clear the filter"
        >
          <Icon name="close" size={12} />
          <span class="u-visually-hidden">Clear the filter</span>
        </button>
      {/if}
    </div>
  </div>

  <div class="c-fleet__filter">
    <label class="u-visually-hidden" for="fleet-status">Session status</label>
    <select
      id="fleet-status"
      class="c-fleet__select"
      value={fleet.only ?? ''}
      onchange={(event) =>
        (fleet.only = (event.currentTarget.value || null) as FleetOnly | null)}
    >
      <option value="">All sessions</option>
      <option value="attention">Needs you</option>
      <option value="working">Running</option>
      <option value="done">Completed</option>
      <option value="failed">Failed</option>
    </select>
    {#if fleet.only !== null || fleet.query !== ''}
      <Button
        variant="quiet"
        size="sm"
        onclick={() => {
          fleet.only = null;
          fleet.query = '';
        }}>Clear</Button
      >
    {/if}
  </div>

  <div class="c-fleet__wrap">
    <div class="c-fleet__scroll">
      <!-- svelte-ignore a11y_interactive_supports_focus -->
      <div
        role="tree"
        aria-label="Sessions everywhere"
        aria-orientation="vertical"
        class="c-fleet__tree"
        bind:this={treeEl}
        onkeydown={onKeydown}
      >
        {#each rows as row (row.key)}
          {@const expanded = expandedOf(row)}
          {@const status = statusOf(row)}
          <!-- Keys are the tree's, which hears them bubble from the focused row; a click is the
               mouse's route to the same activation. -->
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <div
            role="treeitem"
            class="c-fleet__row c-fleet__row--{row.kind}"
            class:is-selected={selected(row)}
            class:has-key={row.kind === 'worktree' && !!row.worktree.issueKey}
            data-nav={row.key}
            title={titleOf(row)}
            aria-describedby={`fleet-detail-${row.key}`}
            tabindex={row.key === tabKey ? 0 : -1}
            aria-level={row.level}
            aria-expanded={expanded ?? undefined}
            aria-selected={selected(row)}
            style:--fleet-level={row.level}
            onclick={() => activate(row)}
            oncontextmenu={(event) => {
              event.preventDefault();
              event.currentTarget.focus();
              rowMenu(event, row, event.currentTarget);
            }}
            onfocus={() => {
              focusKey = row.key;
              fleet.focused = row.kind === 'session' ? row.session.id : null;
            }}
            onfocusout={(event) => {
              if (
                !(event.relatedTarget instanceof Node) ||
                !treeEl?.contains(event.relatedTarget)
              )
                fleet.focused = null;
            }}
          >
            <span id={`fleet-detail-${row.key}`} class="u-visually-hidden"
              >{titleOf(row)}</span
            >
            <span class="c-fleet__marker">
              {#if row.kind === 'home'}<Icon name="home" size={14} />
              {:else if status && status !== 'idle'}<SessionDot {status} labelled />{/if}
            </span>
            <span class="c-fleet__identity">
              {@render twisty(row)}
              {#if row.kind === 'home'}<span class="c-fleet__name">Home</span>
              {:else if row.kind === 'project'}<span class="c-fleet__name"
                  >{row.project.name}</span
                >
              {:else if row.kind === 'worktree'}
                {@const identity = worktreeLabel(row.worktree)}
                {#if identity.key}<span class="c-fleet__key">{identity.key}</span>{/if}
                <span class="c-fleet__name">{identity.name}</span>
              {:else if row.kind === 'settled'}<span class="c-fleet__name"
                  >Idle / finished</span
                >
              {:else if row.kind === 'idle'}<span class="c-fleet__name">No sessions</span>
              {:else if row.kind === 'creation'}<span class="c-fleet__name"
                  >{row.job.title}</span
                >
              {:else}
                <span class="c-fleet__name">{row.session.title}</span>
                <span class="c-fleet__provider"
                  >{row.session.agent.replace(' Code', '').replace(' Agent', '')}</span
                >
              {/if}
            </span>
            <span class="c-fleet__tag">
              {#if row.kind === 'project' || row.kind === 'worktree'}
                {#if row.counts.total > 0}<span
                    title={countDetail(row)}
                    aria-label={countDetail(row)}>{row.counts.total}</span
                  >{/if}
              {:else if row.kind === 'settled'}<span
                  aria-label={`${row.count} idle or finished sessions`}>{row.count}</span
                >
              {:else if row.kind === 'idle'}<span
                  aria-label={`${row.count} worktrees without sessions`}>{row.count}</span
                >{/if}
            </span>
            <span class="c-fleet__actions">
              {#if row.kind === 'project' || row.kind === 'worktree'}
                <Button
                  variant="quiet"
                  icon="sm"
                  title={row.kind === 'project'
                    ? `More for ${row.project.name}`
                    : `More for ${row.worktree.title}`}
                  ariaLabel={row.kind === 'project'
                    ? `More for ${row.project.name}`
                    : `More for ${row.worktree.title}`}
                  ariaHaspopup="menu"
                  tabindex={-1}
                  onclick={(event) => {
                    event.stopPropagation();
                    rowMenu(event, row, event.currentTarget as Element);
                  }}
                >
                  <Icon name="more" size={14} />
                </Button>
              {/if}
            </span>
          </div>
        {/each}
      </div>
    </div>
  </div>

  <div class="c-fleet__foot">
    <Button variant="neutral" full onclick={onaddproject}>
      <Icon name="plus" size={14} /><span class="c-fleet__add-label">Add a repository…</span
      >
    </Button>
  </div>
</nav>

<!-- Gone by another route while it was open — Home closed it, say — and there is nothing to ask. -->
{#if closing && sessions.paneById(closing.pane.id)}
  {@const { pane, next } = closing}
  <CloseSessionDialog
    {pane}
    onclose={() => (closing = null)}
    onclosed={() => closed(pane, next)}
  />
{/if}
