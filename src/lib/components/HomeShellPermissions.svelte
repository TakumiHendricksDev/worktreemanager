<script lang="ts">
  import { commands } from '../ipc/commands';
  import { errorMessage, type Worktree } from '../ipc/types';
  import type { HomeShellGrant } from '../shell-script';
  import { workspace } from '../state/workspace.svelte';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Field from './ui/Field.svelte';
  const { home, onclose }: { home: string; onclose: () => void } = $props();
  let project = $state(''),
    worktree = $state(''),
    directory = $state('');
  let worktrees = $state<Worktree[]>([]),
    grants = $state<HomeShellGrant[]>([]);
  let busy = $state(false),
    error = $state<string | null>(null);
  $effect(() => {
    const owner = home;
    let gone = false;
    void commands
      .listHomeShellGrants(owner)
      .then((items) => {
        if (!gone) grants = items;
      })
      .catch((e) => {
        if (!gone) error = errorMessage(e);
      });
    return () => {
      gone = true;
    };
  });
  $effect(() => {
    const id = project;
    let gone = false;
    worktrees = [];
    if (id)
      void commands
        .listWorktrees(id)
        .then((items) => {
          if (!gone) worktrees = items;
        })
        .catch((e) => {
          if (!gone) error = errorMessage(e);
        });
    return () => {
      gone = true;
    };
  });
  $effect(() => {
    const p = project,
      w = worktree;
    let gone = false;
    directory = '';
    if (p && w)
      void commands
        .shellRunTarget(p, w)
        .then((path) => {
          if (!gone) directory = path;
        })
        .catch((e) => {
          if (!gone) error = errorMessage(e);
        });
    return () => {
      gone = true;
    };
  });
  async function change(projectId: string, worktreeId: string, allow: boolean) {
    if (busy) return;
    busy = true;
    error = null;
    try {
      await commands.setHomeShellGrant(home, projectId, worktreeId, allow);
      grants = await commands.listHomeShellGrants(home);
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog title="Home command permissions" {onclose} closeDisabled={busy} wide>
  {#snippet body()}
    <div class="c-shell-run__review">
      <p>
        By default, every command waits for your review in Needs you. A temporary grant lets
        this Home session run commands in its own untouched shells in the selected worktree.
      </p>
      <p>
        Commands can modify or delete files, use the network, and access anything your user
        can access, including outside the worktree. The grant lasts until Home closes or wtm
        quits. Changes to the worktree or trusted configuration invalidate it.
      </p>
      {#each grants as grant (grant.projectId + grant.worktreeId)}
        <div class="c-shell-run__review">
          <p class="c-shell-run__directory">
            {grant.directory} · {grant.valid
              ? 'Allowed for this Home session'
              : 'Expired — review again to allow'}
          </p>
          <Button
            size="sm"
            disabled={busy}
            onclick={() => void change(grant.projectId, grant.worktreeId, false)}
            >Revoke</Button
          >
        </div>
      {/each}
      <Field id="grant-project" label="Repository"
        ><select
          id="grant-project"
          class="c-select"
          bind:value={project}
          disabled={busy}
          onchange={() => (worktree = '')}
        >
          <option value="">Choose a repository…</option
          >{#each workspace.projects as item (item.id)}<option value={item.id}
              >{item.name}</option
            >{/each}
        </select></Field
      >
      <Field id="grant-worktree" label="Worktree"
        ><select
          id="grant-worktree"
          class="c-select"
          bind:value={worktree}
          disabled={busy || !project}
        >
          <option value="">Choose a worktree…</option
          >{#each worktrees as item (item.id)}<option value={item.id}>{item.title}</option
            >{/each}
        </select></Field
      >
      <p class="c-shell-run__directory">{directory}</p>
      <Button
        variant="accent"
        disabled={busy || !directory}
        onclick={() => void change(project, worktree, true)}
        >Allow Home commands in this worktree</Button
      >
      <p class="c-note">
        Allowing does not approve requests already in Needs you. Revoking prevents new
        starts; use Stop command in a shell to stop work already running.
      </p>
      {#if error}<p class="c-status--danger" role="alert">{error}</p>{/if}
    </div>
  {/snippet}
  {#snippet footer()}<Button disabled={busy} onclick={onclose}>Done</Button>{/snippet}
</Dialog>
