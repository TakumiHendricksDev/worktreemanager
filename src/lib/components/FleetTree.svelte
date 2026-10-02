<script lang="ts">
  /**
   * Home's left column: every project, its worktrees, and every agent session running in them, as
   * one tree — with wires drawn between sessions while one is talking to another.
   *
   * It takes the sidebar's place while Home is up rather than sitting beside it. The sidebar lists
   * one project's worktrees; this lists all of them, so the two side by side would be two trees of
   * the same checkouts. The sidebar stays mounted underneath, hidden, and comes back exactly as it
   * was.
   *
   * What goes where is `fleet.ts`; this renders its rows and owns only what needs the DOM — the
   * roving focus, the menus, and the measuring `FleetWires` does over it. Like the sidebar it is a
   * genuine `role="tree"`: focus lives on the rows, and the tree listens for the keys that bubble.
   */
  import { onMount, tick } from 'svelte';

  import { HOME_KEY, type FleetRow } from '../fleet';
  import { heading, item, popUp, separator, under, type MenuEntry } from '../native-menu';
  import { attention } from '../state/attention.svelte';
  import { fleet } from '../state/fleet.svelte';
  import { sessions, type Pane } from '../state/sessions.svelte';
  import { view } from '../state/view.svelte';
  import { workspace } from '../state/workspace.svelte';
  import { STATUS_WORD, type PaneStatus } from '../status';
  import CloseSessionDialog from './CloseSessionDialog.svelte';
  import FleetWires from './FleetWires.svelte';
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
    onopenworktree: (projectId: string, worktreeId: string) => void;
    onnewworktree: (projectId: string) => void;
    onaddproject: () => void;
  } = $props();

  const arrangement = $derived(fleet.arrangement);
  const rows = $derived(arrangement.rows);

  /** The row that holds the tab stop. Falls back to Home when its row goes away. */
  let focusKey = $state<string>(HOME_KEY);
  const tabKey = $derived(rows.some((row) => row.key === focusKey) ? focusKey : HOME_KEY);

  let treeEl = $state<HTMLDivElement | null>(null);
  let scrollEl = $state<HTMLDivElement | null>(null);
  let searchEl = $state<HTMLInputElement | null>(null);

  /** The tree's own ⌘F, while it is the one on screen. The sidebar's is gated the other way. */
  onMount(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!visible || event.defaultPrevented) return;
      if ((event.metaKey || event.ctrlKey) && event.key === 'f') {
        event.preventDefault();
        searchEl?.focus();
        searchEl?.select();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });

  const counts = $derived.by(() => {
    let attention = 0;
    let working = 0;
    let done = 0;
    for (const session of fleet.sessions) {
      if (session.status === 'attention') attention += 1;
      else if (session.status === 'working' || session.status === 'starting') working += 1;
      else if (session.status === 'done') done += 1;
    }
    return { attention, working, done };
  });

  function selected(row: FleetRow): boolean {
    if (row.kind === 'home') return view.peeked === null && fleet.jobShown === null;
    if (row.kind === 'creation') return fleet.jobShown === row.job.id;
    return row.kind === 'session' && row.session.id === view.peeked;
  }

  /** One count in a row's tag: its words, and the dot and number it shrinks to when compact. */
  interface Tally {
    word: string;
    status: PaneStatus | null;
    n: number | null;
  }

  /** What a heading says about what is under it. Words, never only the dot beside them. */
  function tally(c: {
    total: number;
    attention: number;
    working: number;
    done: number;
    failed: number;
  }): Tally[] {
    const parts: Tally[] = [];
    const count = (n: number, status: PaneStatus, word: string) =>
      parts.push({ word: `${n} ${word}`, status, n });
    if (c.attention > 0)
      count(c.attention, 'attention', c.attention === 1 ? 'needs you' : 'need you');
    if (c.failed > 0) count(c.failed, 'failed', 'failed');
    if (c.working > 0) count(c.working, 'working', 'running');
    if (c.done > 0) count(c.done, 'done', 'done');
    if (parts.length === 0 && c.total > 0) count(c.total, 'idle', 'idle');
    return parts;
  }

  // ─────────────────────────────── counts that fit ───────────────────────────────

  /**
   * Rows whose count is drawn compact — each count as its dot and number — by row key.
   *
   * Only where the full words would leave the name less room than a short one needs, so a long
   * name truncates first and the count is never cut. Decided by measuring, because whether it fits
   * depends on how many counts the row has, how deep it is, and how wide the column is.
   */
  let compact = $state<Record<string, boolean>>({});

  /** Room under which even a short name does not fit: about five characters at the tree's size. */
  const MIN_NAME_PX = 40;

  /**
   * Measure every row with a count. Works from either form: the words stay in the tag, clipped out
   * of sight when compact, so the room the name would have beside them can always be worked out —
   * which is what keeps a row from flipping between the two forms as each changes the space.
   */
  function fit(): void {
    if (!treeEl || !visible) return;
    const next: Record<string, boolean> = {};
    for (const row of treeEl.querySelectorAll<HTMLElement>('[data-nav]')) {
      const name = row.querySelector<HTMLElement>(':scope > .c-fleet__name');
      const tag = row.querySelector<HTMLElement>(':scope > .c-fleet__tag');
      const words = tag?.querySelector<HTMLElement>(':scope > .c-fleet__tag-full');
      const key = row.dataset.nav;
      if (!name || !tag || !words || !key) continue;
      const text = document.createRange();
      text.selectNodeContents(name);
      const wanted = text.getBoundingClientRect().width;
      const room = name.clientWidth + tag.offsetWidth - words.offsetWidth;
      if (wanted > room + 1 && room < MIN_NAME_PX) next[key] = true;
    }
    if (JSON.stringify(next) !== JSON.stringify(compact)) compact = next;
  }

  // On any change to the rows, after the DOM has it. The read of `compact` is after the `tick`, so
  // this effect does not depend on what it writes.
  $effect(() => {
    void rows;
    if (!visible) return;
    void tick().then(fit);
  });

  // And whenever the column changes width: the sidebar dragged, or a scrollbar arriving.
  onMount(() => {
    if (!treeEl) return;
    const observer = new ResizeObserver(() => fit());
    observer.observe(treeEl);
    return () => observer.disconnect();
  });

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
      case 'idle':
        fleet.toggle(row.key, !row.expanded);
        return;
      case 'worktree':
        if (row.expanded !== null) fleet.toggle(row.key, !row.expanded);
        else onopenworktree(row.projectId, row.worktree.id);
        return;
      case 'session':
        peek(row.session.id);
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
  async function startIn(
    event: MouseEvent | null,
    projectId: string,
    worktreeId: string,
    anchor: Element,
  ) {
    event?.stopPropagation();
    let options;
    try {
      options = await fleet.agentsFor(projectId);
    } catch (e) {
      sessions.error = `Could not list that repository's agents: ${String(e)}`;
      return;
    }
    const entries: MenuEntry[] = [
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
    void popUp(entries, under(anchor));
  }

  function rowMenu(event: MouseEvent | null, row: FleetRow, anchor: Element) {
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
          item('Peek', () => peek(pane.id)),
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

{#snippet tag(key: string, parts: Tally[])}
  {#if parts.length > 0}
    {@const words = parts.map((part) => part.word).join(' · ')}
    <span
      class="c-fleet__tag"
      class:is-compact={compact[key] === true}
      title={compact[key] ? words : undefined}
    >
      <span class="c-fleet__tag-full">{words}</span>
      {#if compact[key]}
        {#each parts as part (part.word)}
          <span class="c-fleet__tag-part" aria-hidden="true">
            {#if part.status}<SessionDot status={part.status} />{/if}{part.n ?? ''}
          </span>
        {/each}
      {/if}
    </span>
  {/if}
{/snippet}

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

<nav class="c-fleet" class:is-hidden={!visible} aria-label="Sessions everywhere">
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

  <!-- The counts double as filters, so the question "what needs me" is one click from the answer. -->
  <div class="c-fleet__counts" role="group" aria-label="Show only">
    <button
      class="c-fleet__count"
      class:is-pressed={fleet.only === 'attention'}
      aria-pressed={fleet.only === 'attention'}
      disabled={counts.attention === 0 && fleet.only !== 'attention'}
      onclick={() => (fleet.only = fleet.only === 'attention' ? null : 'attention')}
    >
      <SessionDot status="attention" />{counts.attention} need you
    </button>
    <button
      class="c-fleet__count"
      class:is-pressed={fleet.only === 'working'}
      aria-pressed={fleet.only === 'working'}
      disabled={counts.working === 0 && fleet.only !== 'working'}
      onclick={() => (fleet.only = fleet.only === 'working' ? null : 'working')}
    >
      <SessionDot status="working" />{counts.working} running
    </button>
    <button
      class="c-fleet__count"
      class:is-pressed={fleet.only === 'done'}
      aria-pressed={fleet.only === 'done'}
      disabled={counts.done === 0 && fleet.only !== 'done'}
      onclick={() => (fleet.only = fleet.only === 'done' ? null : 'done')}
    >
      <SessionDot status="done" />{counts.done} done
    </button>
  </div>

  <div class="c-fleet__wrap">
    <div class="c-fleet__scroll" bind:this={scrollEl}>
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
          <!-- Keys are the tree's, which hears them bubble from the focused row; a click is the
               mouse's route to the same activation. -->
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <div
            role="treeitem"
            class="c-fleet__row c-fleet__row--{row.kind}"
            class:is-selected={selected(row)}
            data-nav={row.key}
            tabindex={row.key === tabKey ? 0 : -1}
            aria-level={row.level}
            aria-expanded={expanded ?? undefined}
            aria-selected={selected(row)}
            style:--fleet-level={row.level}
            onclick={() => activate(row)}
            oncontextmenu={(event) => {
              event.preventDefault();
              rowMenu(event, row, event.currentTarget);
            }}
            onfocus={() => (focusKey = row.key)}
          >
            {@render twisty(row)}
            {#if row.kind === 'home'}
              <span class="c-fleet__icon"><Icon name="home" size={14} /></span>
              <span class="c-fleet__name">Home</span>
              {@render tag(row.key, [
                {
                  word: `${fleet.summary.sessions} ${fleet.summary.sessions === 1 ? 'session' : 'sessions'}`,
                  status: null,
                  n: fleet.summary.sessions,
                },
              ])}
            {:else if row.kind === 'project'}
              {#if row.status}<SessionDot status={row.status} />{/if}
              <span class="c-fleet__name" title={row.project.name}>{row.project.name}</span>
              {@render tag(
                row.key,
                row.project.usable
                  ? tally(row.counts)
                  : [{ word: 'needs attention', status: 'attention', n: null }],
              )}
              <span class="c-fleet__actions">
                <Button
                  variant="quiet"
                  icon="sm"
                  title="More for {row.project.name}"
                  ariaLabel="More for {row.project.name}"
                  ariaHaspopup="menu"
                  onclick={(event) => {
                    event.stopPropagation();
                    rowMenu(event, row, event.currentTarget as Element);
                  }}
                >
                  <Icon name="more" size={14} />
                </Button>
              </span>
            {:else if row.kind === 'worktree'}
              {#if row.status}<SessionDot status={row.status} />{/if}
              <span class="c-fleet__name" title={row.worktree.subtitle}
                >{row.worktree.title}</span
              >
              {#if row.expanded === null}
                <span class="c-fleet__meta">{row.worktree.subtitle}</span>
              {:else}
                {@render tag(row.key, tally(row.counts))}
              {/if}
              <span class="c-fleet__actions">
                <Button
                  variant="quiet"
                  icon="sm"
                  title="Start an agent in {row.worktree.title}"
                  ariaLabel="Start an agent in {row.worktree.title}"
                  ariaHaspopup="menu"
                  onclick={(event) =>
                    void startIn(
                      event,
                      row.projectId,
                      row.worktree.id,
                      event.currentTarget as Element,
                    )}
                >
                  <Icon name="plus" size={14} />
                </Button>
              </span>
            {:else if row.kind === 'creation'}
              <SessionDot
                status={row.job.phase === 'running'
                  ? 'starting'
                  : row.job.phase === 'created' || row.job.phase === 'removed'
                    ? 'done'
                    : 'failed'}
              />
              <span class="c-fleet__body">
                <span class="c-fleet__line">
                  <span class="c-fleet__name">⌂ {row.job.title}</span>
                  <span class="c-fleet__status">
                    {row.job.phase === 'running'
                      ? row.job.kind === 'remove'
                        ? 'removing…'
                        : 'creating…'
                      : row.job.phase === 'created'
                        ? 'new'
                        : row.job.phase === 'removed'
                          ? 'removed'
                          : 'failed'}
                  </span>
                </span>
                <span class="c-fleet__meta" title={row.job.detail}>{row.job.detail}</span>
              </span>
            {:else if row.kind === 'idle'}
              <span class="c-fleet__name c-fleet__name--quiet">Other worktrees</span>
              {@render tag(row.key, [
                { word: `(${row.count})`, status: null, n: row.count },
              ])}
            {:else}
              {@const inbound = fleet.inboundTo(row.session.id)}
              <SessionDot status={row.session.status} />
              <span class="c-fleet__body">
                <span class="c-fleet__line">
                  <span class="c-fleet__name" title={row.session.title}
                    >{row.session.title}</span
                  >
                  <span class="c-fleet__status">{STATUS_WORD[row.session.status]}</span>
                </span>
                <span class="c-fleet__meta">
                  {#if row.session.openedFromHome}<span
                      class="c-fleet__badge"
                      title="Opened from Home">⌂</span
                    >{/if}{row.session.agent}{#if row.session.model}
                    · {row.session.model}{/if}
                </span>
                {#if inbound}
                  <span class="c-fleet__chip" title={inbound.prompt}>
                    ← {fleet.nameOf(inbound.from)}: “{inbound.prompt}”
                  </span>
                {/if}
              </span>
            {/if}
          </div>
        {/each}
      </div>
    </div>
    <FleetWires {visible} {scrollEl} {treeEl} />
  </div>

  <div class="c-fleet__foot">
    <Button variant="neutral" full onclick={onaddproject}>
      <Icon name="plus" size={14} /> Add a repository…
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
