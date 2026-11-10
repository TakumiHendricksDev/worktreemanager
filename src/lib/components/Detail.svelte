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
  import type { WorktreeView } from '../worktree-view';
  import WorktreeBar from './WorktreeBar.svelte';

  const {
    worktree,
    projectId,
    view,
    sidebarCollapsed,
    onsessions,
    ondatabase,
    oncode,
    onremove,
    onfavorite,
    oninspect,
    onselect,
    onnew,
  }: {
    worktree: Worktree;
    projectId: string;
    view: WorktreeView;
    sidebarCollapsed: boolean;
    onsessions: () => void;
    ondatabase: () => void;
    oncode: () => void;
    onremove: () => void;
    onfavorite: () => void;
    oninspect: () => void;
    onselect: (worktreeId: string) => void;
    onnew: () => void;
  } = $props();
</script>

<!--
  A region named by its row in the sidebar tree. It was a `tabpanel` while the sidebar was a
  tablist; a tree has no panel role, and `region` is what a landmark labelled by the item that
  controls it is.
-->
<div
  class="c-detail"
  id="worktree-detail"
  role="region"
  aria-labelledby={`tab-${worktree.id}`}
  tabindex="-1"
>
  <WorktreeBar
    {worktree}
    {projectId}
    {view}
    {sidebarCollapsed}
    {onsessions}
    {ondatabase}
    {oncode}
    {onremove}
    {onfavorite}
    {oninspect}
    {onselect}
    {onnew}
  />
</div>
