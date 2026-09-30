<script lang="ts">
  /**
   * The Code tab: a worktree's files, read-only, for reviewing what an agent did.
   *
   * Always mounted and hidden when another view is showing, like `DatabaseSurface`, so a worktree's
   * open folders and files survive a trip to Sessions and back. It follows the selected worktree;
   * each worktree keeps its own state in the `code` store.
   */
  import { onMount, untrack } from 'svelte';

  import { allAdded } from '../code-diff';
  import type { CommentActions } from '../code-comment-marks';
  import { askMessage, isOutdated, reviewMessage } from '../code-review';
  import type { CodeComment } from '../ipc/types';
  import { code, type Popup } from '../state/code.svelte';
  import { sessions } from '../state/sessions.svelte';
  import { workspace } from '../state/workspace.svelte';
  import CodeTree from './CodeTree.svelte';
  import CodeReview from './CodeReview.svelte';
  import CodeViewer from './CodeViewer.svelte';
  import FindInFiles from './FindInFiles.svelte';
  import GoTo from './GoTo.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';
  import { FIND_IN_FILES_SHORTCUT, GO_TO_FILE_SHORTCUT } from '../code-shortcuts';

  const {
    visible,
    onsessions,
  }: {
    /** Hidden rather than unmounted, so open folders and files outlive a switch of view. */
    visible: boolean;
    /** Show the worktree's panes — where comments sent to an agent land. */
    onsessions: () => void;
  } = $props();

  const projectId = $derived(workspace.activeProjectId);
  /** Only a worktree the current listing has: a cached id mid-switch would answer "unknown". */
  const worktree = $derived(workspace.stale ? null : workspace.selected);
  const worktreeId = $derived(worktree?.id ?? null);
  const tree = $derived(worktreeId ? code.treeOf(worktreeId) : null);
  const error = $derived(worktreeId ? (code.errors[worktreeId] ?? null) : null);
  const tabs = $derived(worktreeId ? (code.tabs[worktreeId] ?? []) : []);
  const active = $derived(worktreeId ? (code.active[worktreeId] ?? null) : null);
  const file = $derived(worktreeId && active ? code.fileOf(worktreeId, active) : null);
  const view = $derived(worktreeId ? (code.view[worktreeId] ?? 'project') : 'project');
  const scope = $derived(worktreeId ? (code.scope[worktreeId] ?? 'branch') : 'branch');
  const changes = $derived(worktreeId ? (code.changes[worktreeId]?.answer ?? null) : null);
  const change = $derived(worktreeId && active ? code.changeOf(worktreeId, active) : null);
  /** The open file's text, whichever way it arrived. */
  const text = $derived(
    file?.status === 'ready' ? file.file.text : file?.status === 'base' ? file.text : null,
  );
  /*
   * The gutter's hunks. A new or untracked file is all added, which needs no diff; an edited or
   * renamed one asks git; anything else has no markers.
   */
  const hunks = $derived.by(() => {
    if (!worktreeId || !active || !change || text === null) return null;
    if (change.kind === 'added' || change.kind === 'untracked') {
      return allAdded(
        text.endsWith('\n') ? text.split('\n').length - 1 : text.split('\n').length,
      );
    }
    return code.hunksOf(worktreeId, active);
  });
  const comments = $derived(worktreeId ? code.commentsIn(worktreeId) : []);
  const fileComments = $derived(active ? comments.filter((c) => c.path === active) : []);
  const composing = $derived(
    code.composing &&
      code.composing.worktreeId === worktreeId &&
      code.composing.path === active
      ? { start: code.composing.start, end: code.composing.end }
      : null,
  );
  /** The agent Ask drafts into, named: the last one focused in this worktree. */
  const askTarget = $derived(worktreeId ? sessions.draftDestination(worktreeId) : null);
  const askLabel = $derived.by(() => {
    const pane = askTarget ? sessions.paneById(askTarget.id) : null;
    return pane ? (pane.agentTitle ?? sessions.labelOf(pane)) : null;
  });

  /** A comment's lines no longer read as they did — when its file is open to tell. */
  function outdated(comment: CodeComment): boolean {
    if (!worktreeId) return false;
    const state = code.fileOf(worktreeId, comment.path);
    return state?.status === 'ready' && state.file.text !== null
      ? isOutdated(comment, state.file.text)
      : false;
  }

  /** Put a message in an agent's composer, and show it unless it is in a window of its own. */
  function draft(paneId: string, message: string): void {
    sessions.insertDraft(paneId, message);
    if (!sessions.isOut(paneId)) onsessions();
  }

  const actions: CommentActions = {
    compose: (start, end) => {
      if (worktreeId && active) code.compose(worktreeId, active, start, end);
    },
    save: async (message, excerpt) => {
      if (worktreeId) await code.addComment(worktreeId, message, excerpt);
    },
    cancel: () => (code.composing = null),
    update: async (id, message) => {
      if (worktreeId) await code.updateComment(worktreeId, id, message);
    },
    resolve: (id, resolved) => {
      if (worktreeId) void code.resolveComment(worktreeId, id, resolved);
    },
    remove: (id) => {
      if (worktreeId) void code.removeComment(worktreeId, id);
    },
    ask: (start, end, excerpt) => {
      if (askTarget && active) draft(askTarget.id, askMessage(active, start, end, excerpt));
    },
  };

  function sendComments(paneId: string, label: string, list: readonly CodeComment[]): void {
    if (!worktreeId || list.length === 0) return;
    draft(paneId, reviewMessage(list, outdated));
    void code.markSent(
      worktreeId,
      list.map((c) => c.id),
      label,
    );
  }

  function copyComments(list: readonly CodeComment[]): void {
    void navigator.clipboard.writeText(reviewMessage(list, outdated)).catch(() => {});
  }

  function jumpTo(comment: CodeComment): void {
    if (projectId && worktreeId) {
      code.open(projectId, worktreeId, comment.path, { line: comment.start });
    }
  }

  /** Which change the stepper last landed on, for "2 of 5". Reset when the file changes. */
  let stepAt = $state<number | null>(null);

  /** The popup on screen, from `code.popup`, and the last request already opened. */
  let popup = $state<Popup | null>(null);
  let poppedFor = 0;

  let treeView = $state<ReturnType<typeof CodeTree> | null>(null);
  let viewer = $state<ReturnType<typeof CodeViewer> | null>(null);

  function refresh(): void {
    if (projectId && worktreeId) void code.refresh(projectId, worktreeId);
  }

  function open(path: string): void {
    if (projectId && worktreeId) code.open(projectId, worktreeId, path);
  }

  async function locate(): Promise<void> {
    if (!projectId || !worktreeId || !active) return;
    await code.locate(projectId, worktreeId, active);
    await treeView?.reveal(active);
  }

  function nameOf(path: string): string {
    return path.slice(path.lastIndexOf('/') + 1);
  }

  /** Two tabs with the same file name say which folder each is in, as an IDE's tabs do. */
  function tabLabel(path: string): { name: string; hint: string | null } {
    const name = nameOf(path);
    const twin = tabs.some((other) => other !== path && nameOf(other) === name);
    if (!twin) return { name, hint: null };
    const parts = path.split('/');
    return { name, hint: parts.length > 1 ? (parts[parts.length - 2] ?? null) : null };
  }

  function kib(bytes: number): string {
    return bytes < 1024 ? `${bytes} B` : `${Math.round(bytes / 1024).toLocaleString()} KiB`;
  }

  // A changed file's hunks, for the gutter, once per answer from git.
  $effect(() => {
    if (!visible || !projectId || !worktreeId || !active || !change) return;
    untrack(() => void code.loadHunks(projectId, worktreeId, active));
  });

  // A deleted file, opened from Changes, shows what it was rather than a dead end.
  $effect(() => {
    if (!projectId || !worktreeId || !active || file?.status !== 'gone') return;
    if (change?.kind !== 'deleted') return;
    untrack(() => void code.openBase(projectId, worktreeId, active));
  });

  $effect(() => {
    void active;
    stepAt = null;
  });

  function stepChange(direction: 1 | -1): void {
    stepAt = viewer?.step(direction)?.index ?? null;
  }

  // A shortcut asked for a popup. `App` has already switched to this view.
  $effect(() => {
    const request = code.popup;
    if (!request || request.id === poppedFor || !visible || !worktreeId) return;
    poppedFor = request.id;
    popup = request;
  });

  // Re-read whenever the tab is looked at, including arriving on another worktree while it shows.
  $effect(() => {
    if (!visible || !projectId || !worktreeId) return;
    void code.refresh(projectId, worktreeId);
  });

  // An agent here finished a turn: its edits have landed, so look again — only while showing, since
  // becoming visible refreshes anyway.
  $effect(() => {
    const epoch = worktreeId ? sessions.turnEpoch[worktreeId] : undefined;
    if (epoch === undefined) return;
    untrack(() => {
      if (visible) refresh();
    });
  });

  // Forget worktrees that are gone. Same guard, and the same reason, as `sessions.reconcile`.
  $effect(() => {
    const project = workspace.activeProjectId;
    if (!project || workspace.stale || workspace.loadingWorktrees) return;
    const ids = workspace.worktrees.map((w) => w.id);
    untrack(() => code.reconcile(project, ids));
  });

  // Reading a file that is not open yet — after a relaunch, the tab that was active.
  $effect(() => {
    if (!visible || !projectId || !worktreeId || !active) return;
    const current = code.fileOf(worktreeId, active);
    if (!current) untrack(() => void code.ensure(projectId, worktreeId, active));
  });

  onMount(() => code.listenForComments());

  $effect(() => {
    if (worktreeId) void code.loadComments(worktreeId);
  });

  onMount(() => {
    const onFocus = () => {
      if (visible) refresh();
    };
    window.addEventListener('focus', onFocus);
    return () => window.removeEventListener('focus', onFocus);
  });

  /*
   * ⌘F finds in the open file while this view is showing — from the tree, too.
   *
   * The sidebar has owned ⌘F everywhere, focusing its worktree filter. With a file on screen, find
   * means the file. Capture phase so this runs before the sidebar's listener; the editor's own
   * keymap already handles ⌘F when the editor has focus, and the sidebar keeps its own.
   */
  onMount(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!visible || !(event.metaKey || event.ctrlKey) || event.key.toLowerCase() !== 'f')
        return;
      if (event.shiftKey || event.altKey) return;
      const target = event.target as Element | null;
      if (target?.closest?.('.cm-editor, .c-sidebar')) return;
      if (document.querySelector('[aria-modal="true"]')) return;
      if (!viewer || !active) return;
      event.preventDefault();
      event.stopPropagation();
      viewer.find();
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  });
</script>

<section class="c-code-view" class:is-hidden={!visible} id="code-view" aria-label="Code">
  {#if projectId && worktreeId}
    <div class="c-code-view__body">
      <aside class="c-code-view__explorer" aria-label="Project files">
        <header class="c-code-view__explorer-head">
          <div class="c-code-view__views" role="group" aria-label="Tree">
            <Button
              variant={view === 'project' ? 'neutral' : 'quiet'}
              size="sm"
              ariaPressed={view === 'project'}
              onclick={() => code.setView(worktreeId, 'project')}>Project</Button
            >
            <Button
              variant={view === 'changes' ? 'neutral' : 'quiet'}
              size="sm"
              ariaPressed={view === 'changes'}
              title="Only the files this worktree changed"
              onclick={() => code.setView(worktreeId, 'changes')}
              >Changes{#if changes && changes.changes.length > 0}<span
                  class="c-code-view__count">{changes.changes.length}</span
                >{/if}</Button
            >
          </div>
          <span class="c-code-view__spacer"></span>
          <Button
            variant="quiet"
            size="sm"
            icon="sm"
            title={`Find in files (${FIND_IN_FILES_SHORTCUT})`}
            ariaLabel="Find in files"
            onclick={() => code.ask('find')}><Icon name="search" size={14} /></Button
          >
          <Button
            variant="quiet"
            size="sm"
            icon="sm"
            title={`Go to file (${GO_TO_FILE_SHORTCUT})`}
            ariaLabel="Go to file"
            onclick={() => code.ask('file')}><Icon name="file" size={14} /></Button
          >
          <Button
            variant="quiet"
            size="sm"
            icon="sm"
            title="Select the open file in the tree"
            ariaLabel="Select the open file in the tree"
            disabled={!active}
            onclick={locate}><Icon name="locate" size={14} /></Button
          >
          <Button
            variant="quiet"
            size="sm"
            icon="sm"
            title="Collapse all folders"
            ariaLabel="Collapse all folders"
            onclick={() => worktreeId && code.collapseAll(worktreeId)}
            ><Icon name="collapse" size={14} /></Button
          >
          <Button
            variant="quiet"
            size="sm"
            icon="sm"
            title="Read the worktree again"
            ariaLabel="Refresh"
            onclick={refresh}><Icon name="restart" size={14} /></Button
          >
        </header>
        {#if view === 'changes'}
          <div class="c-code-view__scope">
            <span class="c-code-view__against">
              {#if changes?.scope === 'branch'}
                Since <code>{changes.against}</code>
              {:else}
                Not yet committed
              {/if}
            </span>
            <Button
              variant="link"
              size="sm"
              title={scope === 'branch'
                ? 'Show only what is not committed yet'
                : 'Show everything this branch changed since its base'}
              onclick={() =>
                projectId &&
                code.setScope(
                  projectId,
                  worktreeId,
                  scope === 'branch' ? 'uncommitted' : 'branch',
                )}>{scope === 'branch' ? 'Uncommitted only' : 'Whole branch'}</Button
            >
          </div>
          {#if scope === 'branch' && changes?.scope === 'uncommitted'}
            <p class="c-code-view__notice">
              There is no base branch to compare with, so this is what is not committed yet.
            </p>
          {/if}
        {/if}
        {#if error}
          <p class="c-code-view__notice c-code-view__notice--error">{error}</p>
        {/if}
        {#if tree?.truncated}
          <p class="c-code-view__notice">
            This worktree lists more files than wtm reads at once, so some are missing here.
          </p>
        {/if}
        <CodeTree bind:this={treeView} {projectId} {worktreeId} onopen={open} />
      </aside>

      <div class="c-code-view__main">
        {#if tabs.length > 0}
          <div class="c-code-view__tabs" role="tablist" aria-label="Open files">
            {#each tabs as tab (tab)}
              {@const label = tabLabel(tab)}
              <div class="c-code-view__tab" class:is-active={tab === active}>
                <button
                  type="button"
                  role="tab"
                  class="c-code-view__tab-name"
                  aria-selected={tab === active}
                  title={tab}
                  onclick={() => projectId && code.activate(projectId, worktreeId, tab)}
                  onauxclick={(event) => {
                    if (event.button === 1) code.close(worktreeId, tab);
                  }}
                >
                  {label.name}
                  {#if label.hint}<span class="c-code-view__tab-hint">{label.hint}</span
                    >{/if}
                </button>
                <button
                  type="button"
                  class="c-code-view__tab-close"
                  title="Close"
                  aria-label={`Close ${label.name}`}
                  onclick={() => code.close(worktreeId, tab)}
                  ><Icon name="close" size={12} /></button
                >
              </div>
            {/each}
          </div>
        {/if}

        {#if active}
          <div class="c-code-view__crumbs" title={active}>
            {#each active.split('/') as part, i (i)}
              {#if i > 0}<Icon name="chevron-right" size={10} />{/if}
              <span>{part}</span>
            {/each}
            {#if file?.status === 'ready' && file.file.outside}
              <span class="c-code-view__flag">outside the worktree, through a link</span>
            {/if}
            {#if hunks && hunks.length > 0}
              <span class="c-code-view__spacer"></span>
              <span class="c-code-view__changes">
                {stepAt === null
                  ? `${hunks.length} change${hunks.length === 1 ? '' : 's'}`
                  : `${stepAt + 1} of ${hunks.length}`}
              </span>
              <Button
                variant="quiet"
                size="sm"
                icon="sm"
                title="Previous change"
                ariaLabel="Previous change"
                onclick={() => stepChange(-1)}><Icon name="chevron-up" size={12} /></Button
              >
              <Button
                variant="quiet"
                size="sm"
                icon="sm"
                title="Next change"
                ariaLabel="Next change"
                onclick={() => stepChange(1)}><Icon name="chevron-down" size={12} /></Button
              >
            {/if}
          </div>
        {/if}

        {#if file?.status === 'base'}
          <p class="c-code-view__notice">
            Deleted. This is how it was on <code>{file.against}</code>.
          </p>
        {/if}

        {#if file?.status === 'ready' && file.file.truncated}
          <p class="c-code-view__notice">
            Showing the first 5 MiB of {kib(file.file.size)}.
          </p>
        {/if}

        <CodeViewer
          bind:this={viewer}
          {worktreeId}
          path={active}
          {file}
          {hunks}
          comments={fileComments}
          {composing}
          {askLabel}
          {actions}
        />

        {#if !active}
          <div class="c-code-view__placeholder"><p>Choose a file on the left.</p></div>
        {:else if !file || file.status === 'loading'}
          <div class="c-code-view__placeholder"><p>Reading…</p></div>
        {:else if file.status === 'gone'}
          <div class="c-code-view__placeholder">
            <p>This file is no longer in the worktree.</p>
          </div>
        {:else if file.status === 'error'}
          <div class="c-code-view__placeholder"><p>{file.message}</p></div>
        {:else if file.status === 'ready' && file.file.text === null}
          <div class="c-code-view__placeholder">
            <p>A binary file, {kib(file.file.size)}. Nothing to read here.</p>
          </div>
        {/if}
      </div>

      {#if comments.length > 0}
        <CodeReview
          {worktreeId}
          {comments}
          {outdated}
          onjump={jumpTo}
          onsend={sendComments}
          oncopy={copyComments}
        />
      {/if}
    </div>
  {:else}
    <div class="c-code-view__placeholder"><p>Select a worktree on the left.</p></div>
  {/if}
</section>

{#if popup?.kind === 'find' && projectId && worktreeId}
  <FindInFiles
    {projectId}
    {worktreeId}
    initial={popup.query}
    onopen={(hit) => {
      const [from, to] = hit.ranges[0] ?? [0, 0];
      if (projectId && worktreeId) {
        code.open(projectId, worktreeId, hit.path, {
          line: hit.line,
          from: hit.offset + from,
          to: hit.offset + to,
        });
      }
    }}
    onclose={() => (popup = null)}
  />
{:else if popup?.kind === 'file' && worktreeId}
  <GoTo
    paths={tree?.paths ?? []}
    initial={popup.query}
    onpick={open}
    onclose={() => (popup = null)}
  />
{/if}
