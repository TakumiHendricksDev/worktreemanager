<script lang="ts">
  /**
   * The app shell: a two-column grid with a resizable splitter.
   *
   * Refresh policy lives here, and it is deliberately event-driven. There is no
   * `setInterval` anywhere in this codebase — polling a git repo is how these tools end
   * up spinning a fan on a laptop. Instead: refresh on window focus, which covers the
   * case that actually matters (you did something in a terminal and came back).
   */
  import { listen } from '@tauri-apps/api/event';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { onMount, untrack } from 'svelte';

  import AddProjectDialog from './lib/components/AddProjectDialog.svelte';
  import CodeSurface from './lib/components/CodeSurface.svelte';
  import { chordOf } from './lib/code-shortcuts';
  import { code } from './lib/state/code.svelte';
  import { codeRequests } from './lib/state/code-request.svelte';
  import DatabaseSurface from './lib/components/DatabaseSurface.svelte';
  import Detail from './lib/components/Detail.svelte';
  import HomeShellPermissions from './lib/components/HomeShellPermissions.svelte';
  import RunShellDialog from './lib/components/RunShellDialog.svelte';
  import { shellRuns } from './lib/state/shell-runs.svelte';
  import FleetTree from './lib/components/FleetTree.svelte';
  import HomeAgent from './lib/components/HomeAgent.svelte';
  import HomeSurface from './lib/components/HomeSurface.svelte';
  import NewWorktreePane from './lib/components/NewWorktreePane.svelte';
  import RemoveProjectDialog from './lib/components/RemoveProjectDialog.svelte';
  import RemoveWorktreeDialog from './lib/components/RemoveWorktreeDialog.svelte';
  import Inspector from './lib/components/Inspector.svelte';
  import SessionSurface from './lib/components/SessionSurface.svelte';
  import SettingsDialog from './lib/components/SettingsDialog.svelte';
  import UsageDialog from './lib/components/UsageDialog.svelte';
  import Sidebar from './lib/components/Sidebar.svelte';
  import TitleBar from './lib/components/TitleBar.svelte';
  import Toasts from './lib/components/Toasts.svelte';
  import TrustBanner from './lib/components/TrustBanner.svelte';
  import UpdateDialog from './lib/components/UpdateDialog.svelte';
  import Banner from './lib/components/ui/Banner.svelte';
  import Button from './lib/components/ui/Button.svelte';
  import Icon from './lib/components/ui/Icon.svelte';
  import Logo from './lib/components/ui/Logo.svelte';
  import Splitter from './lib/components/ui/Splitter.svelte';
  import { commands } from './lib/ipc/commands';
  import { errorMessage, type NotificationClick, type Project } from './lib/ipc/types';
  import { attention } from './lib/state/attention.svelte';
  import { worktreeRemoval } from './lib/state/worktree-removal.svelte';
  import { treePrefs } from './lib/state/tree-prefs.svelte';
  import { composerPrefs } from './lib/state/composer.svelte';
  import { dictation } from './lib/state/dictate.svelte';
  import { initMainWindow } from './lib/state/pane-windows.svelte';
  import { sessions } from './lib/state/sessions.svelte';
  import { sessionAwareness } from './lib/state/session-awareness.svelte';
  import { browserTools } from './lib/state/browser-tools.svelte';
  import { restoreSessions } from './lib/state/restore-sessions.svelte';
  import { browsers } from './lib/state/browsers.svelte';
  import { theme } from './lib/state/theme.svelte';
  import { updates } from './lib/state/update.svelte';
  import { usage } from './lib/state/usage.svelte';
  import { databaseConsole } from './lib/state/database-console.svelte';
  import { fleet } from './lib/state/fleet.svelte';
  import { view } from './lib/state/view.svelte';
  import { workspace } from './lib/state/workspace.svelte';
  import { isHome } from './lib/home';
  import type { Pane } from './lib/state/sessions.svelte';

  const MIN_SIDEBAR = 200;
  const MAX_SIDEBAR = 460;
  const DEFAULT_SIDEBAR = 276;
  const SIDEBAR_WIDTH_PREF = 'ui.sidebar_width';
  const SIDEBAR_COLLAPSED_PREF = 'ui.sidebar_collapsed';

  let sidebarWidth = $state(DEFAULT_SIDEBAR);
  let sidebarCollapsed = $state(false);
  let dragging = $state(false);
  let booted = $state(false);
  let addError = $state<string | null>(null);
  /*
   * What the main pane shows lives in `view.svelte.ts` now, because Home made it something
   * `attention` has to read too.
   *
   * New Worktree is a *view*, not a modal: the form, the review screen and a live setup
   * terminal need the room, and a modal implies a quick decision when a setup run can take
   * minutes. Removal stays a modal — a destructive confirmation should block.
   */
  let showAddProject = $state(false);

  /**
   * The project being removed, captured when the dialog opens.
   *
   * Not `workspace.activeProject` read at render time: removal selects the next project before it
   * finishes, and a dialog reading the active one would spend its last frames naming that instead.
   */
  let removingProject = $state<Project | null>(null);
  let showSettings = $state(false);
  let showUsage = $state(false);
  let showInspector = $state(false);
  /**
   * Teardown for the session event listeners.
   *
   * Not `$state`: nothing renders it, and making it reactive would put a mount-time write inside
   * whatever effect happened to read it.
   */
  let offSessions: (() => void) | null = null;
  /** The focus/blur listeners behind the notification gate. Same contract as `offSessions`. */
  let offAttention: (() => void) | null = null;
  /** Hearing the windows panes are popped out into. See `pane-windows.svelte.ts`. */
  let offWindows: (() => void) | null = null;
  /** The message log Home draws its wires from. Same contract as `offSessions`. */
  let offFleet: (() => void) | null = null;
  let offShellRuns: (() => void) | null = null;

  /**
   * The one navigation recipe. Notification clicks and toast clicks both land here, because
   * both name the same kind of target and both have to change the view — a click that selected
   * a worktree while the create pane or Home owned the screen would look like it had done
   * nothing.
   *
   * Order matters: `selectProject` is awaited because it refreshes the worktree list and
   * resets the selection, so the `select` after it is the one that sticks. The pane focus is
   * best-effort — pane ids are process-local, so a notification can outlive its pane. The
   * durable half of the address is (project, worktree); a stale pane means arrive, don't
   * focus.
   */
  async function goTo(target: NotificationClick): Promise<void> {
    // A click can arrive before init finishes — the notification is what relaunched the app.
    // There is nothing to navigate yet, and boot lands on the last active project on its own;
    // the click still brought the window to the front, which is all it ever did before.
    if (!booted) return;
    // Home's own pane has no worktree to arrive at; it is on Home, and Home is a view.
    if (isHome(target.worktreeId)) {
      view.show('home');
      view.peek(null);
      sessions.markPaneSeen(target.paneId);
      attention.clearPane(target.paneId);
      return;
    }
    await arrive(target.projectId, target.worktreeId);
    view.show('worktree');
    const alive = sessions.panes.some(
      (p) => p.id === target.paneId && p.worktreeId === target.worktreeId,
    );
    if (alive) sessions.focus(target.worktreeId, target.paneId);
    // Arriving is what clears the worktree's dots and toasts, exactly as ⌘-Tabbing back does.
    sessions.markSeen(target.worktreeId);
  }

  /**
   * From Home to a pane in its own worktree.
   *
   * `goTo`, and then one more step for a delegated child, which holds no tile: arriving at its
   * worktree would show the panes around it and not the child itself, so it is put in a tile the
   * way the Agents dialog's Show does.
   */
  async function reveal(pane: Pane): Promise<void> {
    await goTo({ projectId: pane.projectId, worktreeId: pane.worktreeId, paneId: pane.id });
    if (pane.parentSession !== null) sessions.showRelated(pane.id);
  }

  /** From Home to a worktree, with nothing in particular to focus. */
  async function openWorktree(projectId: string, worktreeId: string): Promise<void> {
    await arrive(projectId, worktreeId);
    view.show('worktree');
    sessions.markSeen(worktreeId);
  }

  /** Bring a worktree up: the half of `goTo` a reply's Run shares, before it picks a view. */
  async function arrive(projectId: string, worktreeId: string): Promise<void> {
    if (projectId !== workspace.activeProjectId) {
      await workspace.selectProject(projectId);
    }
    workspace.select(worktreeId);
  }

  /** The last request navigated for. Not state: it only stops one request navigating twice. */
  let navigatedFor = 0;

  /*
   * A reply's Run: to the worktree the pane is in, and its Database view. `DatabaseSurface` takes
   * the same request from there and does the running — see `database-console.svelte.ts`.
   */
  $effect(() => {
    const request = databaseConsole.request;
    if (!booted || !request || request.id === navigatedFor) return;
    navigatedFor = request.id;
    untrack(() => {
      void arrive(request.projectId, request.worktreeId).then(() => view.show('database'));
    });
  });

  /** The last file request navigated for. Same contract as `navigatedFor`. */
  let openedFor = 0;

  /*
   * A reply's file link: to the worktree the pane is in, and its Code view. `CodeSurface` takes the
   * same request from there and opens the file — see `code-request.svelte.ts`.
   */
  $effect(() => {
    const request = codeRequests.request;
    if (!booted || !request || request.id === openedFor) return;
    openedFor = request.id;
    untrack(() => {
      void arrive(request.projectId, request.worktreeId).then(() => view.show('code'));
    });
  });

  onMount(() => {
    let gone = false;
    void (async () => {
      await theme.init();

      // Before `sessions.init` for the same reason `attention.init` is: a composer can be typed
      // into the moment a pane mounts, and reading this late would send the first Enter of the
      // session under the default rather than the chosen behaviour.
      await composerPrefs.init();
      await treePrefs.init();
      await sessionAwareness.init();
      await browsers.init();
      await browserTools.init();
      // Before `sessions.init`, which reads it once to decide whether to restore the last run.
      await restoreSessions.init();
      await dictation.init();
      if (gone) return;

      const [storedWidth, storedCollapsed] = await Promise.all([
        commands.getPref(SIDEBAR_WIDTH_PREF).catch(() => null),
        commands.getPref(SIDEBAR_COLLAPSED_PREF).catch(() => null),
      ]);
      const parsed = storedWidth ? Number.parseInt(storedWidth, 10) : NaN;
      if (Number.isFinite(parsed)) {
        sidebarWidth = Math.min(Math.max(parsed, MIN_SIDEBAR), MAX_SIDEBAR);
      }
      sidebarCollapsed = storedCollapsed === 'true';

      // Before `sessions.init`, because that one starts delivering events and every event is judged
      // against the notification preference this reads. Started in the other order, the first
      // approval of a session adopted from a reload would be judged as "not asked yet" whatever the
      // user had already answered.
      offAttention = await attention.init();
      if (gone) {
        offAttention?.();
        return;
      }

      // Awaited, unlike the worktree list it precedes: this subscribes to every `pty:*` and
      // `agent:*` stream, and a session whose events arrive before the listeners attach streams
      // into nothing. Nothing can start a session until there is a worktree list to start one
      // from, so paying for this first costs nothing visible.
      offSessions = await sessions.init();
      if (gone) {
        offSessions?.();
        offAttention?.();
        return;
      }
      // After the panes exist, because every one of these events names one.
      offWindows = await initMainWindow();
      if (gone) {
        offWindows?.();
        offSessions?.();
        offAttention?.();
        return;
      }
      // After the sessions, because each exchange names two of them.
      offFleet = await fleet.init();
      if (gone) {
        offFleet?.();
        offWindows?.();
        offSessions?.();
        offAttention?.();
        return;
      }

      await workspace.init();
      if (gone) return;
      booted = true;
      offShellRuns = await shellRuns.init();
      if (gone) offShellRuns();

      // Last, and not awaited: a network round trip must not hold up a window that is otherwise
      // ready, and nothing above depends on its answer.
      void updates.init();
    })();

    const onFocus = () => {
      if (booted) void workspace.refreshWorktrees();
      /*
       * Coming back to the window is looking at what is on screen.
       *
       * The third and most important of the three routes that clear an unread mark, because it is the
       * common case by a wide margin: an agent finishes a turn while you are in another application,
       * you ⌘-Tab back, and the pane is right there. Without this the dot would insist you had not
       * seen a thing you were looking at.
       */
      // Not while Home is up, where the selected worktree is no more on screen than any other —
      // `HomeSurface` marks what Home shows instead.
      if (!view.home) sessions.markSeen(workspace.selectedWorktreeId);
      // At most daily, and only once the launch check has answered. See `update.svelte.ts`.
      updates.onFocus();
    };
    window.addEventListener('focus', onFocus);

    /*
     * Settings from the macOS app menu.
     *
     * AppKit handles ⌘, itself and the keystroke never reaches the webview, so the menu
     * item emits an event instead — the same route `pty_bridge.rs` uses for progress. The
     * listener is unconditional because a platform with no menu simply never fires it.
     */
    const unlistenSettings = listen('wtm:settings', () => (showSettings = true));
    /* Check for Updates… from the same menu, by the same route. Always runs and always answers. */
    const unlistenUpdates = listen('wtm:check-updates', () => void updates.check(true));
    /* Go › Home from the same menu, by the same route: AppKit keeps ⇧⌘H for itself. */
    const unlistenHome = listen('wtm:home', () => {
      if (booted) view.toggleHome();
    });
    /*
     * The usage registry. Independent of everything above, so not in the boot chain: the figures
     * come from Rust's record whenever they land, and nothing waits on them.
     */
    const unlistenUsage = usage.init();

    /*
     * A macOS notification was clicked. The payload is the pane's address, attached by
     * `notifier.rs` when the notification was posted — see `wtm-notify` for the round trip.
     * Unconditional for the same reason as the settings listener: a platform whose
     * notifications cannot carry a click simply never fires it.
     */
    const unlistenClicks = listen<NotificationClick>('notification:clicked', (event) => {
      void goTo(event.payload);
    });

    const onKey = (event: KeyboardEvent) => {
      const meta = event.metaKey || event.ctrlKey;

      /*
       * ⌘R / Ctrl-R to refresh — except inside the terminal dock.
       *
       * Ctrl-R in a shell is reverse-search, and this handler used to swallow it. That was noted
       * here and deferred while the only terminals in the app were transcripts nobody types
       * into; the dock made it a keystroke people actually press, so it is now fixed.
       *
       * The guard is the dock's id rather than `inTextEntry`, and the distinction matters:
       * `inTextEntry` is true for the sidebar's filter field as well, where ⌘R should still
       * refresh, and it is true for xterm's textarea *by design* — see its own comment. Only the
       * terminal is a text entry where this chord already means something else. An id rather than
       * a class because the rule is to select on ARIA or `data-*`, and xterm's own markup is not
       * ours to name.
       *
       * ⌘F is left alone deliberately, even though it is `forward-char` in readline: the sidebar
       * owns it, and taking it away from the filter to give it to the shell is a different
       * trade-off that nobody has asked for. The database view is the exception — with a grid on
       * screen ⌘F finds in it, and `DatabaseSurface` explains how it takes the chord.
       */
      if (meta && event.key === 'r') {
        const target = event.target as HTMLElement | null;
        if (!target?.closest('#terminal-dock')) {
          event.preventDefault();
          void workspace.refreshWorktrees();
          if (view.home) void fleet.refresh();
        }
      }

      /*
       * ⌘I / Ctrl-I for the Details dialog — except inside a session pane.
       *
       * The guard is not optional on Linux: **Ctrl-I is literally TAB (0x09)**, so an unguarded
       * binding would eat tab-completion in every shell. That is the same bug commit e2d008d fixed
       * for Ctrl-R, and the same fix — target the region's id rather than `inTextEntry`, because
       * `inTextEntry` is true for xterm's textarea *by design* and also true for the sidebar's
       * filter field, where ⌘I should still work.
       */
      if (meta && event.key === 'i') {
        const target = event.target as HTMLElement | null;
        if (
          !target?.closest('.c-surface') &&
          !document.querySelector('[aria-modal="true"]')
        ) {
          event.preventDefault();
          // Home has no worktree in view, so there is nothing for the dialog to describe.
          if (workspace.selected && !view.home) showInspector = true;
        }
      }

      /*
       * ⌃⌘S to show or hide the sidebar, on macOS only — see `SIDEBAR_SHORTCUT` in `TitleBar`.
       *
       * No terminal guard, unlike ⌘R and ⌘I. A shell sees nothing of a chord that holds ⌘, so
       * there is no binding to steal, and the terminal is the one place a keyboard user is most
       * likely to be when they want the room.
       */
      if (!isLinux && event.metaKey && event.ctrlKey && event.key === 's') {
        event.preventDefault();
        toggleSidebar();
      }

      /*
       * Ctrl-, on Linux only.
       *
       * Gated on the platform rather than accepting either modifier the way ⌘R above does,
       * because on macOS the menu accelerator already fires — handling it here as well would
       * open Settings and then immediately have the menu open it again.
       */
      if (isLinux && event.ctrlKey && event.key === ',' && !inTextEntry(event.target)) {
        event.preventDefault();
        showSettings = true;
      }
    };
    window.addEventListener('keydown', onKey);

    /*
     * The Code tab's chords — ⇧⌘O and the rest in `code-shortcuts.ts` — from any view.
     *
     * Here because each one first switches the main pane to Code, and this file owns which view is
     * showing. Capture phase, and stopped, so neither a focused terminal's xterm nor a CodeMirror
     * keymap sees the chord first. A modal on screen keeps the keyboard; nothing opens over it.
     */
    const onCodeChord = (event: KeyboardEvent) => {
      const chord = chordOf(event);
      if (!chord || !booted || !workspace.selected) return;
      // From Home a chord would open a worktree's Code tab you are not looking at.
      if (view.home) return;
      if (document.querySelector('[aria-modal="true"]')) return;
      event.preventDefault();
      event.stopPropagation();
      view.show('code');
      code.ask(chord, window.getSelection()?.toString().trim().split('\n')[0] ?? '');
    };
    window.addEventListener('keydown', onCodeChord, true);

    return () => {
      gone = true;
      window.removeEventListener('focus', onFocus);
      window.removeEventListener('keydown', onKey);
      window.removeEventListener('keydown', onCodeChord, true);
      void unlistenSettings.then((off) => off());
      void unlistenUpdates.then((off) => off());
      void unlistenHome.then((off) => off());
      void unlistenUsage.then((off) => off());
      void unlistenClicks.then((off) => off());
      offShellRuns?.();
      offFleet?.();
      offWindows?.();
      offSessions?.();
      offAttention?.();
    };
  });

  /*
   * The count of sessions waiting on an answer, on the dock icon.
   *
   * The only indicator that reaches someone when wtm is not the front application *and* notifications
   * are off — and the only one that covers a session in a project other than the selected one, since
   * the sidebar lists only the active project's worktrees.
   *
   * A runtime call rather than a `#[cfg]`, and errors swallowed: a platform with no dock simply does
   * nothing here, and a badge that could not be set is not worth a banner over the approval it was
   * trying to report. `undefined` rather than `0` to clear it — that is the API's own way of saying
   * "no badge", where zero would be a badge reading 0.
   */
  $effect(() => {
    const waiting = sessions.waitingCount + shellRuns.pending.length;
    void getCurrentWindow()
      .setBadgeCount(waiting > 0 ? waiting : undefined)
      .catch(() => {});
  });

  /**
   * The first TypeScript reader of `data-platform`; until now only CSS consulted it.
   *
   * Read once at module scope rather than per keystroke — `index.html` sets it before first
   * paint and nothing changes it afterwards.
   */
  const isLinux = document.documentElement.dataset.platform === 'linux';

  /** Whether a keystroke landed somewhere a character would be typed. */
  function inTextEntry(target: EventTarget | null): boolean {
    const el = target as HTMLElement | null;
    if (!el) return false;
    // xterm renders into a textarea it keeps focused, so this covers the terminal too.
    return (
      el.tagName === 'INPUT' ||
      el.tagName === 'TEXTAREA' ||
      el.tagName === 'SELECT' ||
      el.isContentEditable
    );
  }

  function toggleSidebar(): void {
    sidebarCollapsed = !sidebarCollapsed;
    dragging = false;
    void commands.setPref(SIDEBAR_COLLAPSED_PREF, String(sidebarCollapsed)).catch(() => {});
  }

  /**
   * Add a project.
   *
   * Opens a real dialog. The first version called `window.prompt()`, which a Tauri webview
   * does not implement — it returns `null`, so the button silently did nothing.
   */
  function addProject() {
    addError = null;
    showAddProject = true;
  }
</script>

<div class="c-shell" style:--sidebar-w="{sidebarWidth}px">
  <TitleBar
    {sidebarCollapsed}
    home={view.home}
    ontogglehome={() => view.toggleHome()}
    ontogglesidebar={toggleSidebar}
    onaddproject={addProject}
    onremoveproject={() => (removingProject = workspace.activeProject)}
    onusage={() => (showUsage = true)}
    onsettings={() => (showSettings = true)}
  />

  <div
    class="c-shell__columns"
    class:is-dragging={dragging}
    class:is-sidebar-collapsed={sidebarCollapsed}
  >
    <aside class="c-shell__col" id="worktree-sidebar" hidden={sidebarCollapsed}>
      <!--
        In Home the session tree takes the sidebar's place rather than sitting beside it: one lists a
        project's worktrees, the other every project's, and two trees of the same checkouts side by
        side would be one too many. The sidebar stays mounted underneath, hidden, so its scroll and
        its folds are where they were when you come back.
      -->
      <Sidebar
        hidden={view.home}
        onnew={() => view.show('new')}
        onselectworktree={() => view.show('worktree')}
        detailId={view.current === 'new' ? null : 'worktree-detail'}
      />
      {#if view.homeMounted}
        <FleetTree
          visible={view.home}
          onhome={() => view.peek(null)}
          onopenworktree={(projectId, worktreeId) => openWorktree(projectId, worktreeId)}
          onnewworktree={(projectId) =>
            void workspace.selectProject(projectId).then(() => view.show('new'))}
          onaddproject={addProject}
        />
      {/if}
    </aside>

    <Splitter
      value={sidebarWidth}
      min={MIN_SIDEBAR}
      max={MAX_SIDEBAR}
      label="Resize the sidebar"
      grows="right"
      controls="worktree-sidebar"
      hidden={sidebarCollapsed}
      onresize={(width) => (sidebarWidth = width)}
      oncommit={(width) =>
        void commands.setPref(SIDEBAR_WIDTH_PREF, String(width)).catch(() => {})}
      ondrag={(active) => (dragging = active)}
    />

    <main class="c-shell__col c-shell__col--detail">
      <!--
        Updates. A banner rather than a dialog, because the check runs by itself: a modal that
        appeared at launch, or on coming back to the window, would land on top of whatever the
        user came back to do — the same reasoning as the notification opt-in toast.
      -->
      {#if updates.outcome?.kind === 'updated'}
        <Banner variant="info">
          Updated to {updates.outcome.version}.
          {#snippet action()}
            <Button
              variant="inline"
              onclick={() => updates.clearOutcome()}
              ariaLabel="Dismiss"
            >
              <Icon name="close" size={12} />
            </Button>
          {/snippet}
        </Banner>
      {:else if updates.outcome?.kind === 'failed'}
        <Banner>
          The update to {updates.outcome.version} didn’t finish.
          {#snippet action()}
            <span class="o-row">
              <Button variant="inline" onclick={() => updates.showFailure()}>Details</Button
              >
              <Button
                variant="inline"
                onclick={() => updates.clearOutcome()}
                ariaLabel="Dismiss"
              >
                <Icon name="close" size={12} />
              </Button>
            </span>
          {/snippet}
        </Banner>
      {/if}

      {#if updates.offered && updates.status}
        <Banner variant="info">
          Worktree Manager {updates.status.latest} is available.
          {#snippet action()}
            <span class="o-row">
              <Button variant="inline" onclick={() => updates.open()}>Update…</Button>
              <Button
                variant="inline"
                onclick={() => updates.dismiss()}
                ariaLabel="Dismiss"
              >
                <Icon name="close" size={12} />
              </Button>
            </span>
          {/snippet}
        </Banner>
      {/if}

      {#if addError}
        <Banner>
          {addError}
          {#snippet action()}
            <Button variant="inline" onclick={() => (addError = null)} ariaLabel="Dismiss">
              <Icon name="close" size={12} />
            </Button>
          {/snippet}
        </Banner>
      {/if}

      {#if workspace.error}
        <Banner>
          {workspace.error}
          {#snippet action()}
            <Button variant="inline" onclick={() => workspace.refreshWorktrees()}
              >Retry</Button
            >
          {/snippet}
        </Banner>
      {/if}

      <!--
        A session failure. Dismissed rather than retried: unlike a worktree list, there is nothing
        here to re-run — the turn, the approval or the close either happened or did not.

        This was written in six places in the store and rendered in none, so every one of them was
        silent. A banner is further from the pane that failed than the error deserves, and a note in
        the pane's own header would be better; that is a larger change than making them visible at
        all, which is what was actually missing.
      -->
      {#if sessions.error}
        <Banner>
          {sessions.error}
          {#snippet action()}
            <Button
              variant="inline"
              onclick={() => (sessions.error = null)}
              ariaLabel="Dismiss"
            >
              <Icon name="close" size={12} />
            </Button>
          {/snippet}
        </Banner>
      {/if}

      {#each workspace.brokenProjects as project (project.id)}
        <TrustBanner {project} />
      {/each}

      {#if !booted}
        <div class="c-placeholder"><p>Starting…</p></div>
      {:else if view.home}
        <!-- Home draws itself, below; nothing from the worktree chain shows over it. -->
      {:else if view.current === 'new' && workspace.activeProjectId}
        <NewWorktreePane
          projectId={workspace.activeProjectId}
          onclose={() => view.show(view.previous === 'home' ? 'home' : 'worktree')}
        />
      {:else if workspace.projects.length === 0}
        <div class="c-placeholder">
          <!-- The only screen with room to say what the app is, and the only one a first-run
               user is guaranteed to see. Labelled rather than hidden: the heading beside it
               says "No projects yet", which names the state, not the product. -->
          <Logo size={44} label="Worktree Manager" />
          <h2 class="c-placeholder__title">No projects yet</h2>
          <p class="c-placeholder__prose">
            Add a git repository and its worktrees appear as tabs on the left. Any repo
            works — a project can describe its own New Worktree form in a
            <code>wtm.toml</code>, but nothing is required.
          </p>
          <Button variant="accent" size="lg" onclick={addProject}>Add a repository</Button>
        </div>
      {:else if workspace.selected}
        <Detail
          worktree={workspace.selected}
          projectId={workspace.activeProjectId ?? ''}
          view={view.current === 'database' || view.current === 'code'
            ? view.current
            : 'sessions'}
          {sidebarCollapsed}
          onsessions={() => view.show('worktree')}
          ondatabase={() => view.show('database')}
          oncode={() => view.show('code')}
          onremove={() => {
            if (workspace.activeProjectId && workspace.selected)
              void worktreeRemoval.request({
                projectId: workspace.activeProjectId,
                worktree: workspace.selected,
              });
          }}
          oninspect={() => (showInspector = true)}
          onfavorite={() => {
            const id = workspace.selected?.id;
            if (id) workspace.toggleFavorite(id);
          }}
          onselect={(worktreeId) => {
            // What picking a row in the sidebar does, since this is that list with the rail away.
            workspace.select(worktreeId);
            view.show('worktree');
          }}
          onnew={() => view.show('new')}
        />
      {:else if !workspace.loadingWorktrees}
        <div class="c-placeholder">
          <p>Select a worktree on the left.</p>
        </div>
      {/if}

      <!--
        A sibling of the chain above, and unconditional, both deliberately. `Detail` unmounts
        whenever the chain picks another branch — the create view, or a project switch that lands
        on an empty cached list and leaves `selected` null for a moment — and a session that
        unmounts is a transcript that is gone. Mounted here it survives all of them, and is merely
        hidden while the create pane owns the screen.
      -->
      <SessionSurface visible={booted && view.current === 'worktree'} />
      <DatabaseSurface
        visible={booted && view.current === 'database'}
        onsessions={() => view.show('worktree')}
      />
      <CodeSurface
        visible={booted && view.current === 'code'}
        onsessions={() => view.show('worktree')}
      />
      {#if view.homeMounted}
        <HomeSurface
          visible={booted && view.home}
          onreveal={(pane) => void reveal(pane)}
          onopenworktree={(projectId, worktreeId) => openWorktree(projectId, worktreeId)}
        >
          {#snippet main()}
            <HomeAgent visible={booted && view.home} />
          {/snippet}
        </HomeSurface>
      {/if}
    </main>
  </div>

  {#if showAddProject}
    <AddProjectDialog onclose={() => (showAddProject = false)} />
  {/if}

  {#if removingProject}
    <RemoveProjectDialog
      project={removingProject}
      onclose={() => (removingProject = null)}
    />
  {/if}

  {#if worktreeRemoval.target}
    <RemoveWorktreeDialog
      projectId={worktreeRemoval.target.projectId}
      worktree={worktreeRemoval.target.worktree}
      onclose={() => worktreeRemoval.close()}
    />
  {/if}

  {#if showSettings}
    <SettingsDialog onclose={() => (showSettings = false)} />
  {/if}
  {#if showUsage}
    <UsageDialog onclose={() => (showUsage = false)} />
  {/if}

  <UpdateDialog />

  {#if showInspector && workspace.selected && workspace.activeProjectId}
    <Inspector
      worktree={workspace.selected}
      projectId={workspace.activeProjectId}
      onclose={() => (showInspector = false)}
    />
  {/if}

  <!--
    Last child, and unconditional: the list is a live region, and one added to the document at the same
    moment as its first content is not announced. It sits below the scrim by `$z-toast`, so an open
    dialog dims it and it comes back when the dialog closes — see `settings/_config.scss`.

    `onselectworktree` for the same reason `Sidebar` takes one: clicking a toast selects a worktree, and
    doing that while the create pane owns the screen would look like nothing happened.
  -->
  <Toasts onnavigate={(target) => void goTo(target)} />
</div>

{#if shellRuns.review}
  {#key shellRuns.review.id}<RunShellDialog review={shellRuns.review} />{/key}
{/if}

{#if shellRuns.permissions}
  <HomeShellPermissions
    home={shellRuns.permissions}
    onclose={() => (shellRuns.permissions = null)}
  />
{/if}
