<script lang="ts">
  /**
   * The right pane: the worktree's bar, and nothing else.
   *
   * # What this used to be, and why it shrank
   *
   * It was a header, a two-tab strip, and a body of facts, links and a port table — everything known
   * about a worktree, read. The pane is now a place you *work*, so the sessions own the room and this
   * owns one row above them.
   *
   * Nothing was deleted. The facts, the port table and the environment viewer moved into `Inspector`
   * as the same markup, and the links became a native `<select>` in the bar. What is gone is the tab
   * strip, which was a second selector competing with the sidebar.
   *
   * # Why the sessions are not rendered here
   *
   * They cannot be. This component is destroyed whenever the main pane switches views, and
   * momentarily whenever a project switch lands on an empty cached list — and a destroyed transcript
   * is a lost one. `SessionSurface` is mounted by the shell as an unconditional sibling for exactly
   * that reason, which is the same reason `TerminalDock` was.
   */
  import type { Worktree } from '../ipc/types';
  import WorktreeBar from './WorktreeBar.svelte';

  const {
    worktree,
    projectId,
    onremove,
    onfavorite,
    oninspect,
  }: {
    worktree: Worktree;
    projectId: string;
    onremove: () => void;
    onfavorite: () => void;
    oninspect: () => void;
  } = $props();
</script>

<div
  class="c-detail"
  id="worktree-detail"
  role="tabpanel"
  aria-labelledby={`tab-${worktree.id}`}
  tabindex="-1"
>
  <WorktreeBar {worktree} {projectId} {onremove} {onfavorite} {oninspect} />
</div>
