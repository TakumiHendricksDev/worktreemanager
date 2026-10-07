<script lang="ts">
  import { untrack } from 'svelte';
  import { commands } from '../ipc/commands';
  import { errorMessage, type Worktree } from '../ipc/types';
  import type { RunShell, ShellInterpreter } from '../shell-script';
  import { shellRuns, type ShellReview } from '../state/shell-runs.svelte';
  import { workspace } from '../state/workspace.svelte';
  import { sessions } from '../state/sessions.svelte';
  import { view } from '../state/view.svelte';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Field from './ui/Field.svelte';

  const { review }: { review: ShellReview } = $props();
  let projectId = $state(untrack(() => review.projectId));
  let worktreeId = $state(untrack(() => review.worktreeId));
  let command = $state(untrack(() => review.snippet.command.replace(/\r\n/g, '\n')));
  let interpreter = $state<ShellInterpreter>(untrack(() => review.snippet.interpreter));
  let shell = $state(untrack(() => review.run?.request.shell ?? 'auto'));
  let worktrees = $state<Worktree[]>([]);
  let shells = $state<RunShell[]>([]);
  let directory = $state('');
  let loading = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);
  const homeRequest = $derived(review.run !== undefined);
  const pending = $derived(
    !review.run ||
      (shellRuns.runs.find((run) => run.id === review.run?.id)?.phase ??
        review.run.phase) === 'awaiting_approval',
  );
  const prompt = $derived(/^\$ [^\n]+\n?$/.test(command));
  const normalized = untrack(() => review.snippet.command.includes('\r\n'));

  $effect(() => {
    const project = projectId;
    let cancelled = false;
    worktrees = [];
    if (project)
      void commands
        .listWorktrees(project)
        .then((items) => {
          if (!cancelled) worktrees = items;
        })
        .catch((e) => {
          if (!cancelled) error = errorMessage(e);
        });
    return () => {
      cancelled = true;
    };
  });
  $effect(() => {
    const project = projectId,
      worktree = worktreeId;
    let cancelled = false;
    directory = '';
    shells = [];
    loading = !!(project && worktree);
    if (project && worktree)
      void Promise.all([
        commands.shellRunTarget(project, worktree),
        commands.listRunShells(project, worktree),
      ])
        .then(([path, items]) => {
          if (!cancelled) {
            directory = path;
            shells = items;
          }
        })
        .catch((e) => {
          if (!cancelled) error = errorMessage(e);
        })
        .finally(() => {
          if (!cancelled) loading = false;
        });
    return () => {
      cancelled = true;
    };
  });
  function close() {
    if (!busy) shellRuns.review = null;
  }
  async function deny() {
    if (!review.run || busy) return;
    busy = true;
    try {
      await commands.denyShellRun(review.run.id);
      shellRuns.review = null;
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }
  async function run() {
    if (busy || !directory || !pending) return;
    busy = true;
    error = null;
    try {
      const prepared =
        review.run ??
        (await commands.prepareShellRun({
          key: crypto.randomUUID(),
          projectId,
          worktreeId,
          command,
          interpreter,
          shell,
        }));
      if (prepared.directory !== directory || prepared.request.command !== command) {
        await commands.denyShellRun(prepared.id);
        throw new Error(
          'The command or directory changed during review. Close this sheet and review a new request. Nothing ran.',
        );
      }
      const approved = await commands.approveShellRun(prepared.id);
      shellRuns.upsert(approved);
      shellRuns.review = null;
      if (projectId !== workspace.activeProjectId) await workspace.selectProject(projectId);
      workspace.select(worktreeId);
      view.show('worktree');
      const pane = sessions.panes.find(
        (pane) => approved.session !== null && pane.session === approved.session,
      );
      if (pane) sessions.focus(worktreeId, pane.id);
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog
  title={homeRequest ? 'Review Home command' : 'Run in a shell'}
  onclose={close}
  closeDisabled={busy}
  wide
>
  {#snippet body()}
    <div class="c-shell-run__review">
      <p>
        Review the whole command. Run executes it immediately with your normal user access,
        including access outside this worktree.
      </p>
      <Field id="run-project" label="Repository">
        <select
          id="run-project"
          class="c-select"
          bind:value={projectId}
          disabled={busy || homeRequest || !!review.projectId}
          onchange={() => {
            worktreeId = '';
            shell = 'auto';
          }}
        >
          <option value="">Choose a repository…</option>
          {#each workspace.projects as project (project.id)}<option value={project.id}
              >{project.name}</option
            >{/each}
        </select>
      </Field>
      <Field id="run-worktree" label="Worktree">
        <select
          id="run-worktree"
          class="c-select"
          bind:value={worktreeId}
          disabled={busy || homeRequest || !!review.worktreeId || !projectId}
          onchange={() => (shell = 'auto')}
        >
          <option value="">Choose a worktree…</option>
          {#each worktrees as worktree (worktree.id)}<option value={worktree.id}
              >{worktree.issueKey ? `${worktree.issueKey} · ` : ''}{worktree.title}</option
            >{/each}
        </select>
      </Field>
      <p class="c-shell-run__directory" aria-label="Working directory">
        {loading
          ? 'Checking worktree…'
          : directory || 'Select a target. Home has no default worktree.'}
      </p>
      <Field
        id="run-interpreter"
        label="Child interpreter"
        help="Starts at the worktree root; the parent shell’s directory and settings stay as they are."
      >
        <select
          id="run-interpreter"
          class="c-select"
          bind:value={interpreter}
          disabled={busy || homeRequest}
        >
          <option value="sh">sh</option><option value="bash">bash</option><option
            value="zsh">zsh</option
          >
        </select>
      </Field>
      <Field
        id="run-shell"
        label="Destination"
        help="Reuse requires a verified empty zsh prompt with no jobs. Otherwise Auto opens a visible shell."
      >
        <select
          id="run-shell"
          class="c-select"
          bind:value={shell}
          disabled={busy || homeRequest}
        >
          <option value="auto">Auto · most recently focused free shell, or new</option>
          <option value="new">New shell pane</option>
          {#each shells as item (item.session)}<option
              value={item.session}
              disabled={item.availability !== 'ready'}
              >Shell {item.session} · {item.availability}</option
            >{/each}
        </select>
      </Field>
      <Field
        id="run-script"
        label="Exact script"
        help="Multiline commands, comments, heredocs and continuations run as shown. Review destructive commands here too; there is no second confirmation."
      >
        <textarea
          id="run-script"
          class="c-textarea c-shell-run__script"
          rows={10}
          bind:value={command}
          spellcheck="false"
          readonly={homeRequest}
          disabled={busy}></textarea>
      </Field>
      {#if normalized}<p class="c-note">
          Windows line endings were converted to LF in the script shown above.
        </p>{/if}
      {#if prompt && !homeRequest}<Button
          size="sm"
          onclick={() => (command = command.slice(2))}>Remove leading $ prompt</Button
        >{/if}
      {#if !pending}<p role="status">
          This request is no longer waiting for approval.
        </p>{/if}
      {#if error}<p class="c-status--danger" role="alert">{error}</p>{/if}
    </div>
  {/snippet}
  {#snippet footer()}
    <Button disabled={busy} onclick={close}>Cancel</Button>
    {#if homeRequest}<Button
        variant="danger-outline"
        disabled={busy || !pending}
        onclick={() => void deny()}>Deny</Button
      >{/if}
    <Button
      variant="accent"
      disabled={busy || loading || !directory || !command.trim() || !pending}
      onclick={() => void run()}>{busy ? 'Starting…' : 'Run'}</Button
    >
  {/snippet}
</Dialog>
