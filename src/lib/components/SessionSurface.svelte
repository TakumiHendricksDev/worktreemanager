<script lang="ts">
  /**
   * Every worktree's split tree, all mounted, all but the active one hidden.
   *
   * # Why this is mounted by the shell and not by the worktree pane
   *
   * A transcript lives in its component — an xterm instance for a shell, a rendered list for a chat —
   * so unmounting one throws away what is on screen. `Detail` is destroyed whenever the main pane
   * switches views, and momentarily whenever a project switch lands on an empty cached list. Living
   * above it, this is only ever *hidden*.
   *
   * That is the reasoning the terminal dock carried before it became one kind of session, and it
   * applies unchanged: hiding is `display: none`, because a hidden pane must cost nothing to lay out
   * — xterm's DOM renderer writes real rows on every chunk — and because a 0×0 `ResizeObserver` fire
   * doubles as the signal that a pane came back. `_surface.scss` keeps the rejected alternatives.
   *
   * # What it does not do
   *
   * It does not open a session for a worktree because you selected one. Browsing six worktrees would
   * otherwise start six CLIs. A session starts when someone asks.
   */
  import { onMount } from 'svelte';

  import { sessions } from '../state/sessions.svelte';
  import { workspace } from '../state/workspace.svelte';
  import SessionSplit from './SessionSplit.svelte';
  import Button from './ui/Button.svelte';

  const {
    visible,
  }: {
    /** False while the create pane owns the screen. Hidden, never unmounted. */
    visible: boolean;
  } = $props();

  const activeId = $derived(workspace.selectedWorktreeId);
  /** Worktrees that have any pane at all, so nothing renders an empty tree. */
  const occupied = $derived([...new Set(sessions.panes.map((p) => p.worktreeId))]);
  const activeLayout = $derived(sessions.layoutFor(activeId));

  /*
   * Drop panes whose worktree has gone.
   *
   * Guarded on the list being authoritative: a cached or still-loading list is missing worktrees that
   * do exist, and ending a session because a refresh has not landed is the worst false positive
   * available. Same guard, same reason, as the dock's.
   */
  $effect(() => {
    const projectId = workspace.activeProjectId;
    if (!projectId || workspace.stale || workspace.loadingWorktrees) return;
    sessions.reconcile(
      projectId,
      workspace.worktrees.map((w) => w.id),
    );
  });

  onMount(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey)) return;
      if (!visible) return;
      // Not behind a modal: a dialog's focus trap would bounce focus straight back out. `[aria-modal]`
      // is the hook because it is the promise the dialog makes, and the rule is to select on ARIA.
      if (document.querySelector('[aria-modal="true"]')) return;

      const project = workspace.activeProjectId;
      const worktree = workspace.selectedWorktreeId;
      if (!project || !worktree) return;

      if (event.key === 'j') {
        // Still "open a shell here", which is what it has always meant — now expressed as adding a
        // `shell` pane rather than toggling a drawer.
        event.preventDefault();
        event.stopPropagation();
        void sessions.openShell(project, worktree);
      }
    };

    /*
     * Capture, and propagation stopped.
     *
     * xterm listens on the textarea it keeps focused, so a bubble-phase handler runs *after* the byte
     * has gone to the shell — `preventDefault` alone would open a pane and type a line feed. Scoped
     * to this one chord so every other key keeps its ordinary route.
     */
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  });
</script>

<div class="c-surface" class:is-hidden={!visible} aria-label="Sessions">
  {#each occupied as worktreeId (worktreeId)}
    {@const layout = sessions.layoutFor(worktreeId)}
    {#if layout}
      <!--
        Every worktree's tree stays mounted; all but the active one are `display: none`. See the
        header — this is the property the whole component exists for.
      -->
      <div class="c-surface__tree" class:is-inactive={worktreeId !== activeId}>
        <SessionSplit {layout} {worktreeId} visible={visible && worktreeId === activeId} />
      </div>
    {/if}
  {/each}

  {#if !activeLayout}
    <div class="c-surface__empty">
      {#if sessions.atCapacity}
        <p>
          As many sessions are open as wtm keeps alive. Close one to start another — it
          refuses rather than ending a session that may be mid-turn.
        </p>
      {:else if workspace.selected && workspace.activeProjectId}
        <p>
          Start a session in <code>{workspace.selected.dirname}</code>. It runs in that
          directory and can read and change the files there.
        </p>
        <div class="o-row">
          {#each sessions.options as option (option.id)}
            <Button
              variant={option.available ? 'accent' : 'neutral'}
              size="sm"
              disabled={!option.available}
              title={option.detail ?? option.blurb}
              onclick={() =>
                void sessions.openAgent(
                  workspace.activeProjectId ?? '',
                  workspace.selected?.id ?? '',
                  option.id,
                )}
            >
              {option.label}
            </Button>
          {/each}
          <Button
            variant="neutral"
            size="sm"
            title="Open a shell in this worktree"
            onclick={() =>
              void sessions.openShell(
                workspace.activeProjectId ?? '',
                workspace.selected?.id ?? '',
              )}
          >
            Shell
          </Button>
        </div>
        {#if sessions.options.every((o) => !o.available)}
          <p class="c-status--warn">
            No agent CLI is on wtm's PATH. Settings → Advanced shows the PATH wtm resolved.
          </p>
        {/if}
      {:else}
        <p>Select a worktree to start a session in it.</p>
      {/if}
    </div>
  {/if}
</div>
