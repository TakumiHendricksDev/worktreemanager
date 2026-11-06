<script lang="ts">
  import { sessions } from '/src/lib/state/sessions.svelte';
  import SessionPane from '/src/lib/components/SessionPane.svelte';
  import NewAgentSessionDialog from '/src/lib/components/NewAgentSessionDialog.svelte';
  import Button from '/src/lib/components/ui/Button.svelte';
  let open = $state(false);
</script>

<nav><Button onclick={() => (open = true)}>New agent session</Button></nav>
<p>Launch regression · real session store and pane controls, mocked backend</p>
{#if sessions.panes[0]}
  {#key sessions.panes[0].id}
    <SessionPane
      pane={sessions.panes[0]}
      visible={true}
      host={{ kind: 'tile', onmovestart: () => {}, onmovekey: () => {} }}
    />
  {/key}
{/if}
{#if open}
  <NewAgentSessionDialog
    projectId="example"
    worktreeId="/tmp/example"
    onclose={() => (open = false)}
  />
{/if}
