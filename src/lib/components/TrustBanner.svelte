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
  import Button from './ui/Button.svelte';

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

<section class="c-trust" role="alert">
  {#if project.trust}
    <header class="c-trust__head">
      <strong class="c-trust__who">{project.name} requests local capabilities</strong>
      <code class="c-trust__path">{project.trust.path}</code>
    </header>

    <p class="c-trust__prose">
      This project's configuration declares the commands and database targets below. wtm
      will not use any of them until you approve — and it will ask again if the file
      changes. Read them the way you would read a <code>direnv</code> prompt: approval grants
      these capabilities to the repository.
    </p>

    {#if project.trust.commands.length > 0}
      <p class="c-trust__prose"><strong>Commands</strong></p>
      <ul class="c-trust__commands">
        {#each expanded ? project.trust.commands : project.trust.commands.slice(0, 4) as argv, i (i)}
          <li><code>{argv.join(' ')}</code></li>
        {/each}
      </ul>
    {/if}

    {#if project.trust.commands.length > 4 && !expanded}
      <Button variant="inline" onclick={() => (expanded = true)}>
        Show all {project.trust.commands.length} commands
      </Button>
    {/if}

    {#if project.trust.databases.length > 0}
      <p class="c-trust__prose"><strong>Database connections</strong></p>
      <ul class="c-trust__commands">
        {#each project.trust.databases as database (database.id)}
          <li>
            <code>{database.engine} · {database.target}</code>
            {#if database.passwordConfigured}
              — credential configured{/if}
          </li>
        {/each}
      </ul>
    {/if}

    {#if failure}
      <p class="c-status--danger">{failure}</p>
    {/if}

    <div class="c-trust__actions">
      <!-- Reject first, and neither is autofocused: the safe choice must not require
           more effort than the unsafe one. -->
      <Button variant="neutral" onclick={() => decide(false)} disabled={busy}>Deny</Button>
      <Button variant="accent" onclick={() => decide(true)} disabled={busy}>
        {busy ? 'Saving…' : 'Approve this configuration'}
      </Button>
    </div>
  {:else}
    <header class="c-trust__head">
      <strong class="c-trust__who">{project.name} could not be loaded</strong>
    </header>
    <p class="c-status--danger">{project.problem}</p>
    <div class="c-trust__actions">
      <Button variant="neutral" onclick={() => workspace.removeProject(project.root)}>
        Remove from wtm
      </Button>
      <Button variant="accent" onclick={() => workspace.refreshProjects()}>Retry</Button>
    </div>
  {/if}
</section>
