/**
 * Projects and worktrees.
 *
 * Three rules keep this from rotting:
 *
 * 1. **Rust owns the truth.** This is a cache. Nothing backed by *git* is mutated
 *    optimistically — a create or remove awaits the command and then refreshes, so the UI
 *    can never show a worktree that git does not agree exists. The one exception is
 *    `favorite`, which is a preference in the app's own config: git has no opinion on it,
 *    so there is nothing for the flip to be wrong about. See `toggleFavorite`.
 * 2. **No polling.** There is no `setInterval` anywhere; polling a git repo is how these
 *    tools end up spinning a fan. Refresh happens on demand and on window focus.
 * 3. **Client-owned state is only selection.** Which project and worktree are selected,
 *    and nothing else.
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
import { errorMessage, type Opener, type Project, type Worktree } from '../ipc/types';

const LAST_PROJECT_KEY = 'wtm.lastProject';
const WORKTREE_CACHE_PREFIX = 'wtm.worktrees.';

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

/** Drop cached lists for projects that are no longer registered. */
function pruneCache(keep: string[]): void {
  try {
    const live = new Set(keep.map((id) => WORKTREE_CACHE_PREFIX + id));
    const doomed = Object.keys(localStorage).filter(
      (key) => key.startsWith(WORKTREE_CACHE_PREFIX) && !live.has(key),
    );
    for (const key of doomed) localStorage.removeItem(key);
  } catch {
    /* See writeCache. */
  }
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

  /**
   * Bumped by every star toggle, to detect one that raced an in-flight refresh.
   *
   * A refresh reads favorites from the config as it starts, so a star clicked while it is
   * still running produces a response that says "not starred" — and applying it would make
   * the star the user just clicked flick back off. Not `$state`: nothing renders it.
   */
  private favoriteEpoch = 0;
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

  /**
   * The visible list split by star, each half keeping git's own order.
   *
   * Sorting within a group is deliberately left alone: git lists the main worktree first
   * and the rest in a stable order, and imposing an alphabetical sort on top would move
   * rows the user has learned the position of.
   */
  favorites = $derived(this.matching.filter((w) => w.favorite));
  others = $derived(this.matching.filter((w) => !w.favorite));

  /**
   * Display order: starred first.
   *
   * The sidebar renders from `favorites` and `others`, but keyboard navigation has to walk
   * *this* — arrow keys that step through `worktrees` while the eye sees a grouped list
   * would jump around the screen.
   */
  ordered = $derived([...this.favorites, ...this.others]);

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
   * the same reasoning as `toggleFavorite`, and safe for the same reason: nothing else in
   * the system has an opinion about this value, so there is nothing to be contradicted by.
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
    this.stale = cached !== null;
    this.selectedWorktreeId = cached
      ? ((cached.find((w) => w.isMain) ?? cached[0])?.id ?? null)
      : null;

    try {
      localStorage.setItem(LAST_PROJECT_KEY, projectId);
    } catch {
      /* Not worth failing over. */
    }
    await this.refreshWorktrees();
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

    const favoriteAt = this.favoriteEpoch;
    const epoch = ++this.listEpoch;

    try {
      const list = await commands.listWorktrees(projectId);

      // Guard against a slow response for a project the user has since navigated away from.
      if (this.activeProjectId !== projectId) return;
      if (epoch !== this.listEpoch) return;

      // A star clicked while this was in flight is newer than what came back, and has
      // already been written to disk. Keep it rather than letting the stale answer win.
      if (this.favoriteEpoch !== favoriteAt) {
        const local = new Map(this.worktrees.map((w) => [w.id, w.favorite]));
        for (const fresh of list) {
          const known = local.get(fresh.id);
          if (known !== undefined) fresh.favorite = known;
        }
      }

      const merged = merge(this.worktrees, list);
      if (merged !== null) this.worktrees = merged;
      this.stale = false;
      this.error = null;
      writeCache(projectId, list);

      // Keep the selection if it survived the refresh; otherwise fall back to the main
      // worktree, which is the one that always exists.
      const stillThere = list.some((w) => w.id === this.selectedWorktreeId);
      if (!stillThere) {
        this.selectedWorktreeId = (list.find((w) => w.isMain) ?? list[0])?.id ?? null;
      }
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

  select(worktreeId: string): void {
    this.selectedWorktreeId = worktreeId;
  }

  /** Move the selection by `delta`, for keyboard navigation. */
  selectRelative(delta: number): void {
    // `ordered`, not `worktrees`: this has to follow what is on screen.
    const list = this.ordered;
    if (list.length === 0) return;
    const current = list.findIndex((w) => w.id === this.selectedWorktreeId);
    // Nothing selected: ArrowDown starts *on* the first row, ArrowUp on the last. Starting
    // from 0 then adding delta used to skip the first row on the way down.
    const from = current === -1 ? (delta > 0 ? -1 : list.length) : current;
    const next = Math.min(Math.max(from + delta, 0), list.length - 1);
    this.selectedWorktreeId = list[next]?.id ?? this.selectedWorktreeId;
  }

  /**
   * Star or unstar a worktree.
   *
   * Flipped locally before the call, unlike a create or a remove. Those await git because
   * git could disagree; a star is a preference in the app's own config, so there is nothing
   * to be wrong about and no reason to make a click wait on a disk write. Same shape as a
   * theme change. On failure the flag goes back and the error is surfaced.
   */
  async toggleFavorite(worktreeId: string): Promise<void> {
    const projectId = this.activeProjectId;
    const worktree = this.worktrees.find((w) => w.id === worktreeId);
    if (!projectId || !worktree) return;

    const next = !worktree.favorite;
    worktree.favorite = next;
    this.favoriteEpoch += 1;

    try {
      await commands.setWorktreeFavorite(projectId, worktreeId, next);
      // Keep the cache in step, or a restart would show the pre-click stars until the
      // first refresh lands.
      writeCache(projectId, this.worktrees);
      this.error = null;
    } catch (e) {
      worktree.favorite = !next;
      this.error = errorMessage(e);
    }
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
    if (this.activeProject === null || this.activeProjectId === path) {
      this.activeProjectId = null;
      this.worktrees = [];
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
