<script lang="ts">
  /**
   * The trust prompt.
   *
   * This is a security control, so it is built to be *read*, not clicked through:
   *
   * - Every command the config would run is listed verbatim, in a monospace block. A
   *   summary ("this config runs 4 commands") would train people to approve blindly.
   * - There is no default action and no auto-focus on Approve.
   * - The consequence is stated plainly, with the `direnv` comparison, because "trust
   *   this workspace" means nothing to someone who has not met the idea before.
   *
   * A project that cannot be loaded for some other reason renders here too, as a plain
   * error — better than a repo silently vanishing from the sidebar.
   */
  import type { Project } from '../ipc/types';
  import { errorMessage } from '../ipc/types';
  import { workspace } from '../state/workspace.svelte';

  const { project }: { project: Project } = $props();

  let busy = $state(false);
  let failure = $state<string | null>(null);
  let expanded = $state(false);

  async function decide(approve: boolean) {
    if (!project.trust) return;
    busy = true;
    failure = null;
    try {
      await workspace.decideTrust(project.trust.path, approve);
    } catch (e) {
      failure = errorMessage(e);
    } finally {
      busy = false;
    }
  }
</script>

<section class="banner" class:trust={!!project.trust} role="alert">
  {#if project.trust}
    <header>
      <strong>{project.name} wants to run commands</strong>
      <code class="path">{project.trust.path}</code>
    </header>

    <p>
      This project's configuration declares the commands below. wtm will not run any of them
      until you approve — and it will ask again if the file changes. Read them the way you
      would read a <code>direnv</code> prompt: approving is the same as running them yourself.
    </p>

    <ul class="commands">
      {#each expanded ? project.trust.commands : project.trust.commands.slice(0, 4) as argv, i (i)}
        <li><code>{argv.join(' ')}</code></li>
      {/each}
    </ul>

    {#if project.trust.commands.length > 4 && !expanded}
      <button class="more" onclick={() => (expanded = true)}>
        Show all {project.trust.commands.length} commands
      </button>
    {/if}

    {#if failure}
      <p class="failure">{failure}</p>
    {/if}

    <div class="actions">
      <!-- Reject first, and neither is autofocused: the safe choice must not require
           more effort than the unsafe one. -->
      <button class="reject" onclick={() => decide(false)} disabled={busy}>
        Don't run these
      </button>
      <button class="approve" onclick={() => decide(true)} disabled={busy}>
        {busy ? 'Saving…' : 'Approve this configuration'}
      </button>
    </div>
  {:else}
    <header>
      <strong>{project.name} could not be loaded</strong>
    </header>
    <p class="problem">{project.problem}</p>
    <div class="actions">
      <button class="reject" onclick={() => workspace.removeProject(project.root)}>
        Remove from wtm
      </button>
      <button class="approve" onclick={() => workspace.refreshProjects()}>Retry</button>
    </div>
  {/if}
</section>

<style>
  .banner {
    flex: 0 0 auto;
    margin: var(--sp-3) var(--sp-5) 0;
    padding: var(--sp-3) var(--sp-4);
    border-radius: var(--r-lg);
    border: 1px solid color-mix(in oklab, var(--warn) 34%, transparent);
    background: color-mix(in oklab, var(--warn) 10%, transparent);
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    font-size: var(--step--1);
  }

  header {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  strong {
    font-size: var(--step-0);
  }

  .path {
    color: var(--fg-muted);
    font-size: var(--step--2);
    overflow-wrap: anywhere;
  }

  p {
    line-height: 1.6;
    max-width: 76ch;
    color: var(--fg);
  }

  .commands {
    list-style: none;
    padding: var(--sp-2) var(--sp-3);
    margin: 0;
    background: var(--bg-code);
    border-radius: var(--r-md);
    display: flex;
    flex-direction: column;
    gap: 3px;
    /* Long argv must scroll rather than widen the pane. */
    overflow-x: auto;
  }

  .commands code {
    white-space: pre;
    font-size: var(--step--2);
  }

  .more {
    align-self: flex-start;
    color: var(--fg-muted);
    font-size: var(--step--2);
    text-decoration: underline;
  }

  .failure,
  .problem {
    color: var(--danger);
  }

  .actions {
    display: flex;
    gap: var(--sp-2);
    margin-top: var(--sp-1);
  }

  .actions button {
    padding: 6px 12px;
    border-radius: var(--r-md);
    font-size: var(--step--1);
    font-weight: 500;
  }

  .reject {
    border: 1px solid var(--border-strong);
    background: var(--bg-elevated);
    color: var(--fg);
  }

  .reject:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  .approve {
    background: var(--accent);
    color: var(--fg-on-accent);
  }

  .approve:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .actions button:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
