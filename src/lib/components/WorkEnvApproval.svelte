<script lang="ts">
  /**
   * A destructive command on a shared resource, waiting for your approval: the same card in the
   * Environment tab and in the inbox, so answering in either place answers it.
   *
   * # What it shows is what was declared
   *
   * The command as the config wrote it, templates and all, never rendered: a rendered argv can hold
   * a password from the worktree's `.env`, and ARCHITECTURE §6a keeps every value out of the
   * window. What the approval covers is the hash of the rendered command, so if the config or the
   * `.env` changes before you approve, the approval refuses to run anything and says so.
   *
   * # One approval, one run
   *
   * The next run asks again. That is the point of a shared resource: one worktree's migration can
   * break every other worktree's database, and "you said yes last Tuesday" is not a reason.
   */
  import type { PendingCommandView } from '../ipc/types';
  import CodeText from './CodeText.svelte';
  import Button from './ui/Button.svelte';

  const {
    pending,
    busy,
    error,
    onapprove,
    ondecline,
  }: {
    pending: PendingCommandView;
    busy: boolean;
    error: string | null;
    onapprove: () => void;
    ondecline: () => void;
  } = $props();

  const shared = $derived(pending.shared.join(', '));
</script>

<section
  class="c-work-state c-work-state--warn c-env-approval"
  aria-label="Approval needed"
>
  <span class="c-work-state__eyebrow">Approval needed · destructive command</span>
  <h3 class="c-work-state__title">
    <CodeText text={`Run ${pending.run.label} against ${shared}?`} />
  </h3>
  <pre class="c-env-approval__argv">{pending.run.argv.join(' ')}</pre>
  <p class="c-work-state__prose">
    <CodeText
      text={`In ${pending.worktreeName}. It acts on ${shared}, which every worktree shares, so it waits for your approval each time. This approval covers one run of exactly this command; if its config or this worktree's .env changes first, nothing runs.`}
    />
  </p>
  {#if error}
    <p class="c-work-state__error" role="alert">{error}</p>
  {/if}
  <div class="c-work-state__actions">
    <Button variant="danger-solid" size="sm" disabled={busy} onclick={onapprove}
      >Approve and run once</Button
    >
    <Button size="sm" disabled={busy} onclick={ondecline}>Decline</Button>
  </div>
</section>
