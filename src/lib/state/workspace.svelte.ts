/**
 * Projects and worktrees.
 *
 * Three rules keep this from rotting:
 *
 * 1. **Rust owns the truth.** This is a cache. Nothing backed by *git* is mutated
 *    optimistically — a create or remove awaits the command and then refreshes, so the UI
 *    can never show a worktree that git does not agree exists. The one exception is the
 *    sidebar `layout` — order, groups, folds and stars — which is a preference in the app's
 *    own config: git has no opinion on it, so there is nothing for an edit to be wrong
 *    about. See `editLayout`.
 * 2. **No polling.** There is no `setInterval` anywhere; polling a git repo is how these
 *    tools end up spinning a fan. Refresh happens on demand and on window focus.
 * 3. **Client-owned state is only selection.** Which project and worktree are selected,
 *    and nothing else. The layout is edited here but *owned* by the config file, where it
 *    can be hand-edited and where clearing the webview's storage cannot lose it; the copy in
 *    `localStorage` is a cache of it, like the worktree list's.
 *
 * # Why the list is cached, and why it is patched rather than replaced
 *
 * Listing worktrees runs several `git` commands and reads each worktree's dotenv files, so it
 * takes long enough to see. Refresh happens on every window focus, which made switching back to
 * the app blank the sidebar and reload the detail pane — for a list that had almost always not
 * changed at all.
 *
 * Two separate fixes, because the annoyance had two separate causes:
 *
 * - **Cache first.** The last known list is kept per project, in `localStorage`, and shown
 *   immediately while the real one loads behind it. `loadingWorktrees` is now true only when
 *   there is genuinely nothing to show; a refresh over existing data sets `revalidating`, which
 *   no layout depends on.
 * - **Patch, don't replace.** Even an identical list, freshly assigned, hands every component
 *   new object identities — so effects re-run, the detail pane reloads and the terminal
 *   remounts. `merge` keeps the existing object whenever it is deep-equal to the incoming one,
 *   and skips the assignment entirely when nothing moved.
 */

import { commands } from '../ipc/commands';
import {
  errorMessage,
  type Opener,
  type Project,
  type SidebarLayout,
  type Worktree,
} from '../ipc/types';
import { emptyLayout, FAVORITES, toggleStar } from '../sidebar';
import { dropCodeCaches } from './code-cache';

const LAST_PROJECT_KEY = 'wtm.lastProject';
const LAST_WORKTREE_PREFIX = 'wtm.lastWorktree.';
const WORKTREE_CACHE_PREFIX = 'wtm.worktrees.';
/**
 * The last known sidebar layout per project, so a cold start draws the grouped list at once rather
 * than a flat one that rearranges itself a moment later. The job the per-row `favorite` flag used
 * to do by riding on the cached listing.
 */
const LAYOUT_CACHE_PREFIX = 'wtm.sidebar.';

/**
 * The label of the link last opened in each project, for the Links button's primary half.
 *
 * `localStorage`, as `wtm.lastProject` is, because it is the same kind of fact — where you were
 * last — and losing it costs one click. Not a preference like `OPENER_PREF` below: that one is a
 * setting someone chose and might hand-edit, and this is a trail nobody would. Keyed by label
 * because the label is what a project's config declares once; the URL is rendered per worktree.
 */
const LAST_LINK_PREFIX = 'wtm.lastLink.';

/**
 * Where the chosen "Open in …" tool is remembered.
 *
 * A backend preference rather than `localStorage`, unlike the two keys above: those are
 * caches of things Rust already knows, whereas this *is* the setting. It must live in
 * `~/.config/wtm/config.toml` where it can be hand-edited and where clearing the webview's
 * storage cannot lose it. Must match `OPENER_PREF` in `src-tauri/src/commands.rs`.
 */
const OPENER_PREF = 'ui.opener';

/** The cached list for a project, or null if there is none or it is unreadable. */
function readCache(projectId: string): Worktree[] | null {
  try {
    const raw = localStorage.getItem(WORKTREE_CACHE_PREFIX + projectId);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed) ? (parsed as Worktree[]) : null;
  } catch {
    // A shape change across versions must not brick the app; a miss is free.
    return null;
  }
}

function writeCache(projectId: string, worktrees: Worktree[]): void {
  try {
    localStorage.setItem(WORKTREE_CACHE_PREFIX + projectId, JSON.stringify(worktrees));
  } catch {
    /* Quota or private mode. The cache is an optimization, never a requirement. */
  }
}

/**
 * Another project's last known worktrees, for Home's tree.
 *
 * Exported wrappers rather than the functions themselves, so the cache keeps one owner: Home reads
 * what this store last wrote, and writes back what it fetched, which then makes switching to that
 * project paint its fresh list at once.
 */
export function cachedWorktrees(projectId: string): Worktree[] | null {
  return readCache(projectId);
}

export function cacheWorktrees(projectId: string, worktrees: Worktree[]): void {
  writeCache(projectId, worktrees);
}

/** The cached layout for a project, or null. Shape-checked only as far as rendering needs. */
function readLayoutCache(projectId: string): SidebarLayout | null {
  try {
    const raw = localStorage.getItem(LAYOUT_CACHE_PREFIX + projectId);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<SidebarLayout> | null;
    return parsed && Array.isArray(parsed.groups)
      ? { groups: parsed.groups, origins: parsed.origins ?? {} }
      : null;
  } catch {
    return null;
  }
}

function writeLayoutCache(projectId: string, layout: SidebarLayout): void {
  try {
    localStorage.setItem(LAYOUT_CACHE_PREFIX + projectId, JSON.stringify(layout));
  } catch {
    /* See writeCache. */
  }
}

/**
 * Drop cached lists, layouts, selections, last links and Code tab state for projects that are no
 * longer registered.
 */
function pruneCache(keep: string[]): void {
  try {
    const prefixes = [
      WORKTREE_CACHE_PREFIX,
      LAYOUT_CACHE_PREFIX,
      LAST_WORKTREE_PREFIX,
      LAST_LINK_PREFIX,
    ];
    const live = new Set(keep.flatMap((id) => prefixes.map((prefix) => prefix + id)));
    const doomed = Object.keys(localStorage).filter(
      (key) => prefixes.some((prefix) => key.startsWith(prefix)) && !live.has(key),
    );
    for (const key of doomed) localStorage.removeItem(key);
  } catch {
    /* See writeCache. */
  }
  // The Code tab's entries are per worktree, so they are matched by the project stored inside.
  const live = new Set(keep);
  dropCodeCaches((_, cache) => !live.has(cache.projectId));
}

/**
 * Reconcile `incoming` against `current`, reusing objects that have not changed.
 *
 * Returns `null` when the two lists are equivalent, so the caller can skip the assignment
 * entirely and leave every downstream effect untouched. Returning `current` instead would be
 * ambiguous: `$state` hands out a proxy, so an identity comparison at the call site is not the
 * straightforward thing it looks like.
 */
function merge(current: Worktree[], incoming: Worktree[]): Worktree[] | null {
  const byId = new Map(current.map((w) => [w.id, w]));
  let changed = incoming.length !== current.length;

  const next = incoming.map((fresh, index) => {
    const existing = byId.get(fresh.id);
    // Deep equality via JSON: a Worktree is plain serialized data, so this is exactly the
    // comparison that matters and it needs no per-field maintenance.
    const same =
      existing !== undefined && JSON.stringify(existing) === JSON.stringify(fresh);
    if (!same || current[index]?.id !== fresh.id) changed = true;
    return same ? existing : fresh;
  });

  return changed ? next : null;
}

class Workspace {
  projects = $state<Project[]>([]);
  worktrees = $state<Worktree[]>([]);

  activeProjectId = $state<string | null>(null);
  selectedWorktreeId = $state<string | null>(null);

  /** Keep repository switches working even when storage is full or unavailable. */
  private selectedWorktrees = new Map<string, string | null>();

  /**
   * How the sidebar arranges this project's worktrees. Edited only through `editLayout`.
   *
   * Loaded separately from the listing and only when a project opens: the frontend is the only
   * thing that changes it, so there is nothing a refresh on window focus could learn — and keeping
   * it off the listing is what stops a slow refresh from undoing a drag.
   */
  layout = $state<SidebarLayout>(emptyLayout());

  /** The starred ids, as a set for a per-row lookup. */
  starred = $derived(
    new Set(this.layout.groups.find((g) => g.id === FAVORITES)?.worktrees ?? []),
  );

  /**
   * Bumped by every layout edit, to detect one that raced a read or an earlier write.
   *
   * A read that started before a drag answers with the layout from before it, and applying that
   * would put the row back where it came from. Not `$state`: nothing renders it.
   */
  private layoutEpoch = 0;

  /**
   * The tail of the queue of layout writes.
   *
   * Writes are sent one at a time, in order. Each carries the whole layout, and `blocking` in Rust
   * runs commands on a thread pool — so two in flight at once could land in either order, and the
   * older one landing last would leave the file a click behind the screen.
   */
  private layoutWrites: Promise<void> = Promise.resolve();
  /** Bumped at the start of every list fetch so a stale `finally` cannot clear a newer spinner. */
  private listEpoch = 0;

  loadingProjects = $state(false);
  /** True only when there is nothing to show yet — the one case that warrants a placeholder. */
  loadingWorktrees = $state(false);
  /** A refresh happening behind an already-visible list. Deliberately not load-bearing. */
  revalidating = $state(false);
  /** True while the visible list came from the cache and has not been confirmed yet. */
  stale = $state(false);
  error = $state<string | null>(null);

  activeProject = $derived(
    this.projects.find((p) => p.id === this.activeProjectId) ?? null,
  );

  selected = $derived(this.worktrees.find((w) => w.id === this.selectedWorktreeId) ?? null);

  /**
   * The sidebar's filter text.
   *
   * Client-owned, and deliberately not persisted: a filter that survives a restart is a
   * list that looks broken until you notice the field. It also does not touch the backend —
   * the whole list is already in memory, so filtering it is a `$derived`, not a query.
   */
  query = $state('');

  /** Worktrees matching `query`, or all of them when it is empty. */
  matching = $derived.by(() => {
    // Every whitespace-separated term must match somewhere, so `8259 nfo` narrows rather
    // than failing to match a single contiguous string.
    const terms = this.query.toLowerCase().split(/\s+/).filter(Boolean);
    if (terms.length === 0) return this.worktrees;

    return this.worktrees.filter((w) => {
      // Deliberately not the full path: every worktree of a project shares its parent
      // directory, so a term matching that would match everything and look broken.
      const haystack = [w.title, w.subtitle, w.branch, w.dirname, w.issueKey]
        .filter(Boolean)
        .join(' ')
        .toLowerCase();
      return terms.every((term) => haystack.includes(term));
    });
  });

  /** True when a filter is hiding something, so the sidebar can say so. */
  filtering = $derived(this.query.trim().length > 0);

  /** Projects that could not be loaded — shown as a banner, never hidden. */
  brokenProjects = $derived(this.projects.filter((p) => !p.usable));

  /**
   * Every tool the machine can open a worktree in, and which one is preferred.
   *
   * App-level rather than component-level because it is a property of the machine, not of
   * the selection: holding it here means switching worktrees does not re-probe, and the
   * split button does not flicker through an empty state on every click in the sidebar.
   */
  openers = $state<Opener[]>([]);
  preferredOpener = $state<string | null>(null);

  /**
   * Links opened since launch, by project, over what `localStorage` said.
   *
   * State rather than a read each time because a read is not reactive: without this the button
   * would open the link and keep its old label until something else re-rendered it.
   */
  private openedLinks = $state<Record<string, string>>({});

  /**
   * Know one worktree, without listing or selecting anything. For a pane window.
   *
   * `init` reads the project list, lands on the last project and writes it back as the last one —
   * all of which is the main window's to do, and the last of which a pane window would get wrong.
   * What a popped-out pane needs is only the worktree it is in, for a browser's links and home page,
   * and the main window hands that over.
   */
  seed(projectId: string, worktree: Worktree | null): void {
    this.activeProjectId = projectId;
    this.worktrees = worktree ? [worktree] : [];
  }

  async init(): Promise<void> {
    // Not awaited: the picker is the last thing anyone reaches for, and a slow PATH probe
    // must not hold up the worktree list. Failure is silent by design — see below.
    void this.refreshOpeners();

    await this.refreshProjects();

    // Prefer the last project, so reopening the app lands where you left off.
    const remembered = localStorage.getItem(LAST_PROJECT_KEY);
    const target =
      this.projects.find((p) => p.id === remembered) ??
      this.projects.find((p) => p.usable) ??
      this.projects[0];

    if (target) await this.selectProject(target.id);
  }

  /**
   * Re-probe the machine for installed editors.
   *
   * Called on start and again whenever the picker is opened, so an editor installed while
   * wtm was running shows up without a restart — Rust deliberately caches nothing.
   *
   * A failure here leaves `openers` empty and the control unrendered, which is the right
   * outcome: this is an auxiliary convenience, and surfacing "could not list editors" in
   * the banner reserved for git and config errors would be noise.
   */
  async refreshOpeners(): Promise<void> {
    try {
      const listed = await commands.listOpeners();
      this.openers = listed.openers;
      this.preferredOpener = listed.preferred;
    } catch {
      /* Deliberately silent. See above. */
    }
  }

  /**
   * Remember a tool as the default, optimistically.
   *
   * Applied locally first so the button relabels on click rather than after a round trip —
   * the same reasoning as `editLayout`, and safe for the same reason: nothing else in the
   * system has an opinion about this value, so there is nothing to be contradicted by.
   */
  async setPreferredOpener(openerId: string): Promise<void> {
    const previous = this.preferredOpener;
    this.preferredOpener = openerId;
    try {
      await commands.setPref(OPENER_PREF, openerId);
    } catch (e) {
      this.preferredOpener = previous;
      this.error = errorMessage(e);
    }
  }

  /** The label of the link last opened in `projectId`, or null if none has been. */
  lastLink(projectId: string): string | null {
    if (projectId in this.openedLinks) return this.openedLinks[projectId] ?? null;
    try {
      return localStorage.getItem(LAST_LINK_PREFIX + projectId);
    } catch {
      return null;
    }
  }

  rememberLink(projectId: string, label: string): void {
    this.openedLinks[projectId] = label;
    try {
      localStorage.setItem(LAST_LINK_PREFIX + projectId, label);
    } catch {
      /* See writeCache. */
    }
  }

  async refreshProjects(): Promise<void> {
    this.loadingProjects = true;
    try {
      this.projects = await commands.listProjects();
      this.error = null;
    } catch (e) {
      this.error = errorMessage(e);
    } finally {
      this.loadingProjects = false;
    }
  }

  async selectProject(projectId: string): Promise<void> {
    this.activeProjectId = projectId;
    // A filter typed for one project means nothing in the next, and carrying it over would
    // present the new project as an empty list.
    this.query = '';

    // Show the cached list at once. Switching projects used to blank the sidebar and the detail
    // pane for as long as the git calls took, which is the whole reason this hurt.
    const cached = readCache(projectId);
    this.worktrees = cached ?? [];
    this.layout = readLayoutCache(projectId) ?? emptyLayout();
    void this.loadLayout(projectId);
    this.stale = cached !== null;
    // The cache can predate the remembered selection. Only a fresh listing can prove that
    // worktree was removed; falling back here would forget it before git has answered.
    this.selectedWorktreeId =
      this.lastWorktree(projectId) ??
      (cached?.find((w) => w.isMain) ?? cached?.[0])?.id ??
      null;

    try {
      localStorage.setItem(LAST_PROJECT_KEY, projectId);
    } catch {
      /* Not worth failing over. */
    }
    await this.refreshWorktrees();
  }

  /** Drop a confirmed removal even if the subsequent refresh fails. Never change projects. */
  async afterRemoval(projectId: string, worktreeId: string): Promise<void> {
    const cached = readCache(projectId);
    if (cached)
      writeCache(
        projectId,
        cached.filter((w) => w.id !== worktreeId),
      );
    try {
      localStorage.removeItem(LAYOUT_CACHE_PREFIX + projectId);
    } catch {
      /* Cache only. */
    }
    if (this.activeProjectId !== projectId) return;
    this.worktrees = this.worktrees.filter((w) => w.id !== worktreeId);
    if (this.selectedWorktreeId === worktreeId)
      this.select(
        this.worktrees.find((w) => w.isMain)?.id ?? this.worktrees[0]?.id ?? null,
      );
    await this.refreshWorktrees();
    await this.loadLayout(projectId);
  }

  async refreshWorktrees(): Promise<void> {
    const projectId = this.activeProjectId;
    if (!projectId) {
      this.worktrees = [];
      this.stale = false;
      return;
    }

    // A project awaiting trust approval has no worktrees to show; asking would only
    // produce the same error the banner is already reporting.
    if (this.activeProject && !this.activeProject.usable) {
      this.worktrees = [];
      this.stale = false;
      return;
    }

    // Only claim to be "loading" when the screen is empty. Otherwise this is a background
    // revalidation and the list stays exactly where it is.
    const cold = this.worktrees.length === 0;
    if (cold) this.loadingWorktrees = true;
    else this.revalidating = true;

    const epoch = ++this.listEpoch;

    try {
      const list = await commands.listWorktrees(projectId);

      // Guard against a slow response for a project the user has since navigated away from.
      if (this.activeProjectId !== projectId) return;
      if (epoch !== this.listEpoch) return;

      const merged = merge(this.worktrees, list);
      if (merged !== null) this.worktrees = merged;
      this.stale = false;
      this.error = null;
      writeCache(projectId, list);

      // Keep the selection if it survived the refresh; otherwise fall back to the main
      // worktree, which is the one that always exists.
      const selected =
        list.find((w) => w.id === this.selectedWorktreeId) ??
        list.find((w) => w.isMain) ??
        list[0];
      this.select(selected?.id ?? null);
    } catch (e) {
      if (epoch !== this.listEpoch) return;
      this.error = errorMessage(e);
      // Keep whatever is on screen. A failed refresh is a reason to show a retry banner, not
      // a reason to throw away a list that was correct a moment ago.
      if (this.worktrees.length === 0) this.stale = false;
    } finally {
      if (epoch === this.listEpoch) {
        this.loadingWorktrees = false;
        this.revalidating = false;
      }
    }
  }

  private lastWorktree(projectId: string): string | null {
    if (this.selectedWorktrees.has(projectId)) {
      return this.selectedWorktrees.get(projectId) ?? null;
    }
    try {
      return localStorage.getItem(LAST_WORKTREE_PREFIX + projectId);
    } catch {
      return null;
    }
  }

  select(worktreeId: string | null): void {
    this.selectedWorktreeId = worktreeId;
    const projectId = this.activeProjectId;
    if (!projectId) return;
    this.selectedWorktrees.set(projectId, worktreeId);
    try {
      if (worktreeId === null) localStorage.removeItem(LAST_WORKTREE_PREFIX + projectId);
      else localStorage.setItem(LAST_WORKTREE_PREFIX + projectId, worktreeId);
    } catch {
      /* The in-memory selection still survives a repository switch. */
    }
  }

  /**
   * Read the stored layout for a project, unless an edit has overtaken the read.
   *
   * Failure keeps whatever is on screen — the cached layout, or the empty one — and says nothing:
   * the listing's own refresh reports a broken config, and a second banner for the same cause
   * would only be noise.
   */
  private async loadLayout(projectId: string): Promise<void> {
    const epoch = this.layoutEpoch;
    try {
      const stored = await commands.sidebarLayout(projectId);
      if (this.activeProjectId !== projectId || this.layoutEpoch !== epoch) return;
      this.adoptLayout(projectId, stored);
    } catch {
      /* See above. */
    }
  }

  /** Take `layout` as the current one, skipping the assignment when nothing changed. */
  private adoptLayout(projectId: string, layout: SidebarLayout): void {
    // The same reason `merge` exists: a fresh object for an identical layout would re-derive every
    // section and re-render every row, which mid-drag moves the rows the drag measured.
    if (JSON.stringify($state.snapshot(this.layout)) !== JSON.stringify(layout)) {
      this.layout = layout;
    }
    writeLayoutCache(projectId, layout);
  }

  /**
   * Apply an edit to the sidebar layout now, then write it behind the click.
   *
   * Optimistic for the reason `setPreferredOpener` is: nothing but the user has an opinion on where
   * a row goes, so there is nothing for the write to be contradicted by. What comes back is the
   * layout as Rust normalized it, and that is adopted — it is how an edit the invariants reject is
   * corrected on screen rather than only on disk. A failed write puts back the layout from before
   * this edit, unless a newer edit has already replaced it; that one's write carries this edit too,
   * since every write is the whole layout.
   *
   * `edit` gets a plain snapshot and every listed id in git's order, and must return a new layout;
   * every edit in `sidebar.ts` has that shape.
   */
  editLayout(edit: (layout: SidebarLayout, all: readonly string[]) => SidebarLayout): void {
    const projectId = this.activeProjectId;
    if (!projectId) return;
    const before = $state.snapshot(this.layout);
    const next = edit(
      before,
      this.worktrees.map((w) => w.id),
    );
    const epoch = ++this.layoutEpoch;
    this.layout = next;
    writeLayoutCache(projectId, next);

    const current = () => this.activeProjectId === projectId && this.layoutEpoch === epoch;
    this.layoutWrites = this.layoutWrites.then(async () => {
      try {
        const stored = await commands.setSidebarLayout(projectId, next);
        if (current()) this.adoptLayout(projectId, stored);
      } catch (e) {
        if (current()) {
          this.layout = before;
          writeLayoutCache(projectId, before);
        }
        this.error = errorMessage(e);
      }
    });
  }

  isFavorite(worktreeId: string): boolean {
    return this.starred.has(worktreeId);
  }

  /**
   * Star or unstar a worktree: into Favorites, or back to the group it was starred from.
   *
   * Starring is a move rather than a flag, so the row changes place — see `sidebar.ts`.
   */
  toggleFavorite(worktreeId: string): void {
    if (!this.worktrees.some((w) => w.id === worktreeId)) return;
    this.editLayout((layout, all) => toggleStar(layout, all, worktreeId));
  }

  /**
   * Register a repository and switch to it.
   *
   * The id comes back from the backend rather than being matched here. This used to guess —
   * `p.root === path || path.startsWith(p.root)` — which never matched a tilde path, because
   * `path` is the raw string typed into the dialog and `root` is what git resolved. Since the
   * dialog's own placeholder is `~/Sites/your-repo`, the common case silently added the project
   * and then stayed where it was. The prefix half had its own bug: with no separator boundary,
   * adding `/x/foo/src` could select an existing project at `/x/f`.
   */
  async addProject(path: string): Promise<void> {
    const { id, projects } = await commands.registerProject(path);
    this.projects = projects;
    await this.selectProject(id);
  }

  async removeProject(path: string): Promise<void> {
    this.projects = await commands.unregisterProject(path);
    // Prune against the surviving projects rather than deleting the key for `path`, so a cache
    // entry cannot outlive the project it belongs to whatever the caller passed.
    pruneCache(this.projects.map((p) => p.id));
    const live = new Set(this.projects.map((p) => p.id));
    for (const projectId of this.selectedWorktrees.keys()) {
      if (!live.has(projectId)) this.selectedWorktrees.delete(projectId);
    }
    if (this.activeProject === null || this.activeProjectId === path) {
      this.activeProjectId = null;
      this.selectedWorktreeId = null;
      this.worktrees = [];
      this.layout = emptyLayout();
      this.stale = false;
      const next = this.projects.find((p) => p.usable) ?? this.projects[0];
      if (next) await this.selectProject(next.id);
    }
  }

  /** Approve or reject a project's config, then reload it. */
  async decideTrust(path: string, approve: boolean): Promise<void> {
    this.projects = await commands.setConfigTrust(path, approve);
    await this.refreshWorktrees();
  }
}

export const workspace = new Workspace();
