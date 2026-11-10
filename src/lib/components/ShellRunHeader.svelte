<script lang="ts">
  import { onMount } from 'svelte';
  import { commands } from '../ipc/commands';
  import { errorMessage } from '../ipc/types';
  import { runSettled, runStatus } from '../shell-script';
  import { shellRuns } from '../state/shell-runs.svelte';
  import { attention } from '../state/attention.svelte';
  import { inPaneWindow } from '../window-role';
  import Button from './ui/Button.svelte';
  const { session }: { session: string | null } = $props();
  const run = $derived(shellRuns.forSession(session));
  onMount(() => {
    if (!inPaneWindow) return;
    let gone = false;
    const ready = shellRuns.init().then((off) => {
      if (gone) off();
      return off;
    });
    return () => {
      gone = true;
      void ready.then((off) => off());
    };
  });
</script>

{#if run}
  <details class="c-shell-run__header">
    <summary
      >{run.requester ? 'Home command' : 'Reviewed command'} · {runStatus(run)}</summary
    >
    <p class="c-shell-run__directory">{run.request.interpreter} · {run.directory}</p>
    <pre class="c-shell-run__command">{run.request.command}</pre>
    {#if !runSettled(run)}<Button
        size="sm"
        onclick={() =>
          void commands
            .cancelShellRun(run.id)
            .catch((e) => attention.notice('Could not stop command', errorMessage(e)))}
        >Stop command</Button
      >{/if}
  </details>
{/if}
