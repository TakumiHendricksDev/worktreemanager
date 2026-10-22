<script lang="ts">
  /**
   * A review blocked on its environment: the checks that could not run and the runtime roles
   * waiting, and the one action that unblocks them. The same card in the Reviews tab and in the
   * inbox, so answering in either place answers it.
   *
   * # What it shows is what was declared
   *
   * Each service's start command as the config wrote it, templates and all, never rendered: a
   * rendered argv can hold a value from the worktree's `.env`, and ARCHITECTURE §6a keeps every
   * value out of the window. What the approval covers is the hash of the rendered commands, which
   * travels with the button; if the config or the `.env` changes before you click, nothing starts
   * and the card is read again.
   *
   * # Blocked is never a pass
   *
   * Until you start the environment the checks stay blocked and say so. Declining leaves them
   * blocked; it does not turn them into anything else.
   */
  import type { StartView } from '../ipc/types';
  import CodeText from './CodeText.svelte';
  import Button from './ui/Button.svelte';

  const {
    start,
    busy,
    error,
    onstart,
    ondecline,
  }: {
    start: StartView;
    busy: boolean;
    error: string | null;
    onstart: () => void;
    ondecline: () => void;
  } = $props();

  const services = $derived(start.request.services);
  const names = $derived(services.map((s) => s.label).join(', '));
  const capitalised = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);
  /** `a`, `a and b`, `a, b and c`. */
  const listed = (items: string[]) =>
    items.length <= 1
      ? (items[0] ?? '')
      : `${items.slice(0, -1).join(', ')} and ${items.at(-1)}`;
  const waiting = $derived([
    ...start.request.checks.map((c) => `check ${c.label}`),
    ...start.request.roles.map((r) => `runtime role ${r}`),
  ]);
</script>

<section
  class="c-work-state c-work-state--warn c-start-approval"
  aria-label="Environment not started"
>
  <span class="c-work-state__eyebrow">Approval needed · start the environment</span>
  <h3 class="c-work-state__title">
    <CodeText text={`Start ${names || 'the environment'} and run what it blocks?`} />
  </h3>
  <p class="c-work-state__prose">
    <CodeText
      text={`Blocked: environment not started. ${capitalised(listed(waiting))} ${waiting.length === 1 ? 'needs' : 'need'} ${services.length === 1 ? 'this service' : 'these services'} answering, and ${services.length === 1 ? 'it is' : 'they are'} not. Nothing has run, and nothing reads as passed. Starting waits for each service's health check before anything runs, and records which answered where.`}
    />
  </p>
  <dl class="o-facts c-start-approval__facts">
    {#each services as service (service.service)}
      <dt>Starts {service.label}</dt>
      <dd><pre class="c-start-approval__argv">{service.argv.join(' ')}</pre></dd>
    {/each}
    {#each start.request.checks as check (check.check)}
      <dt>Then runs {check.label}</dt>
      <dd><pre class="c-start-approval__argv">{check.argv.join(' ')}</pre></dd>
    {/each}
    {#if start.request.roles.length > 0}
      <dt>Then lets run</dt>
      <dd>{start.request.roles.join(', ')}</dd>
    {/if}
  </dl>
  {#if error}
    <p class="c-work-state__error" role="alert">{error}</p>
  {/if}
  <div class="c-work-state__actions">
    <Button variant="accent" size="sm" disabled={busy} onclick={onstart}
      >Start environment and run</Button
    >
    <Button size="sm" disabled={busy} onclick={ondecline}>Not now</Button>
  </div>
</section>
