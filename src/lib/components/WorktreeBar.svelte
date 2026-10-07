<script lang="ts">
  /**
   * One row above the sessions: what this worktree is, and what you can do to it.
   *
   * # Six controls, grouped by what they do
   *
   * It had grown to ten, one per action, and they no longer fitted: at the smallest window the bar
   * gets about 580 pixels with the sidebar open, and the row needed about 900, so the last buttons
   * slid off the edge of the window. Most of them were one of three kinds of thing, so they are
   * grouped by kind now rather than squeezed:
   *
   *   * **New** is everything that opens a pane here — the agents, a shell, a browser. They used to
   *     be a New agent menu and two buttons beside it.
   *   * **Open in** keeps its split button. It is the one action here most people use every time.
   *   * **Links** is a split button too, when the project configures any: the one last opened, and
   *     the rest. They were a submenu of ⋯ until it was clear that most people open the same link
   *     every time, and three steps to reach it was the cost — see `LinksButton.svelte`.
   *   * **⋯** is what you reach for occasionally: Details and Remove.
   *
   * The menus are native — see `native-menu.ts` — so they cost no `z-index`, which is what keeps
   * `settings/_config.scss`'s rule intact.
   *
   * Remove used to sit alone past a hairline, because a destructive control flush against a neutral
   * one is how it gets clicked by accident. Inside a menu, at the bottom, behind a separator and still
   * followed by its confirmation dialog, it is further from an accident than the hairline made it.
   *
   * The facts moved into the Details dialog, and the dirty state went too, because the selected tab
   * beside it already says it. The branch stays: while the sidebar is hidden there is no tab beside
   * it, and this bar is the only place left that says which branch you are on.
   *
   * # The name is a switcher while the sidebar is hidden
   *
   * With the rail gone this bar is the only place that names a worktree, so the name becomes the
   * list: a native menu under it, as the project picker in the title bar has, and for the same
   * reason it is not a `<select>` — see `native-menu.ts`. It follows the sidebar's arrangement — a
   * heading per group, in the order the user dragged them into — so the two never disagree about
   * where a worktree is.
   *
   * What it deliberately does not follow is the sidebar's folds and filter. Those decide what a
   * list leaves out to save room, and a menu is the thing you open to see everything; a worktree
   * missing from it because a group was folded in a rail nobody can see would just be missing.
   */
  import { browsers } from '../state/browsers.svelte';
  import { sessions } from '../state/sessions.svelte';
  import { INSPECTOR_SHORTCUT, SHELL_SHORTCUT } from '../state/sessions.svelte';
  import { summaryText, tightest, usage } from '../state/usage.svelte';
  import { workspace } from '../state/workspace.svelte';
  import {
    choice,
    heading,
    item,
    popUp,
    separator,
    under,
    type MenuEntry,
  } from '../native-menu';
  import { arrange } from '../sidebar';
  import type { Worktree } from '../ipc/types';
  import type { WorktreeView } from '../worktree-view';
  import LinksButton from './LinksButton.svelte';
  import OpenInButton from './OpenInButton.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    worktree,
    projectId,
    view,
    sidebarCollapsed,
    onsessions,
    ondatabase,
    oncode,
    onremove,
    onfavorite,
    oninspect,
    onselect,
    onnew,
  }: {
    worktree: Worktree;
    projectId: string;
    /** Which of the worktree's views is showing, for the switch's pressed state. */
    view: WorktreeView;
    /** Whether the name is a switcher. See the header. */
    sidebarCollapsed: boolean;
    onsessions: () => void;
    ondatabase: () => void;
    oncode: () => void;
    onremove: () => void;
    onfavorite: () => void;
    oninspect: () => void;
    onselect: (worktreeId: string) => void;
    /** The sidebar's New worktree button, which is out of reach while it is hidden. */
    onnew: () => void;
  } = $props();

  /** In the Favorites group. A lookup, since a star is where a worktree sits, not a field on it. */
  const favorite = $derived(workspace.isFavorite(worktree.id));

  const startable = $derived(
    sessions.options.filter((option) => option.available && option.offered),
  );

  // The Compose project name is an implementation detail and, in the ordinary case, just a
  // slugged copy of the worktree title beside it. Keep genuinely useful configured badges (issue
  // status, environment, owner) without spending top-bar space repeating the selected worktree.
  const visibleBadges = $derived(
    worktree.badges.filter((badge) => badge.label.trim().toLowerCase() !== 'compose'),
  );

  /**
   * The same `●` the project picker appends, for the same reason: the row dots are gone with the
   * rail, and without it a blocked session in another worktree would be invisible again.
   */
  function rowLabel(candidate: Worktree): string {
    const status = sessions.statuses[candidate.id];
    return candidate.title + (status === 'attention' || status === 'failed' ? '  ●' : '');
  }

  /**
   * Every group that has anyone in it, each listing all its members.
   *
   * `members` rather than `rows`, and no filter and no `urgent`, for the reason in the header. An
   * empty group is dropped here although the sidebar keeps it, because a heading with nothing under
   * it is one you can neither pick nor drop onto.
   */
  function switchMenu(event: MouseEvent) {
    const { sections, headers } = arrange(workspace.worktrees, workspace.layout, {
      matching: null,
      selectedId: worktree.id,
      urgent: () => false,
      dragging: false,
    });
    const row = (candidate: Worktree) =>
      choice(rowLabel(candidate), candidate.id === worktree.id, () => {
        if (candidate.id !== worktree.id) onselect(candidate.id);
      });
    const entries: MenuEntry[] = sections
      .filter((section) => section.members.length > 0)
      .flatMap((section) => [
        ...(headers ? [separator, heading(section.label)] : []),
        ...section.members.map(row),
      ]);
    entries.push(separator, item('New worktree…', onnew));
    void popUp(entries, under(event.currentTarget as Element));
  }

  function newMenu(event: MouseEvent) {
    const now = Date.now();
    const entries: MenuEntry[] = [
      // Available even when the only pane is a shell; starting an agent never has to replace it.
      //
      // Each row carries the agent's tightest usage limit when one is known, because this is the
      // moment of choosing between them, and the agent near its limit is the one not to start a
      // long task on. Only what Rust already holds: opening a menu does not ask anyone, and a
      // menu row is text, so there is no bar.
      ...startable.map((option) => {
        const window = tightest(usage.accounts[option.id], now);
        return item(
          window ? `${option.label}  ·  ${summaryText(window)}` : option.label,
          () => void sessions.openAgent(projectId, worktree.id, option.id),
        );
      }),
      separator,
      // The call ⌘J makes, as the Shell button this replaced did, so the two never disagree: a
      // shortcut that focused the open shell beside a menu row that spawned another login shell per
      // click would leak one for every habituated pick.
      item('Shell', () => void sessions.focusOrOpenShell(projectId, worktree.id)),
      item(
        'Browser',
        () => void sessions.openBrowser(projectId, worktree.id),
        browsers.unavailable === null,
      ),
    ];
    void popUp(entries, under(event.currentTarget as Element));
  }

  function moreMenu(event: MouseEvent) {
    const entries: MenuEntry[] = [
      item('Details…', oninspect),
      separator,
      // Disabled rather than left out on the main worktree: git refuses to remove it, and so does the
      // pipeline, and a menu that sometimes has no Remove teaches nobody where Remove lives.
      item('Remove Worktree…', onremove, !worktree.isMain),
    ];
    void popUp(entries, under(event.currentTarget as Element));
  }
</script>

<header class="c-worktree-bar">
  <!--
    Duplicated from the sidebar on purpose: the one there only appears on hover, which is not
    somewhere a control can be *found*. This is where you learn it exists, and while the sidebar
    is hidden it is the only star there is.
  -->
  <button
    class="c-detail__star"
    class:is-on={favorite}
    aria-pressed={favorite}
    title={favorite ? 'Remove from favorites' : 'Add to favorites'}
    onclick={onfavorite}
  >
    <Icon name={favorite ? 'star' : 'star-outline'} size={18} />
    <span class="u-visually-hidden">Favorite</span>
  </button>

  {#if sidebarCollapsed}
    <h1 class="c-worktree-bar__picker">
      <button
        class="c-worktree-bar__switch"
        aria-label="Switch worktree: {worktree.title}"
        aria-haspopup="menu"
        onclick={switchMenu}
      >
        <span class="c-worktree-bar__name">{worktree.title}</span>
        <Icon name="chevron-down" size={12} />
      </button>
    </h1>
  {:else}
    <h1 class="c-worktree-bar__title">{worktree.title}</h1>
  {/if}

  <div class="c-worktree-bar__facts">
    <!-- Shown with the sidebar open as well, although the selected tab there repeats it, so the
         bar does not change shape each time the rail is toggled. -->
    {#if worktree.branch}
      <code class="c-worktree-bar__branch" title={worktree.branch}>{worktree.branch}</code>
    {:else}
      <span class="c-status--muted">detached</span>
    {/if}
    <!-- No "main worktree" badge and no "modified". The selected tab directly to the left already
         shows both — it has a `main` pill and carries the dirty state — so here they repeated the
         one thing on screen the user had just clicked. -->
    <!-- The `↑N↓N` divergence counter was here and is gone for the same reason it left
         `WorktreeTab`: it is a measurement presented as a status, and this header is the one place
         where the Inspector — which reports ahead, behind, staged and unstaged properly — is a
         single click away. -->

    {#each visibleBadges as badge, i (`${badge.label}:${i}`)}
      <span class="c-badge" title={badge.label}>{badge.label}: {badge.value}</span>
    {/each}
  </div>

  <div class="c-worktree-bar__actions">
    <div class="c-worktree-bar__view-switch" aria-label="Worktree view">
      <Button
        variant={view === 'sessions' ? 'neutral' : 'quiet'}
        size="sm"
        ariaPressed={view === 'sessions'}
        onclick={onsessions}>Sessions</Button
      >
      <Button
        variant={view === 'database' ? 'neutral' : 'quiet'}
        size="sm"
        ariaPressed={view === 'database'}
        onclick={ondatabase}>Database</Button
      >
      <Button
        variant={view === 'code' ? 'neutral' : 'quiet'}
        size="sm"
        ariaPressed={view === 'code'}
        onclick={oncode}>Code</Button
      >
    </div>

    <Button
      variant="quiet"
      size="sm"
      title="Open an agent, a shell ({SHELL_SHORTCUT}) or a browser in this worktree"
      ariaHaspopup="menu"
      onclick={newMenu}
    >
      New <Icon name="chevron-down" size={11} />
    </Button>

    <OpenInButton {projectId} worktreeId={worktree.id} />

    <LinksButton {projectId} worktreeId={worktree.id} links={worktree.links} />

    <Button
      variant="quiet"
      size="sm"
      icon="sm"
      title="Details ({INSPECTOR_SHORTCUT}) and Remove"
      ariaLabel="More actions for this worktree"
      ariaHaspopup="menu"
      onclick={moreMenu}
    >
      <Icon name="more" size={13} />
    </Button>
  </div>
</header>
