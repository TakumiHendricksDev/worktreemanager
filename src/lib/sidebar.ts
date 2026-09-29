/**
 * How the sidebar turns git's list of worktrees and the stored layout into what is on screen, and
 * how every edit to that layout is made.
 *
 * # Why this is a plain module
 *
 * The reason `dropzone.ts` gives: there is no JS test runner, so logic that is only reachable by
 * dragging a row across a live window has to be pure functions in a file a person can read, or it
 * cannot be checked at all. Nothing here touches the DOM, the stores or the backend. Every edit takes
 * a layout and returns a new one; `workspace.editLayout` is what applies it and writes it back.
 *
 * # Favorites is a group
 *
 * Starring moves a worktree into the built-in `favorites` group, and every worktree is in exactly one
 * group — see `wtm_config::sidebar` for why the star stopped being a flag. The other built-in,
 * `ungrouped`, holds whatever the user has filed nowhere, and is also where a worktree git lists but
 * the layout has never heard of appears: at its end, in git's order. A project nobody has arranged
 * therefore renders exactly as it did before any of this existed — one list, git's order, no
 * headings.
 *
 * # The one invariant the edits have to keep themselves
 *
 * Rust normalizes whatever it is sent (one place per worktree, the built-ins first and last), so an
 * edit here cannot corrupt the stored layout. What Rust cannot do is keep a *displayed* position
 * meaningful, because it never sees the listing. So an edit that inserts relative to what is on
 * screen first writes the target group's displayed order into it — `materialize` — and only then
 * inserts. Without that, dropping a row among the never-arranged worktrees at the end of the
 * ungrouped section would land it above all of them.
 */

import type { SidebarGroup, SidebarLayout, Worktree } from './ipc/types';

/** Must match `wtm_config::sidebar::FAVORITES`. */
export const FAVORITES = 'favorites';
/** Must match `wtm_config::sidebar::UNGROUPED`. */
export const UNGROUPED = 'ungrouped';

export type SectionKind = 'favorites' | 'custom' | 'ungrouped';

/** A layout with nothing arranged — what a project reads as before anyone touches it. */
export function emptyLayout(): SidebarLayout {
  return { groups: [builtin(FAVORITES), builtin(UNGROUPED)], origins: {} };
}

function builtin(id: string): SidebarGroup {
  return { id, name: '', collapsed: false, worktrees: [] };
}

export function kindOf(groupId: string): SectionKind {
  if (groupId === FAVORITES) return 'favorites';
  if (groupId === UNGROUPED) return 'ungrouped';
  return 'custom';
}

/**
 * What a group's header says.
 *
 * The built-ins' labels live here rather than in the config, so they are copy the app can change
 * without a migration. The ungrouped section is "Other worktrees", not the "All worktrees" it was
 * called beside Favorites: once there are groups, the rows under it are no longer all of them.
 */
export function labelOf(group: SidebarGroup): string {
  if (group.id === FAVORITES) return 'Favorites';
  if (group.id === UNGROUPED) return 'Other worktrees';
  return group.name.trim() || 'Untitled group';
}

/**
 * The groups in display order, with both built-ins present.
 *
 * Rust already guarantees this for anything it returned. Repeated here because the layout on screen
 * may instead be the cached copy from an older build, or the empty default, and rendering must not
 * depend on which.
 */
function ordered(layout: SidebarLayout): SidebarGroup[] {
  const seen = new Set<string>();
  const custom: SidebarGroup[] = [];
  for (const group of layout.groups) {
    if (kindOf(group.id) !== 'custom' || seen.has(group.id)) continue;
    seen.add(group.id);
    custom.push(group);
  }
  return [
    layout.groups.find((g) => g.id === FAVORITES) ?? builtin(FAVORITES),
    ...custom,
    layout.groups.find((g) => g.id === UNGROUPED) ?? builtin(UNGROUPED),
  ];
}

/**
 * Every listed worktree placed in its group, in display order.
 *
 * A worktree the layout names twice is placed the first time, reading Favorites first — the same
 * rule Rust's `normalized` applies. A worktree the layout does not name goes to the end of the
 * ungrouped section. A name git does not list is skipped.
 */
export function membership(
  worktrees: readonly Worktree[],
  layout: SidebarLayout,
): Map<string, Worktree[]> {
  const byId = new Map(worktrees.map((w) => [w.id, w]));
  const placed = new Set<string>();
  const members = new Map<string, Worktree[]>();

  for (const group of ordered(layout)) {
    const rows: Worktree[] = [];
    for (const id of group.worktrees) {
      const worktree = byId.get(id);
      if (worktree && !placed.has(id)) {
        placed.add(id);
        rows.push(worktree);
      }
    }
    members.set(group.id, rows);
  }

  const unplaced = worktrees.filter((w) => !placed.has(w.id));
  members.get(UNGROUPED)?.push(...unplaced);
  return members;
}

/** One group as the sidebar renders it. */
export interface Section {
  id: string;
  kind: SectionKind;
  label: string;
  /** As stored. Whether rows are actually hidden also depends on the filter; see `open`. */
  collapsed: boolean;
  /**
   * Whether the section is showing its rows, which is what `aria-expanded` has to say. Not just
   * `!collapsed`: a filter looks inside folds, and without headings there are no folds at all.
   */
  open: boolean;
  /** Every listed member, in order, before the filter or the fold hides any. */
  members: Worktree[];
  /** What renders under the header. */
  rows: Worktree[];
  /** Members the fold is hiding — what the header's count and dot summarise. */
  hidden: Worktree[];
}

export interface Arrangement {
  sections: Section[];
  /**
   * Whether headings render at all.
   *
   * Only once something is starred or grouped, so a project with neither looks exactly as it did
   * before groups existed. While headings are off, folds are ignored too: a folded ungrouped
   * section with no header would be a list with no way to open it.
   */
  headers: boolean;
  /** Every rendered row, top to bottom. What arrow keys walk, so screen and keyboard agree. */
  rows: Worktree[];
}

export interface ArrangeOptions {
  /** The rows the filter lets through, or null when there is no filter. */
  matching: readonly Worktree[] | null;
  selectedId: string | null;
  /**
   * Whether a row must stay visible inside a folded group.
   *
   * The sidebar's status dot exists because a blocked session in another worktree used to be
   * invisible. A fold that hid it would bring that back, so a folded group still shows any row
   * whose session needs you or has failed — and the selected row, so the row you are on never
   * vanishes from under you.
   */
  urgent: (worktreeId: string) => boolean;
  /** While a row is being dragged, an empty Favorites is shown so there is somewhere to drop it. */
  dragging: boolean;
}

export function arrange(
  worktrees: readonly Worktree[],
  layout: SidebarLayout,
  options: ArrangeOptions,
): Arrangement {
  const members = membership(worktrees, layout);
  const filtering = options.matching !== null;
  const matches = new Set((options.matching ?? worktrees).map((w) => w.id));

  const all: Section[] = ordered(layout).map((group) => {
    const everyone = members.get(group.id) ?? [];
    const matched = everyone.filter((w) => matches.has(w.id));
    return {
      id: group.id,
      kind: kindOf(group.id),
      label: labelOf(group),
      collapsed: group.collapsed,
      open: true,
      members: everyone,
      rows: matched,
      hidden: [],
    };
  });

  const shown = all.filter((section) => {
    // A filter shows only where the matches are: a heading over nothing reads as a broken result.
    if (filtering) return section.rows.length > 0;
    if (section.kind === 'favorites') return section.members.length > 0 || options.dragging;
    if (section.kind === 'ungrouped') return section.members.length > 0;
    // An empty custom group stays: the user made it, and it is a place to drop rows.
    return true;
  });

  const headers = shown.some((s) => s.kind !== 'ungrouped');

  for (const section of shown) {
    // A filter looks inside folded groups. Typing a name and getting "nothing matches" because the
    // match is behind a fold would be the filter lying.
    if (!headers || filtering || !section.collapsed) continue;
    section.open = false;
    const visible = section.rows.filter(
      (w) => w.id === options.selectedId || options.urgent(w.id),
    );
    section.hidden = section.rows.filter((w) => !visible.includes(w));
    section.rows = visible;
  }

  return { sections: shown, headers, rows: shown.flatMap((s) => s.rows) };
}

// ───────────────────────────────── edits ─────────────────────────────────

/**
 * A copy that is safe to mutate. The layout handed in may be a `$state` proxy, and an edit must not
 * write through it: `editLayout` keeps the original to roll back to if the write fails.
 */
function clone(layout: SidebarLayout): SidebarLayout {
  return {
    groups: ordered(layout).map((g) => ({ ...g, worktrees: [...g.worktrees] })),
    origins: { ...layout.origins },
  };
}

function groupIn(layout: SidebarLayout, id: string): SidebarGroup | undefined {
  return layout.groups.find((g) => g.id === id);
}

/** Which group a worktree is in. One the layout does not name is ungrouped. */
export function locate(layout: SidebarLayout, worktreeId: string): string {
  for (const group of ordered(layout)) {
    if (group.worktrees.includes(worktreeId)) return group.id;
  }
  return UNGROUPED;
}

export function isStarred(layout: SidebarLayout, worktreeId: string): boolean {
  return locate(layout, worktreeId) === FAVORITES;
}

/**
 * Write a group's displayed order into it. See the module header for why an insert needs this.
 *
 * Only the ungrouped section can differ from what it stores — it also shows every listed worktree
 * placed nowhere — so only it gains anything; the others are left alone rather than losing the
 * entries git does not list right now, which are inert and cost nothing to keep.
 *
 * `all` is every listed worktree id in git's order.
 */
function materialize(layout: SidebarLayout, groupId: string, all: readonly string[]): void {
  if (groupId !== UNGROUPED) return;
  const ungrouped = groupIn(layout, UNGROUPED);
  if (!ungrouped) return;
  const placed = new Set(layout.groups.flatMap((g) => g.worktrees));
  ungrouped.worktrees.push(...all.filter((id) => !placed.has(id)));
}

/** Where a worktree is put: into `group`, just above `before`, or at the end when that is null. */
export interface Place {
  group: string;
  before: string | null;
}

/**
 * Put a worktree somewhere. Every other row-moving edit is this one.
 *
 * Moving into Favorites records where the worktree came from, so unstarring can put it back; moving
 * out of Favorites any other way — a drag, "Move to" — forgets that, because the user has just said
 * where they want it instead.
 */
export function move(
  layout: SidebarLayout,
  all: readonly string[],
  worktreeId: string,
  to: Place,
): SidebarLayout {
  const next = clone(layout);
  const from = locate(next, worktreeId);
  const target = groupIn(next, to.group) ?? groupIn(next, UNGROUPED);
  if (!target) return next;

  materialize(next, target.id, all);

  // "Just above itself" is where it already is, which is not the same as "at the end" — the answer
  // the lookup below would give once the row has been taken out.
  let before = to.before;
  if (before === worktreeId) {
    const at = target.worktrees.indexOf(worktreeId);
    before = at === -1 ? null : (target.worktrees[at + 1] ?? null);
  }

  for (const group of next.groups) {
    group.worktrees = group.worktrees.filter((id) => id !== worktreeId);
  }
  const at = before === null ? -1 : target.worktrees.indexOf(before);
  if (at === -1) target.worktrees.push(worktreeId);
  else target.worktrees.splice(at, 0, worktreeId);

  if (target.id === FAVORITES && from !== FAVORITES) {
    if (kindOf(from) === 'custom') next.origins[worktreeId] = from;
    else delete next.origins[worktreeId];
  } else if (target.id !== FAVORITES) {
    delete next.origins[worktreeId];
  }
  return next;
}

/** Star: to the end of Favorites. */
export function star(
  layout: SidebarLayout,
  all: readonly string[],
  worktreeId: string,
): SidebarLayout {
  return move(layout, all, worktreeId, { group: FAVORITES, before: null });
}

/** Unstar: back to the group it was starred from, or to the ungrouped section if that is gone. */
export function unstar(
  layout: SidebarLayout,
  all: readonly string[],
  worktreeId: string,
): SidebarLayout {
  const origin = layout.origins[worktreeId];
  const home = origin !== undefined && groupIn(layout, origin) ? origin : UNGROUPED;
  return move(layout, all, worktreeId, { group: home, before: null });
}

export function toggleStar(
  layout: SidebarLayout,
  all: readonly string[],
  worktreeId: string,
): SidebarLayout {
  return isStarred(layout, worktreeId)
    ? unstar(layout, all, worktreeId)
    : star(layout, all, worktreeId);
}

/**
 * A new custom group, last among the custom groups, optionally holding one worktree.
 *
 * The id is the caller's, because the group has to render — and be put into rename mode — before
 * any round trip could name it.
 */
export function createGroup(
  layout: SidebarLayout,
  all: readonly string[],
  group: { id: string; name: string },
  worktreeId: string | null,
): SidebarLayout {
  const next = clone(layout);
  next.groups.splice(next.groups.length - 1, 0, {
    id: group.id,
    name: group.name,
    collapsed: false,
    worktrees: [],
  });
  return worktreeId === null
    ? next
    : move(next, all, worktreeId, { group: group.id, before: null });
}

export function renameGroup(
  layout: SidebarLayout,
  groupId: string,
  name: string,
): SidebarLayout {
  const next = clone(layout);
  const group = groupIn(next, groupId);
  if (group && kindOf(groupId) === 'custom') group.name = name.trim();
  return next;
}

/**
 * Delete a custom group. Its worktrees go to the end of the ungrouped section rather than with it —
 * deleting a heading is not a reason to lose track of anything under it.
 */
export function deleteGroup(
  layout: SidebarLayout,
  all: readonly string[],
  groupId: string,
): SidebarLayout {
  if (kindOf(groupId) !== 'custom') return clone(layout);
  const next = clone(layout);
  const doomed = groupIn(next, groupId);
  if (!doomed) return next;
  const orphans = doomed.worktrees;
  next.groups = next.groups.filter((g) => g.id !== groupId);
  for (const [worktree, origin] of Object.entries(next.origins)) {
    if (origin === groupId) delete next.origins[worktree];
  }
  materialize(
    next,
    UNGROUPED,
    all.filter((id) => !orphans.includes(id)),
  );
  groupIn(next, UNGROUPED)?.worktrees.push(...orphans);
  return next;
}

/** Put a custom group just above `before` (another custom group), or last among them when null. */
export function moveGroup(
  layout: SidebarLayout,
  groupId: string,
  before: string | null,
): SidebarLayout {
  const next = clone(layout);
  if (kindOf(groupId) !== 'custom' || before === groupId) return next;
  const moving = groupIn(next, groupId);
  if (!moving) return next;
  next.groups = next.groups.filter((g) => g.id !== groupId);
  const at =
    before !== null && kindOf(before) === 'custom'
      ? next.groups.findIndex((g) => g.id === before)
      : -1;
  // -1 means last among the custom groups, which is just above ungrouped.
  next.groups.splice(at === -1 ? next.groups.length - 1 : at, 0, moving);
  return next;
}

/** Move a custom group one place up or down among the custom groups. */
export function shiftGroup(
  layout: SidebarLayout,
  groupId: string,
  delta: -1 | 1,
): SidebarLayout {
  const custom = ordered(layout).filter((g) => kindOf(g.id) === 'custom');
  const at = custom.findIndex((g) => g.id === groupId);
  if (at === -1) return clone(layout);
  const to = at + delta;
  if (to < 0 || to >= custom.length) return clone(layout);
  // Moving down one means landing above the group two below, or last.
  const before = delta < 0 ? custom[to]!.id : (custom[to + 1]?.id ?? null);
  return moveGroup(layout, groupId, before);
}

export function setCollapsed(
  layout: SidebarLayout,
  groupId: string,
  collapsed: boolean,
): SidebarLayout {
  const next = clone(layout);
  const group = groupIn(next, groupId);
  if (group) group.collapsed = collapsed;
  return next;
}

export function setAllCollapsed(layout: SidebarLayout, collapsed: boolean): SidebarLayout {
  const next = clone(layout);
  for (const group of next.groups) group.collapsed = collapsed;
  return next;
}

/**
 * The keyboard equivalent of a drag: one row up or down, measured in what is on screen.
 *
 * At the top or bottom of a section the row leaves it, for the section above (landing last) or the
 * one below (landing first). Rows inside a fold are skipped, since the only ones visible there are
 * the selected row and the ones that need you. Null when there is nowhere to go.
 */
export function nudge(
  layout: SidebarLayout,
  all: readonly string[],
  arrangement: Arrangement,
  worktreeId: string,
  delta: -1 | 1,
): SidebarLayout | null {
  const sections = arrangement.sections;
  const s = sections.findIndex((section) => section.rows.some((w) => w.id === worktreeId));
  const section = sections[s];
  if (!section) return null;
  const rows = section.rows;
  const at = rows.findIndex((w) => w.id === worktreeId);

  if (delta < 0 && at > 0) {
    return move(layout, all, worktreeId, { group: section.id, before: rows[at - 1]!.id });
  }
  if (delta > 0 && at < rows.length - 1) {
    return move(layout, all, worktreeId, {
      group: section.id,
      before: after(section.members, rows[at + 1]!.id),
    });
  }

  const neighbour = sections[s + delta];
  if (!neighbour) return null;
  return move(layout, all, worktreeId, {
    group: neighbour.id,
    before: delta < 0 ? null : (neighbour.members[0]?.id ?? null),
  });
}

/** The member after `id` in `members`, or null when it is last. */
function after(members: readonly Worktree[], id: string): string | null {
  const at = members.findIndex((w) => w.id === id);
  return at === -1 ? null : (members[at + 1]?.id ?? null);
}

// ─────────────────────────────── drag hit test ───────────────────────────────

/**
 * Something a dragged row can be dropped on, measured once when the drag arms.
 *
 * `top` and `bottom` are relative to the list's own content box, not the viewport, so the cache
 * stays right while the list scrolls under the pointer.
 */
export type Slot =
  | {
      kind: 'header';
      group: string;
      top: number;
      bottom: number;
      /** The group's first member, which a drop on the header lands above. */
      first: string | null;
    }
  | {
      kind: 'row';
      group: string;
      worktreeId: string;
      top: number;
      bottom: number;
      /** The member after this row in its group, for a drop on its lower half. */
      next: string | null;
    };

export interface RowDrop {
  place: Place;
  /** Where the insertion line is drawn, or null when the drop is onto a header. */
  line: number | null;
  /** The header to highlight, for a drop into a group. */
  header: string | null;
}

/**
 * What releasing a dragged row at `y` would do, or null for "put it back where it was".
 *
 * A header takes the row into its group, at the top — the same for a folded group, whose rows are
 * not there to aim between. A row's upper half means above it and its lower half below it. Past the
 * last slot means the end of the last section.
 */
export function rowDropAt(
  slots: readonly Slot[],
  y: number,
  dragged: string,
): RowDrop | null {
  const slot = slots.find((s) => y < s.bottom) ?? slots.at(-1);
  if (!slot) return null;

  if (slot.kind === 'header') {
    if (slot.first === dragged) return null;
    return {
      place: { group: slot.group, before: slot.first },
      line: null,
      header: slot.group,
    };
  }

  if (y > slot.bottom) {
    // Below everything.
    return { place: { group: slot.group, before: null }, line: slot.bottom, header: null };
  }
  if (slot.worktreeId === dragged) return null;
  const upper = y < (slot.top + slot.bottom) / 2;
  const before = upper ? slot.worktreeId : slot.next;
  // Just below the row above it, or just above the row below it, is where it already is.
  const own = slots.find((s) => s.kind === 'row' && s.worktreeId === dragged);
  if (before === dragged) return null;
  if (own?.kind === 'row' && own.group === slot.group && own.next === before) return null;
  return {
    place: { group: slot.group, before },
    line: upper ? slot.top : slot.bottom,
    header: null,
  };
}

/** A custom group's whole extent — header and rows — for dragging groups. */
export interface GroupSlot {
  group: string;
  top: number;
  bottom: number;
}

export interface GroupDrop {
  /** The custom group to land above, or null for last among them. */
  before: string | null;
  line: number;
}

/**
 * What releasing a dragged group header at `y` would do, or null for no change.
 *
 * Only custom groups are slots: Favorites stays first and the ungrouped section last, so the space
 * a group can move through is the run between them.
 */
export function groupDropAt(
  slots: readonly GroupSlot[],
  y: number,
  dragged: string,
): GroupDrop | null {
  const at = slots.findIndex((s) => y < (s.top + s.bottom) / 2);
  const before = at === -1 ? null : slots[at]!.group;
  const line = at === -1 ? (slots.at(-1)?.bottom ?? 0) : slots[at]!.top;

  // Above itself or just below itself is where it already is.
  const own = slots.findIndex((s) => s.group === dragged);
  const nextAfterOwn = slots[own + 1]?.group ?? null;
  if (before === dragged || before === nextAfterOwn) return null;
  return { before, line };
}
