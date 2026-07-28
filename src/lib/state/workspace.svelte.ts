/**
 * Projects and worktrees.
 *
 * Three rules keep this from rotting:
 *
 * 1. **Rust owns the truth.** This is a cache. Nothing is mutated optimistically — a
 *    create or remove awaits the command and then refreshes, so the UI can never show a
 *    worktree that git does not agree exists.
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
import { errorMessage, type Project, type Worktree } from '../ipc/types';

const LAST_PROJECT_KEY = 'wtm.lastProject';
const WORKTREE_CACHE_PREFIX = 'wtm.worktrees.';

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

  /** Projects that could not be loaded — shown as a banner, never hidden. */
  brokenProjects = $derived(this.projects.filter((p) => !p.usable));

  async init(): Promise<void> {
    await this.refreshProjects();

    // Prefer the last project, so reopening the app lands where you left off.
    const remembered = localStorage.getItem(LAST_PROJECT_KEY);
    const target =
      this.projects.find((p) => p.id === remembered) ??
      this.projects.find((p) => p.usable) ??
      this.projects[0];

    if (target) await this.selectProject(target.id);
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

    try {
      const list = await commands.listWorktrees(projectId);

      // Guard against a slow response for a project the user has since navigated away from.
      if (this.activeProjectId !== projectId) return;

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
      this.error = errorMessage(e);
      // Keep whatever is on screen. A failed refresh is a reason to show a retry banner, not
      // a reason to throw away a list that was correct a moment ago.
      if (this.worktrees.length === 0) this.stale = false;
    } finally {
      this.loadingWorktrees = false;
      this.revalidating = false;
    }
  }

  select(worktreeId: string): void {
    this.selectedWorktreeId = worktreeId;
  }

  /** Move the selection by `delta`, for keyboard navigation. */
  selectRelative(delta: number): void {
    if (this.worktrees.length === 0) return;
    const current = this.worktrees.findIndex((w) => w.id === this.selectedWorktreeId);
    const next = Math.min(
      Math.max((current === -1 ? 0 : current) + delta, 0),
      this.worktrees.length - 1,
    );
    this.selectedWorktreeId = this.worktrees[next]?.id ?? this.selectedWorktreeId;
  }

  async addProject(path: string): Promise<void> {
    this.projects = await commands.registerProject(path);
    // Select whatever was just added, matching the root the backend resolved.
    const added = this.projects.find((p) => p.root === path || path.startsWith(p.root));
    if (added) await this.selectProject(added.id);
  }

  async removeProject(path: string): Promise<void> {
    this.projects = await commands.unregisterProject(path);
    // Prune against the surviving projects rather than deleting the key for `path`. Unregister
    // accepts any path inside a repository, so `path` is not necessarily the project's id.
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
