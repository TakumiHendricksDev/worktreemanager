<script lang="ts">
  /**
   * The left rail: a filter field, then worktrees as a tree of foldable groups.
   *
   * The list is a genuine `role="tree"` with arrow-key navigation, so "tabs down the left" still
   * works from the keyboard, and the groups are legible to a screen reader as groups. It is not
   * virtualized on purpose — a developer with 500 open worktrees does not exist, and a virtual list
   * would cost more than it could ever save here.
   *
   * What goes where is `sidebar.ts`: this component renders its `arrange` and hands every edit to
   * `workspace.editLayout`. What lives here is only what needs the DOM — focus, the drag, the menus.
   *
   * The project switcher used to sit above the list and now lives in the title bar; see
   * `TitleBar.svelte` for why.
   */
  import { onMount, tick } from 'svelte';

  import { ARM } from '../dropzone';
  import type { Worktree } from '../ipc/types';
  import {
    arrange,
    createGroup,
    deleteGroup,
    groupDropAt,
    kindOf,
    labelOf,
    locate,
    move,
    moveGroup,
    nudge,
    renameGroup,
    rowDropAt,
    setAllCollapsed,
    setCollapsed,
    shiftGroup,
    UNGROUPED,
    type GroupDrop,
    type GroupSlot,
    type RowDrop,
    type Section,
    type Slot,
  } from '../sidebar';
  import { item, popUp, separator, type MenuEntry } from '../native-menu';
  import { sessions } from '../state/sessions.svelte';
  import { workspace } from '../state/workspace.svelte';
  import { inRail, worse, type PaneStatus } from '../status';
  import SidebarSection from './SidebarSection.svelte';
  import WorktreeTab from './WorktreeTab.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    onnew,
    onselectworktree,
    detailId = 'worktree-detail',
  }: {
    onnew: () => void;
    /** Picking a worktree means "show me that one", so the pane leaves the create view. */
    onselectworktree?: () => void;
    /** The panel this tree controls, or empty while the create pane owns the screen. */
    detailId?: string | null;
  } = $props();

  /**
   * The dot a row shows, or null for nothing worth saying.
   *
   * This is why the sidebar knows about sessions at all, and it is the gap the whole status feature
   * exists to close: `SessionSurface` hides an unselected worktree's panes with `display: none`, and
   * both CLIs stop a turn until an approval is answered — so a blocked session in another worktree had
   * no representation anywhere in the chrome. It could sit there indefinitely.
   *
   * `sessions.statuses` is one derived map for the whole list, so this is a key lookup per row rather
   * than a scan per row. `inRail` is what keeps the quiet states out; see `status.ts`.
   */
  function railStatus(worktreeId: string): PaneStatus | null {
    const status = sessions.statuses[worktreeId];
    return status !== undefined && inRail(status) ? status : null;
  }

  /** The statuses a folded group may not hide. See `ArrangeOptions.urgent`. */
  function urgent(worktreeId: string): boolean {
    const status = sessions.statuses[worktreeId];
    return status === 'attention' || status === 'failed';
  }

  /** What a folded header's dot says: the most urgent status among the rows it hides. */
  function hiddenStatus(section: Section): PaneStatus | null {
    let found: PaneStatus | null = null;
    for (const worktree of section.hidden) {
      const status = railStatus(worktree.id);
      if (status) found = found === null ? status : worse(found, status);
    }
    return found;
  }

  /** What is being dragged, once a press has travelled far enough to be a drag. */
  let dragging = $state<{ kind: 'row' | 'group'; id: string } | null>(null);
  let rowDrop = $state<RowDrop | null>(null);
  let groupDrop = $state<GroupDrop | null>(null);
  /** The group whose name is being edited in place. */
  let renaming = $state<string | null>(null);
  /** The sentence the keyboard move just announced. A live region reads it. */
  let announcement = $state('');

  const arrangement = $derived(
    arrange(workspace.worktrees, workspace.layout, {
      matching: workspace.filtering ? workspace.matching : null,
      selectedId: workspace.selectedWorktreeId,
      urgent,
      dragging: dragging?.kind === 'row',
    }),
  );

  /**
   * Reordering is off while the filter is on. The rows it hides are still in the groups, so "just
   * above this row" on screen can be anywhere at all among them.
   */
  const arrangeable = $derived(!workspace.filtering);

  let wrapEl = $state<HTMLDivElement | null>(null);
  let treeEl = $state<HTMLDivElement | null>(null);
  let searchEl = $state<HTMLInputElement | null>(null);

  /**
   * ⌘F / Ctrl-F focuses the filter.
   *
   * Registered here rather than alongside the other shortcuts in `App.svelte` because the
   * thing it acts on is this component's input element. Reaching it from the parent would
   * mean exporting a ref upward for one keystroke.
   *
   * Unless something nearer the keystroke already took it. The database view's grid and
   * CodeMirror's search panel both handle ⌘F and cancel it, and without this check the filter
   * stole focus from the find field each of them had just opened.
   */
  onMount(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.defaultPrevented) return;
      if ((event.metaKey || event.ctrlKey) && event.key === 'f') {
        event.preventDefault();
        searchEl?.focus();
        searchEl?.select();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });

  // ─────────────────────────────── focus and selection ───────────────────────────────

  function pick(worktreeId: string) {
    workspace.select(worktreeId);
    onselectworktree?.();
  }

  /** Every item arrow keys stop on, in screen order — which is DOM order, by construction. */
  function navItems(): HTMLElement[] {
    return [...(treeEl?.querySelectorAll<HTMLElement>('[data-nav]') ?? [])];
  }

  /** Focus the item keyed `key` once the DOM has caught up, and scroll it into view. */
  function focusItem(key: string) {
    void tick().then(() => {
      const target = navItems().find((el) => el.dataset.nav === key);
      target?.focus();
      target?.scrollIntoView({ block: 'nearest' });
    });
  }

  function focusSelected() {
    const id = workspace.selectedWorktreeId;
    if (id) focusItem(`row:${id}`);
  }

  /** Arrive on an item: a row is selected as it is reached, the way a tab strip behaves. */
  function land(el: HTMLElement | undefined) {
    if (!el) return;
    const key = el.dataset.nav ?? '';
    if (key.startsWith('row:')) pick(key.slice(4));
    focusItem(key);
  }

  /** Which item an event came from: the row or group it is inside, star buttons included. */
  function itemOf(
    target: EventTarget | null,
  ): { kind: 'row' | 'group'; id: string } | null {
    const el = target instanceof HTMLElement ? target : null;
    const row = el?.closest<HTMLElement>('[data-row]')?.dataset.row;
    if (row) return { kind: 'row', id: row };
    const group = el?.closest<HTMLElement>('[data-nav^="group:"]')?.dataset.nav?.slice(6);
    return group ? { kind: 'group', id: group } : null;
  }

  function sectionOf(groupId: string): Section | undefined {
    return arrangement.sections.find((s) => s.id === groupId);
  }

  function onTreeKeydown(event: KeyboardEvent) {
    const here = itemOf(event.target);
    const items = navItems();
    const key = here ? `${here.kind}:${here.id}` : null;
    const at = items.findIndex((el) => el.dataset.nav === key);

    if (event.altKey && (event.key === 'ArrowUp' || event.key === 'ArrowDown')) {
      event.preventDefault();
      const delta = event.key === 'ArrowUp' ? -1 : 1;
      if (here?.kind === 'row') nudgeRow(here.id, delta);
      else if (here?.kind === 'group') shift(here.id, delta);
      return;
    }

    if ((event.shiftKey && event.key === 'F10') || event.key === 'ContextMenu') {
      if (!here) return;
      event.preventDefault();
      const el = items[at];
      const box = el?.getBoundingClientRect();
      const point = box ? { x: box.left + 12, y: box.bottom } : undefined;
      if (here.kind === 'row') void rowMenu(here.id, point);
      else void groupMenu(here.id, point);
      return;
    }

    switch (event.key) {
      case 'ArrowDown':
      case 'ArrowUp': {
        event.preventDefault();
        const delta = event.key === 'ArrowDown' ? 1 : -1;
        // Nothing focused yet: ArrowDown starts *on* the first item, ArrowUp on the last.
        const from = at === -1 ? (delta > 0 ? -1 : items.length) : at;
        land(items[Math.min(Math.max(from + delta, 0), items.length - 1)]);
        return;
      }
      case 'Home':
        event.preventDefault();
        land(items[0]);
        return;
      case 'End':
        event.preventDefault();
        land(items.at(-1));
        return;
    }

    if (here?.kind === 'group') {
      const section = sectionOf(here.id);
      if (!section) return;
      if (event.key === 'Enter' || event.key === ' ') {
        event.preventDefault();
        toggle(section);
      } else if (event.key === 'ArrowRight') {
        event.preventDefault();
        if (!section.open) toggle(section);
        else
          land(items[at + 1]?.dataset.nav?.startsWith('row:') ? items[at + 1] : undefined);
      } else if (event.key === 'ArrowLeft') {
        event.preventDefault();
        if (section.open) toggle(section);
      } else if (event.key === 'F2' && section.kind === 'custom') {
        event.preventDefault();
        renaming = section.id;
      }
      return;
    }

    if (here?.kind === 'row' && event.key === 'ArrowLeft' && arrangement.headers) {
      // Up to the group this row is in, the tree's way of saying "back out".
      event.preventDefault();
      const group = arrangement.sections.find((s) => s.rows.some((w) => w.id === here.id));
      if (group) focusItem(`group:${group.id}`);
    }
  }

  /** Escape clears, Enter and ArrowDown hand off to the list. */
  function onSearchKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      // Only swallow the key while there is something to clear, so Escape still reaches
      // anything else that wants it once the field is empty.
      if (workspace.query !== '') {
        event.preventDefault();
        workspace.query = '';
      }
      return;
    }

    if (event.key === 'Enter' || event.key === 'ArrowDown') {
      const first = arrangement.rows[0];
      if (!first) return;
      event.preventDefault();
      // Enter means "the one I was typing towards"; ArrowDown means "let me walk the list".
      // Both start by selecting the top match, and both move focus out of the field.
      pick(first.id);
      focusSelected();
    }
  }

  // ──────────────────────────────────── edits ────────────────────────────────────

  function toggle(section: Section) {
    // A filter shows folded groups open, and a fold changed behind it would be invisible until the
    // filter is cleared — so while filtering, a header does nothing.
    if (workspace.filtering) return;
    workspace.editLayout((layout) => setCollapsed(layout, section.id, !section.collapsed));
  }

  function nudgeRow(worktreeId: string, delta: -1 | 1) {
    if (!arrangeable) return;
    const layout = $state.snapshot(workspace.layout);
    const all = workspace.worktrees.map((w) => w.id);
    const next = nudge(layout, all, arrangement, worktreeId, delta);
    if (next === null) return;
    workspace.editLayout(() => next);
    void tick().then(() => {
      announceRow(worktreeId);
      focusItem(`row:${worktreeId}`);
    });
  }

  /** "Moved to Reviews, 2 of 4" — where a keyboard move landed, for the live region. */
  function announceRow(worktreeId: string) {
    const section = arrangement.sections.find((s) =>
      s.members.some((w) => w.id === worktreeId),
    );
    if (!section) return;
    const position = section.members.findIndex((w) => w.id === worktreeId) + 1;
    announcement = `Moved to ${section.label}, ${position} of ${section.members.length}`;
  }

  function shift(groupId: string, delta: -1 | 1) {
    if (kindOf(groupId) !== 'custom') return;
    workspace.editLayout((layout) => shiftGroup(layout, groupId, delta));
    void tick().then(() => {
      const custom = arrangement.sections.filter((s) => s.kind === 'custom');
      const position = custom.findIndex((s) => s.id === groupId) + 1;
      const section = sectionOf(groupId);
      if (section) announcement = `${section.label} moved, ${position} of ${custom.length}`;
      focusItem(`group:${groupId}`);
    });
  }

  function moveTo(worktreeId: string, group: string) {
    workspace.editLayout((layout, all) =>
      move(layout, all, worktreeId, { group, before: null }),
    );
  }

  /**
   * A new group, named "New group" and already in rename mode, so making one and naming it is one
   * gesture. The id is made here because the group has to render before any round trip returns.
   */
  function newGroup(worktreeId: string | null) {
    const id = `g-${crypto.randomUUID()}`;
    workspace.editLayout((layout, all) =>
      createGroup(layout, all, { id, name: 'New group' }, worktreeId),
    );
    renaming = id;
  }

  function commitRename(groupId: string, name: string) {
    renaming = null;
    if (name.trim() !== '')
      workspace.editLayout((layout) => renameGroup(layout, groupId, name));
    focusItem(`group:${groupId}`);
  }

  function cancelRename(groupId: string) {
    renaming = null;
    focusItem(`group:${groupId}`);
  }

  // ──────────────────────────────────── menus ────────────────────────────────────

  function customGroups() {
    return workspace.layout.groups.filter((g) => kindOf(g.id) === 'custom');
  }

  async function rowMenu(worktreeId: string, at?: { x: number; y: number }) {
    const here = locate(workspace.layout, worktreeId);
    const starred = workspace.isFavorite(worktreeId);
    const ungrouped = workspace.layout.groups.find((g) => g.id === UNGROUPED);

    const destinations: MenuEntry[] = [
      ...customGroups()
        .filter((g) => g.id !== here)
        .map((g) => item(labelOf(g), () => moveTo(worktreeId, g.id))),
      ...(here !== UNGROUPED && ungrouped
        ? [item(labelOf(ungrouped), () => moveTo(worktreeId, UNGROUPED))]
        : []),
      separator,
      item('New Group…', () => newGroup(worktreeId)),
    ];

    await popUp(
      [
        item(starred ? 'Remove from Favorites' : 'Add to Favorites', () =>
          workspace.toggleFavorite(worktreeId),
        ),
        { kind: 'submenu', text: 'Move To', items: destinations },
        separator,
        item('Move Up', () => nudgeRow(worktreeId, -1), arrangeable),
        item('Move Down', () => nudgeRow(worktreeId, 1), arrangeable),
      ],
      at,
    );
  }

  async function groupMenu(groupId: string, at?: { x: number; y: number }) {
    const section = sectionOf(groupId);
    if (!section) return;
    const custom = section.kind === 'custom';
    const order = customGroups().map((g) => g.id);
    const position = order.indexOf(groupId);

    await popUp(
      [
        ...(custom
          ? [
              item('Rename…', () => (renaming = groupId)),
              item('Move Up', () => shift(groupId, -1), position > 0),
              item('Move Down', () => shift(groupId, 1), position < order.length - 1),
              separator,
            ]
          : []),
        item(
          section.open ? 'Collapse' : 'Expand',
          () => toggle(section),
          !workspace.filtering,
        ),
        item(
          'Collapse All',
          () => workspace.editLayout((layout) => setAllCollapsed(layout, true)),
          !workspace.filtering,
        ),
        item(
          'Expand All',
          () => workspace.editLayout((layout) => setAllCollapsed(layout, false)),
          !workspace.filtering,
        ),
        separator,
        item('New Group…', () => newGroup(null)),
        ...(custom
          ? [
              separator,
              item('Delete Group', () =>
                workspace.editLayout((layout, all) => deleteGroup(layout, all, groupId)),
              ),
            ]
          : []),
      ],
      at,
    );
  }

  function onRowMenu(event: MouseEvent, worktreeId: string) {
    event.preventDefault();
    void rowMenu(worktreeId);
  }

  function onGroupMenu(event: MouseEvent, groupId: string) {
    event.preventDefault();
    void groupMenu(groupId);
  }

  // ──────────────────────────────────── drag ────────────────────────────────────

  /**
   * How close to the list's top or bottom edge the pointer has to be for it to scroll, and how many
   * pixels a frame it scrolls by. A drag has to be able to reach a group that is off screen.
   */
  const EDGE = 28;
  const SCROLL_STEP = 8;

  /** The pointer's height in the tree's own content box — the space `Slot`s are measured in. */
  function contentY(clientY: number): number {
    return clientY - (treeEl?.getBoundingClientRect().top ?? 0);
  }

  function measureRows(): Slot[] {
    const top = treeEl?.getBoundingClientRect().top ?? 0;
    return [...(treeEl?.querySelectorAll<HTMLElement>('[data-slot]') ?? [])].map((el) => {
      const box = el.getBoundingClientRect();
      const group = el.dataset.group ?? UNGROUPED;
      return el.dataset.slot === 'header'
        ? {
            kind: 'header',
            group,
            top: box.top - top,
            bottom: box.bottom - top,
            first: el.dataset.first || null,
          }
        : {
            kind: 'row',
            group,
            worktreeId: el.dataset.row ?? '',
            top: box.top - top,
            bottom: box.bottom - top,
            next: el.dataset.next || null,
          };
    });
  }

  function measureGroups(): GroupSlot[] {
    const top = treeEl?.getBoundingClientRect().top ?? 0;
    return [...(treeEl?.querySelectorAll<HTMLElement>('[data-group-slot]') ?? [])].map(
      (el) => {
        const box = el.getBoundingClientRect();
        return {
          group: el.dataset.groupSlot ?? '',
          top: box.top - top,
          bottom: box.bottom - top,
        };
      },
    );
  }

  /**
   * Pointer events rather than HTML5 drag-and-drop, and not by preference: `tauri.conf.json` sets
   * `dragDropEnabled` so a Finder drop can yield a real path for an `@`-mention, and that disables
   * `dragstart`/`dragover`/`drop` across the entire webview. See `SessionPane`'s note on it.
   *
   * The slots are measured once, when the press arms into a drag — after the empty Favorites group a
   * drag reveals has rendered — and in the tree's content box, so they stay right while the list
   * scrolls. A press that never travels `ARM` pixels is left entirely alone, which is what keeps a
   * click on a row a click.
   */
  function startDrag(event: PointerEvent, kind: 'row' | 'group', id: string) {
    // Control-click is macOS's right-click: it opens the menu, and is not the start of a drag.
    if (event.button !== 0 || event.ctrlKey || !arrangeable || renaming !== null) return;
    if (kind === 'group' && kindOf(id) !== 'custom') return;

    const origin = { x: event.clientX, y: event.clientY };
    let armed = false;
    let slots: Slot[] = [];
    let groupSlots: GroupSlot[] = [];
    let lastY = event.clientY;
    let frame = 0;

    const aim = () => {
      const y = contentY(lastY);
      if (kind === 'row') rowDrop = rowDropAt(slots, y, id);
      else groupDrop = groupDropAt(groupSlots, y, id);
    };

    // Scroll while the pointer rests near an edge, re-aiming each frame: the list moves under a
    // pointer that does not, and the drop point has to move with it.
    const scroll = () => {
      frame = 0;
      const box = wrapEl?.getBoundingClientRect();
      if (!box || !wrapEl) return;
      const step =
        lastY < box.top + EDGE ? -SCROLL_STEP : lastY > box.bottom - EDGE ? SCROLL_STEP : 0;
      if (step === 0) return;
      wrapEl.scrollTop += step;
      aim();
      frame = requestAnimationFrame(scroll);
    };

    const onMove = (m: PointerEvent) => {
      // The button is up but no `pointerup` arrived — released over a native menu or outside the
      // window. Without this the next plain mouse move would arm a drag nobody is holding.
      if ((m.buttons & 1) === 0) {
        finish(false);
        return;
      }
      lastY = m.clientY;
      if (!armed) {
        if (Math.hypot(m.clientX - origin.x, m.clientY - origin.y) < ARM) return;
        armed = true;
        dragging = { kind, id };
        void tick().then(() => {
          if (dragging === null) return;
          if (kind === 'row') slots = measureRows();
          else groupSlots = measureGroups();
          aim();
        });
        return;
      }
      aim();
      if (frame === 0) frame = requestAnimationFrame(scroll);
    };

    const finish = (commit: boolean) => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onCancel);
      window.removeEventListener('keydown', onKey, true);
      if (frame !== 0) cancelAnimationFrame(frame);

      const row = commit ? rowDrop : null;
      const group = commit ? groupDrop : null;
      dragging = null;
      rowDrop = null;
      groupDrop = null;
      if (!armed) return;

      // The release that ends a drag also clicks whatever it lands on, and a drag that ends on a row
      // must not select it, nor one ending on a header fold it. Swallow that one click; the timeout
      // retires the listener if the release happened somewhere that produces none.
      const swallow = (c: MouseEvent) => {
        c.stopPropagation();
        c.preventDefault();
      };
      window.addEventListener('click', swallow, { capture: true, once: true });
      setTimeout(() => window.removeEventListener('click', swallow, true), 0);

      if (row) {
        workspace.editLayout((layout, all) => move(layout, all, id, row.place));
      } else if (group) {
        workspace.editLayout((layout) => moveGroup(layout, id, group.before));
      }
    };

    const onUp = () => finish(true);
    // Not optional. A system gesture or a window drag ends the sequence here, and without it the
    // indicator stays painted and the listeners stay live.
    const onCancel = () => finish(false);
    const onKey = (k: KeyboardEvent) => {
      if (k.key !== 'Escape' || !armed) return;
      k.preventDefault();
      k.stopPropagation();
      finish(false);
    };

    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onCancel);
    window.addEventListener('keydown', onKey, true);
  }

  /** The drop line's height in the tree, whichever kind of drag is under way. */
  const line = $derived(rowDrop?.line ?? groupDrop?.line ?? null);

  /** The member after `worktree` in `section`, for a row's lower-half drop. */
  function nextOf(section: Section, worktree: Worktree): string | null {
    const at = section.members.indexOf(worktree);
    return section.members[at + 1]?.id ?? null;
  }
</script>

{#snippet row(worktree: Worktree, section: Section, level: 1 | 2)}
  <WorktreeTab
    {worktree}
    {level}
    group={section.id}
    next={nextOf(section, worktree)}
    status={railStatus(worktree.id)}
    selected={worktree.id === workspace.selectedWorktreeId}
    favorite={workspace.isFavorite(worktree.id)}
    dragging={dragging?.kind === 'row' && dragging.id === worktree.id}
    controls={detailId}
    onselect={() => pick(worktree.id)}
    onfavorite={() => workspace.toggleFavorite(worktree.id)}
    onpress={(event) => startDrag(event, 'row', worktree.id)}
    onmenu={(event) => onRowMenu(event, worktree.id)}
  />
{/snippet}

<nav class="c-sidebar" aria-label="Worktrees">
  <div class="c-sidebar__controls">
    <div class="c-search" role="search">
      <span class="c-search__icon"><Icon name="search" size={14} /></span>
      <label class="u-visually-hidden" for="worktree-search">Filter worktrees</label>
      <input
        id="worktree-search"
        class="c-search__input"
        type="search"
        bind:this={searchEl}
        bind:value={workspace.query}
        onkeydown={onSearchKeydown}
        placeholder="Filter worktrees"
        autocomplete="off"
        spellcheck="false"
        disabled={!workspace.activeProject?.usable}
      />
      {#if workspace.query !== ''}
        <button
          class="c-search__clear"
          onclick={() => (workspace.query = '')}
          title="Clear the filter"
        >
          <Icon name="close" size={12} />
          <span class="u-visually-hidden">Clear the filter</span>
        </button>
      {/if}
    </div>
  </div>

  <div class="c-sidebar__list-wrap" bind:this={wrapEl}>
    <!--
      `loadingWorktrees` is true only when there is nothing on screen. A refresh over an
      existing list sets `revalidating` instead, which deliberately changes no layout — the
      list stays put and gets patched in place.
    -->
    {#if workspace.loadingWorktrees && workspace.worktrees.length === 0}
      <p class="c-sidebar__empty">Loading…</p>
    {:else if workspace.projects.length === 0}
      <p class="c-sidebar__empty">Add a git repository to get started.</p>
    {:else if workspace.activeProject && !workspace.activeProject.usable}
      <p class="c-sidebar__empty">
        This project needs attention — see the panel on the right.
      </p>
    {:else if workspace.worktrees.length === 0}
      <p class="c-sidebar__empty">No worktrees.</p>
    {:else if workspace.matching.length === 0}
      <p class="c-sidebar__empty">
        Nothing matches <strong>{workspace.query}</strong>.
        <Button variant="link" onclick={() => (workspace.query = '')}
          >Clear the filter</Button
        >
      </p>
    {:else}
      <!--
        The tree itself is deliberately not focusable. Per the ARIA tree pattern, focus lives
        on the items (roving tabindex, set on the rows) and the tree only listens for the keys
        that bubble up from them. Giving the container a tabindex would add a second, pointless
        tab stop before the list.

        Rows and headers render from one `arrange`, which is also what arrow keys walk — by way
        of the DOM order it produced — so screen order and keyboard order cannot drift.
      -->
      <!-- svelte-ignore a11y_interactive_supports_focus -->
      <div
        role="tree"
        aria-label="Worktrees"
        aria-orientation="vertical"
        class="c-sidebar__list"
        class:is-dragging={dragging !== null}
        bind:this={treeEl}
        onkeydown={onTreeKeydown}
      >
        {#each arrangement.sections as section (section.id)}
          {#if arrangement.headers}
            <SidebarSection
              {section}
              status={hiddenStatus(section)}
              renaming={renaming === section.id}
              droptarget={rowDrop?.header === section.id}
              dragging={dragging?.kind === 'group' && dragging.id === section.id}
              ontoggle={() => toggle(section)}
              onrename={(name) => commitRename(section.id, name)}
              oncancelrename={() => cancelRename(section.id)}
              onpress={(event) => startDrag(event, 'group', section.id)}
              onmenu={(event) => onGroupMenu(event, section.id)}
            >
              {#each section.rows as worktree (worktree.id)}
                {@render row(worktree, section, 2)}
              {/each}
            </SidebarSection>
          {:else}
            {#each section.rows as worktree (worktree.id)}
              {@render row(worktree, section, 1)}
            {/each}
          {/if}
        {/each}

        {#if line !== null}
          <div class="c-sidebar__drop" style:top={`${line}px`} aria-hidden="true"></div>
        {/if}
      </div>
    {/if}
  </div>

  <div class="c-sidebar__foot">
    <!--
      A live region rather than a spinner. The point of the cache is that a refresh is not an
      event worth reacting to; this exists so "the list may be a few seconds old" is still
      *knowable*, without anything moving.
    -->
    <p class="c-sidebar__status" aria-live="polite">
      {#if workspace.filtering}
        {workspace.matching.length} of {workspace.worktrees.length}
      {:else if workspace.revalidating}Refreshing…{:else if workspace.stale}Showing the last
        known list.{/if}
    </p>
    <!-- Where a keyboard move landed. Hidden, because the row itself moving is the visual answer. -->
    <p class="u-visually-hidden" aria-live="polite">{announcement}</p>
    <Button
      variant="accent"
      full
      onclick={onnew}
      disabled={!workspace.activeProject?.usable}
    >
      <Icon name="plus" size={14} /> New Worktree
    </Button>
  </div>
</nav>
