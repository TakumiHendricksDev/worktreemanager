<script lang="ts">
  /**
   * Every installed agent's usage limits, side by side.
   *
   * # Why a dialog and not a dropdown under the title-bar button
   *
   * A browser pane is a native view drawn over its tile, and nothing in the DOM can paint over it.
   * A dropdown hanging from the title bar would vanish behind any browser in the top row.
   * `Dialog` registers with `overlay`, which hides those views while it is up, and it brings
   * Escape, the scrim and the focus trap with it. It also unmounts on close, so nothing here
   * lingers when it is not being looked at, which is the same argument `AgentsDialog` makes.
   *
   * # Why it asks every provider when it opens
   *
   * Opening it is the "on demand" in ARCHITECTURE §8's rule. Each provider is asked separately and
   * fills in as it answers, because Codex starts an app server and Claude reads a file, and the
   * fast ones should not wait for the slow one.
   *
   * # Why the sign-in is here
   *
   * Which account pays is the other half of "how much is left", and a workflow run refuses anything
   * but a plan's sign-in. The line under each heading is that preflight's answer, asked with a run's
   * environment, so the method a run would bill is on screen before a run exists to refuse it. Its
   * third line names billing variables in wtm's own environment, because the preflight cannot see
   * them: it removes them first, while every pane opened from wtm still inherits them.
   */
  import { onMount } from 'svelte';

  import { sessions } from '../state/sessions.svelte';
  import { usage } from '../state/usage.svelte';
  import LimitMeters from './LimitMeters.svelte';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';

  const { onclose }: { onclose: () => void } = $props();

  const agents = $derived(sessions.options.filter((option) => option.available));
  const busy = $derived(agents.some((agent) => usage.asking[agent.id]));

  /** Taken on open and on Refresh, so every row judges its reset against the same moment. */
  let now = $state(Date.now());

  function refresh() {
    now = Date.now();
    for (const agent of agents) void usage.refresh(agent.id);
  }

  onMount(refresh);
</script>

<Dialog title="Usage limits" {onclose}>
  {#snippet body()}
    {#if agents.length === 0}
      <p>No agent CLI is installed, so there are no limits to show.</p>
    {:else}
      {#each agents as agent (agent.id)}
        {@const account = usage.accounts[agent.id]}
        <section class="c-usage">
          <h3 class="c-section-heading">
            {agent.label}
            {#if account?.limits.plan}
              <!-- Classed rather than a bare `small`, because the heading is uppercased and an
                   element selector under it would be a naked one. -->
              <span class="c-usage__plan">{account.limits.plan}</span>
            {/if}
          </h3>
          {#if account?.auth}
            {@const auth = account.auth}
            <p class="c-usage__signin">{auth.summary}.</p>
            {#if auth.refusal}
              <p class="c-usage__error">{auth.refusal}</p>
            {/if}
            {#if auth.inherited.length > 0}
              {@const them = auth.inherited.length === 1 ? 'it' : 'them'}
              <p class="c-usage__note">
                wtm's environment sets
                {#each auth.inherited as name, i (name)}{i > 0 ? ', ' : ''}<code
                    >{name}</code
                  >{/each}. Panes opened from wtm inherit {them}; workflow runs remove {them}.
              </p>
            {/if}
          {/if}
          <LimitMeters
            provider={agent.id}
            label={agent.label}
            {account}
            asking={usage.asking[agent.id] ?? false}
            {now}
          />
        </section>
      {/each}
    {/if}
  {/snippet}

  {#snippet footer()}
    <Button variant="quiet" onclick={refresh} disabled={busy || agents.length === 0}>
      {busy ? 'Refreshing…' : 'Refresh'}
    </Button>
    <Button variant="neutral" onclick={onclose}>Done</Button>
  {/snippet}
</Dialog>
