<script lang="ts">
  /**
   * A drawer along the bottom of the detail pane, with one long-lived shell per worktree.
   *
   * # Why every pane stays mounted
   *
   * A terminal's transcript lives in its xterm instance, so unmounting one throws it away. The
   * shells are the point of the feature and switching worktrees is the most common thing anyone
   * does here, so all of them stay mounted and all but the active one get `display: none`.
   * `Terminal` carries the guard that makes that safe — a hidden pane's observer fires at 0×0
   * and the fit addon floors its answer at 2×1 rather than declining, so an unguarded fit would
   * tell a live shell its window is two columns wide.
   *
   * Which is also why this component is mounted by `App.svelte` rather than by `Detail`:
   * `Detail` unmounts whenever the main pane switches views, and momentarily whenever a project
   * switch lands on an empty cached list and leaves the selection null. Living above it, the
   * dock is only ever *hidden*.
   *
   * # What it does not do
   *
   * It does not open a shell for a worktree just because you selected it. Browsing six
   * worktrees with the dock open would fork six login shells and run six sets of shell rc
   * files. A shell starts when someone asks for one, and until then the pane says so.
   */
  import { onMount } from 'svelte';

  import {
    MAX_HEIGHT,
    MIN_HEIGHT,
    SHORTCUT_LABEL,
    terminals,
  } from '../state/terminals.svelte';
  import { workspace } from '../state/workspace.svelte';
  import Terminal from './Terminal.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    visible,
  }: {
    /** False while the create pane owns the screen. Hidden, never unmounted. */
    visible: boolean;
  } = $props();

  /** What `bind:this` on a pane hands back — its `focus`, plus Svelte's own members. */
  type Pane = ReturnType<typeof Terminal>;

  /**
   * The panes' focus handles.
   *
   * Deliberately not `$state`: nothing renders these, and making them reactive would make the
   * focus effect below depend on every mount — so a restart, which remounts, would pull focus
   * out of wherever the user actually is.
   */
  const handles: Record<string, Pane | null> = {};

  let dockEl = $state<HTMLElement | null>(null);
  let dragging = $state(false);
  /** Whether focus was inside the dock when it closed. Not `$state`; see `handles`. */
  let hadFocus = false;
  let restoreTo: HTMLElement | null = null;

  const activeId = $derived(workspace.selectedWorktreeId);
  const active = $derived(terminals.paneFor(activeId));
  const others = $derived(terminals.live.filter((p) => p.worktreeId !== activeId).length);

  /**
   * The key a pane renders and its handle is stored under.
   *
   * The generation is in it so a restart remounts rather than continuing a dead shell's
   * transcript under a live prompt. The handles are keyed the same way because with a keyed
   * `{#each}` Svelte is free to create the replacement before destroying its predecessor, and a
   * shared key would leave the predecessor's teardown nulling the new pane's handle.
   */
  function keyOf(pane: { worktreeId: string; generation: number }): string {
    return `${pane.worktreeId}:${pane.generation}`;
  }

  /*
   * Move focus into the terminal when someone asks for it, and at no other time.
   *
   * Tracking `focusEpoch` alone is the whole design. An effect that also tracked the selection
   * would fire on every arrow key in the sidebar, because switching worktrees with the dock open
   * changes which pane is active — and focus would land in a terminal the user was navigating
   * past. `focusTarget` is a plain field for the same reason.
   */
  $effect(() => {
    if (terminals.focusEpoch === 0) return;
    const pane = terminals.paneFor(terminals.focusTarget);
    if (!pane) return;
    restoreTo = document.activeElement as HTMLElement | null;
    handles[keyOf(pane)]?.focus();
  });

  /*
   * Closing hands focus back to whatever opened the dock — the contract `Dialog` keeps, for the
   * same reason. Only when focus was inside, so dismissing the dock from the sidebar leaves the
   * sidebar alone, and `isConnected` because the trigger may be gone: removing a worktree
   * unmounts the header its button lives in.
   *
   * `hadFocus` is tracked by a `focusin` listener rather than read off `document.activeElement`
   * here, because by the time this runs the pane has left the render tree and taken focus with
   * it — the answer would always be `<body>`.
   */
  $effect(() => {
    if (terminals.open) return;
    if (hadFocus && restoreTo?.isConnected) restoreTo.focus();
    hadFocus = false;
    restoreTo = null;
  });

  /*
   * Drop panes whose worktree has gone.
   *
   * Removal through the app already ends the shell in Rust, before teardown runs. This is for
   * `git worktree remove` in a real terminal, which wtm notices on the next window focus. See
   * `terminals.reconcile` for why a failed removal is left alone.
   *
   * Guarded on the list being authoritative: a cached or still-loading list is missing worktrees
   * that do exist, and killing a shell because a refresh has not landed yet is the worst false
   * positive available.
   */
  $effect(() => {
    const projectId = workspace.activeProjectId;
    if (!projectId || workspace.stale || workspace.loadingWorktrees) return;
    terminals.reconcile(
      projectId,
      workspace.worktrees.map((w) => w.id),
    );
  });

  onMount(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.key !== 'j') return;
      // Nothing to toggle while the create pane owns the screen — and a pane created inside a
      // hidden dock measures its character cell at zero and never recovers.
      if (!visible) return;
      // Not behind a modal: the dock would toggle under the scrim and the dialog's focus trap
      // would bounce focus straight back out of it. `[aria-modal]` is the hook because it is the
      // promise the dialog makes, and the rule is to select on ARIA, never on a class.
      if (document.querySelector('[aria-modal="true"]')) return;

      event.preventDefault();
      event.stopPropagation();
      void terminals.toggle(workspace.activeProjectId, activeId);
    };

    /*
     * Capture, and propagation stopped.
     *
     * xterm listens on the textarea it keeps focused, so a bubble-phase window handler runs
     * *after* the byte has already gone to the shell — `preventDefault` alone would toggle the
     * dock and type a line feed. Scoped to this one chord, so ⌘R here and ⌘F in the sidebar keep
     * their ordinary route.
     *
     * Deliberately not gated on `App.svelte`'s `inTextEntry`, whose own comment notes it returns
     * true for xterm's textarea: dismissing the dock while typing in the shell is the most
     * important direction this has to work in.
     *
     * ⌘J, or Ctrl-J on Linux. Every Ctrl-letter means something to a line editor, so one has to
     * be taken; Ctrl-J's claim is the weakest, because it sends LF and the Enter key that
     * duplicates it is unaffected. Ctrl-` was the first choice and lost twice over: it is a dead
     * key on several European layouts, and AppKit claims ⌘` for cycling windows in any app with
     * a Window menu, which `src-tauri/src/lib.rs` builds.
     */
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  });

  /*
   * Drag to resize, inverted: the dock grows upward, so a pointer moving up — a falling
   * `clientY` — has to *increase* the height. Adapted from the sidebar splitter in `App.svelte`
   * rather than shared with it; `_dock.scss` explains why the same call was made for the CSS.
   *
   * `preventDefault` on the pointerdown, which the sidebar does not do: it suppresses the
   * compatibility mousedown and with it the browser's selection, so a drag crossing the detail
   * pane cannot highlight everything it passes over. The cost is that clicking the handle no
   * longer focuses it, which a separator does not need — Tab reaches it.
   */
  function startDrag(event: PointerEvent) {
    event.preventDefault();
    dragging = true;
    const startY = event.clientY;
    const startHeight = terminals.height;
    // Pointer capture, so a fast drag that leaves the splitter keeps working.
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);

    const onMove = (move: PointerEvent) => {
      terminals.setHeight(startHeight - (move.clientY - startY));
    };

    const onUp = () => {
      dragging = false;
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      // Persist only on release; saving on every move would write hundreds of times.
      terminals.persistHeight();
    };

    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  function onSplitterKey(event: KeyboardEvent) {
    const step = event.shiftKey ? 32 : 8;
    // Up grows the dock, which is the opposite mapping to the sidebar's — the same inversion as
    // the drag, and the sign error someone will otherwise put back.
    const deltas: Record<string, number> = { ArrowUp: step, ArrowDown: -step };
    const delta = deltas[event.key];
    if (delta === undefined) return;
    event.preventDefault();
    terminals.setHeight(terminals.height + delta);
    terminals.persistHeight();
  }
</script>

<svelte:window
  onfocusin={(event) => {
    if (dockEl?.contains(event.target as Node)) hadFocus = true;
  }}
/>

<section
  bind:this={dockEl}
  id="terminal-dock"
  class="c-dock"
  class:is-hidden={!visible || !terminals.open}
  class:is-dragging={dragging}
  style:--dock-h="{terminals.height}px"
  aria-label="Terminal"
>
  <!--
    A resize handle is a real widget, not decoration: `role="separator"` with aria-value* and a
    tabindex is the ARIA window-splitter pattern, and the keydown handler is what makes the dock
    resizable without a mouse. Svelte's rule assumes a separator is decorative, which a
    *focusable* one is not. Same two exemptions as the sidebar's splitter, for the same reason.
  -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    class="c-dock__splitter"
    role="separator"
    aria-orientation="horizontal"
    aria-label="Resize the terminal"
    aria-valuenow={terminals.height}
    aria-valuemin={MIN_HEIGHT}
    aria-valuemax={MAX_HEIGHT}
    tabindex="0"
    onpointerdown={startDrag}
    onkeydown={onSplitterKey}
  ></div>

  <!--
    The header says what the shell is doing and nothing the pane above already says. The
    worktree's name is in the detail header two inches up, and repeating it is how the project
    name came to appear twice before the switcher moved into the title bar.
  -->
  <header class="c-dock__head">
    <h2 class="c-dock__title">Terminal</h2>

    {#if active?.error}
      <p class="c-dock__note c-status--danger">{active.error}</p>
    {:else if active?.ended}
      <p class="c-dock__note c-status--warn">{active.ended}</p>
    {:else if terminals.error}
      <p class="c-dock__note c-status--danger">{terminals.error}</p>
    {:else if others > 0}
      <!-- How you learn that the shells you are not looking at are still there. A tab strip
           would be a second selector competing with the sidebar, which is already one. -->
      <p class="c-dock__note c-status--subtle">
        {others} other shell{others === 1 ? '' : 's'} running
      </p>
    {/if}

    <div class="c-dock__actions">
      {#if active && (active.ended || active.error)}
        <Button
          variant="neutral"
          size="sm"
          onclick={() =>
            void terminals.restart(workspace.activeProjectId ?? '', active.worktreeId)}
        >
          Restart
        </Button>
      {:else if active}
        <Button
          variant="quiet"
          size="sm"
          title="End the shell and keep the transcript"
          onclick={() => void terminals.kill(active.worktreeId)}
        >
          Kill
        </Button>
      {/if}
      <Button
        variant="quiet"
        icon="sm"
        onclick={() => terminals.hide()}
        title="Hide the terminal ({SHORTCUT_LABEL})"
        ariaLabel="Hide the terminal"
      >
        <Icon name="close" size={12} />
      </Button>
    </div>
  </header>

  {#each terminals.panes as pane (keyOf(pane))}
    <div class="c-dock__pane" class:is-inactive={pane.worktreeId !== activeId}>
      <Terminal
        bind:this={handles[keyOf(pane)]}
        session={pane.session}
        active={visible && terminals.open && pane.worktreeId === activeId}
        onexit={(exit) => terminals.noteExit(pane.worktreeId, exit)}
      />
    </div>
  {/each}

  {#if !active}
    <div class="c-dock__empty">
      {#if terminals.atCapacity}
        <p>
          Six shells are already running, which is as many as wtm keeps alive. Close one to
          start another.
        </p>
      {:else if workspace.selected && workspace.activeProjectId}
        <p>No shell here yet. It starts in this worktree's directory and stays alive.</p>
        <Button
          variant="accent"
          size="sm"
          onclick={() =>
            void terminals.show(workspace.activeProjectId, workspace.selected?.id ?? null)}
        >
          Start a shell
        </Button>
      {:else}
        <p>Select a worktree to open a shell in it.</p>
      {/if}
    </div>
  {/if}
</section>
