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
  import { onMount } from 'svelte';

  import AddProjectDialog from './lib/components/AddProjectDialog.svelte';
  import Detail from './lib/components/Detail.svelte';
  import NewWorktreePane from './lib/components/NewWorktreePane.svelte';
  import RemoveWorktreeDialog from './lib/components/RemoveWorktreeDialog.svelte';
  import SettingsDialog from './lib/components/SettingsDialog.svelte';
  import Sidebar from './lib/components/Sidebar.svelte';
  import TerminalDock from './lib/components/TerminalDock.svelte';
  import TitleBar from './lib/components/TitleBar.svelte';
  import TrustBanner from './lib/components/TrustBanner.svelte';
  import Banner from './lib/components/ui/Banner.svelte';
  import Button from './lib/components/ui/Button.svelte';
  import Icon from './lib/components/ui/Icon.svelte';
  import Logo from './lib/components/ui/Logo.svelte';
  import { commands } from './lib/ipc/commands';
  import { errorMessage } from './lib/ipc/types';
  import { agents } from './lib/state/agents.svelte';
  import { terminals } from './lib/state/terminals.svelte';
  import { theme } from './lib/state/theme.svelte';
  import { workspace } from './lib/state/workspace.svelte';

  const MIN_SIDEBAR = 200;
  const MAX_SIDEBAR = 460;
  const DEFAULT_SIDEBAR = 276;

  let sidebarWidth = $state(DEFAULT_SIDEBAR);
  let dragging = $state(false);
  let booted = $state(false);
  let addError = $state<string | null>(null);
  /**
   * What the main pane shows.
   *
   * New Worktree is a *view*, not a modal: the form, the review screen and a live setup
   * terminal need the room, and a modal implies a quick decision when a setup run can take
   * minutes. Removal stays a modal — a destructive confirmation should block.
   */
  let mainView = $state<'worktree' | 'new'>('worktree');
  let showAddProject = $state(false);
  let showRemove = $state(false);
  let showSettings = $state(false);
  /**
   * Teardown for the agent event listeners.
   *
   * Not `$state`: nothing renders it, and making it reactive would put a mount-time write inside
   * whatever effect happened to read it.
   */
  let offAgents: (() => void) | null = null;

  onMount(() => {
    void (async () => {
      await theme.init();

      const stored = await commands.getPref('ui.sidebar_width').catch(() => null);
      const parsed = stored ? Number.parseInt(stored, 10) : NaN;
      if (Number.isFinite(parsed)) {
        sidebarWidth = Math.min(Math.max(parsed, MIN_SIDEBAR), MAX_SIDEBAR);
      }

      // Not awaited: the dock starts closed, so neither its remembered height nor the shells it
      // is adopting are needed for the first paint, and a slow config read must not hold up the
      // worktree list.
      void terminals.init();

      // Same reasoning, plus one of its own: this subscribes to the three `agent:*` event streams,
      // and a session started before the listeners attach would stream into nothing. `init` is
      // called before `workspace.init` for exactly that ordering — nothing can start a session
      // until there is a worktree list to start one from.
      void agents.init().then((off) => {
        offAgents = off;
      });

      await workspace.init();
      booted = true;
    })();

    const onFocus = () => {
      if (booted) void workspace.refreshWorktrees();
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
       * trade-off that nobody has asked for.
       */
      if (meta && event.key === 'r') {
        const target = event.target as HTMLElement | null;
        if (!target?.closest('#terminal-dock')) {
          event.preventDefault();
          void workspace.refreshWorktrees();
        }
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

    return () => {
      window.removeEventListener('focus', onFocus);
      window.removeEventListener('keydown', onKey);
      void unlistenSettings.then((off) => off());
      offAgents?.();
    };
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

  function startDrag(event: PointerEvent) {
    dragging = true;
    const startX = event.clientX;
    const startWidth = sidebarWidth;
    // Pointer capture, so a fast drag that leaves the splitter keeps working.
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);

    const onMove = (move: PointerEvent) => {
      sidebarWidth = Math.min(
        Math.max(startWidth + (move.clientX - startX), MIN_SIDEBAR),
        MAX_SIDEBAR,
      );
    };

    const onUp = () => {
      dragging = false;
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      // Persist only on release; saving on every move would write hundreds of times.
      void commands
        .setPref('ui.sidebar_width', String(Math.round(sidebarWidth)))
        .catch(() => {});
    };

    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  function onSplitterKey(event: KeyboardEvent) {
    const step = event.shiftKey ? 32 : 8;
    const deltas: Record<string, number> = { ArrowLeft: -step, ArrowRight: step };
    const delta = deltas[event.key];
    if (delta === undefined) return;
    event.preventDefault();
    sidebarWidth = Math.min(Math.max(sidebarWidth + delta, MIN_SIDEBAR), MAX_SIDEBAR);
    void commands
      .setPref('ui.sidebar_width', String(Math.round(sidebarWidth)))
      .catch(() => {});
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
  <TitleBar onaddproject={addProject} onsettings={() => (showSettings = true)} />

  <div class="c-shell__columns" class:is-dragging={dragging}>
    <aside class="c-shell__col">
      <Sidebar
        onnew={() => (mainView = 'new')}
        onselectworktree={() => (mainView = 'worktree')}
      />
    </aside>

    <!--
      A resize handle is a real widget, not decoration: `role="separator"` with
      aria-value* and a tabindex is the ARIA window-splitter pattern, and the keydown
      handler is what makes the sidebar resizable without a mouse. Svelte's rule assumes
      a separator is decorative, which a *focusable* one is not.
    -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      class="c-shell__splitter"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize the sidebar"
      aria-valuenow={Math.round(sidebarWidth)}
      aria-valuemin={MIN_SIDEBAR}
      aria-valuemax={MAX_SIDEBAR}
      tabindex="0"
      onpointerdown={startDrag}
      onkeydown={onSplitterKey}
    ></div>

    <main class="c-shell__col c-shell__col--detail">
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

      {#each workspace.brokenProjects as project (project.id)}
        <TrustBanner {project} />
      {/each}

      {#if !booted}
        <div class="c-placeholder"><p>Starting…</p></div>
      {:else if mainView === 'new' && workspace.activeProjectId}
        <NewWorktreePane
          projectId={workspace.activeProjectId}
          onclose={() => (mainView = 'worktree')}
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
          onremove={() => (showRemove = true)}
          onfavorite={() => {
            const id = workspace.selected?.id;
            if (id) void workspace.toggleFavorite(id);
          }}
        />
      {:else if !workspace.loadingWorktrees}
        <div class="c-placeholder">
          <p>Select a worktree on the left.</p>
        </div>
      {/if}

      <!--
        A sibling of the chain above, and unconditional, both deliberately. `Detail` unmounts
        whenever the chain picks another branch — the create view, or a project switch that lands
        on an empty cached list and leaves `selected` null for a moment — and a terminal that
        unmounts is a transcript that is gone. Mounted here it survives all of them, and is
        merely hidden while the create pane owns the screen.
      -->
      <TerminalDock visible={booted && mainView === 'worktree'} />
    </main>
  </div>

  {#if showAddProject}
    <AddProjectDialog onclose={() => (showAddProject = false)} />
  {/if}

  {#if showRemove && workspace.selected && workspace.activeProjectId}
    <RemoveWorktreeDialog
      projectId={workspace.activeProjectId}
      worktree={workspace.selected}
      onclose={() => (showRemove = false)}
    />
  {/if}

  {#if showSettings}
    <SettingsDialog onclose={() => (showSettings = false)} />
  {/if}
</div>
