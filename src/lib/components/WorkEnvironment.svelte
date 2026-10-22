<script lang="ts">
  /**
   * A worktree's Environment tab: the D1 W5 panel. Its services with their state and what to do
   * about it, the resources they use and whose those are, the commands you can run, and the log of
   * whichever service you open it for.
   *
   * # States, and what each offers
   *
   * Stopped and failed offer Start; a running service offers Restart and Stop, and Open once it is
   * healthy and has a preview; an unhealthy one also offers Check again, which asks it once more.
   * Nothing here asks on a timer: health is asked after a start until it answers or its deadline
   * passes, and after that only when you press something (ARCHITECTURE §8).
   *
   * # A port in use is reported, never freed
   *
   * A start refused for a port names who holds it: a program and its pid from `lsof`, a container
   * from `docker ps` when the project asks, or another worktree's service that wtm started. wtm never
   * ends a process it did not start, so the fix is yours.
   *
   * # Left running
   *
   * A service the last run of wtm left running — a detached one, which quitting leaves up, or one a
   * crash orphaned — is listed as such, with Stop.
   */
  import {
    addressOf,
    conflictWords,
    IDLE_CHOICES,
    ISOLATION_WORDS,
    isRunning,
    RUN_TONE,
    RUN_WORDS,
    STATE_DOT,
    STATE_WORDS,
  } from '../environment';
  import type { ServiceView, Worktree } from '../ipc/types';
  import { openPreview } from '../open-preview';
  import { environments } from '../state/environments.svelte';
  import CodeText from './CodeText.svelte';
  import WorkEnvApproval from './WorkEnvApproval.svelte';
  import Button from './ui/Button.svelte';

  const {
    projectId,
    worktree,
    onopenworktree,
  }: {
    projectId: string;
    worktree: Worktree;
    onopenworktree: (worktreeId: string) => void;
  } = $props();

  const view = $derived(environments.views[worktree.id] ?? null);
  const busy = $derived(environments.busy[worktree.id] ?? false);
  const error = $derived(environments.errors[worktree.id] ?? null);
  const pending = $derived(environments.pending[worktree.id] ?? null);
  const shownLog = $derived(environments.logs[worktree.id] ?? null);
  const anyRunning = $derived(view?.services.some((s) => isRunning(s.state)) ?? false);
  const anyStopped = $derived(view?.services.some((s) => !isRunning(s.state)) ?? false);
  const leftRunning = $derived(view?.services.filter((s) => s.leftRunning) ?? []);
  const them = $derived(leftRunning.length === 1 ? 'it' : 'them');
  const isolation = $derived(
    view?.resources.map((r) => `${r.label}: ${ISOLATION_WORDS[r.isolation]}`).join(' · ') ??
      '',
  );

  function start(service: string | null): void {
    void environments.start(projectId, worktree.id, service);
  }
  function stop(service: string | null): void {
    void environments.stop(projectId, worktree.id, service);
  }
  function restart(service: string): void {
    void environments.restart(projectId, worktree.id, service);
  }
  function open(service: ServiceView): void {
    void openPreview(projectId, worktree.id, service.id, onopenworktree);
  }

  function idleChanged(event: Event): void {
    const value = (event.currentTarget as HTMLSelectElement).value;
    void environments.setIdleStop(
      projectId,
      worktree.id,
      value === '' ? null : Number(value),
    );
  }

  function when(at: number): string {
    return new Date(at).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
  }
</script>

{#if !view}
  <p class="c-work-state__note">Reading the environment…</p>
{:else if view.problem}
  <section class="c-work-state c-work-state--warn">
    <span class="c-work-state__eyebrow">Environment unavailable</span>
    <p class="c-work-state__prose">{view.problem}</p>
  </section>
{:else}
  <div class="c-env">
    <header class="c-env__head">
      <h2 class="c-env__title">
        Environment <span class="c-env__where u-mono"
          >· {worktree.branch ?? worktree.path}</span
        >
      </h2>
      {#if isolation}
        <p class="c-env__isolation">Isolation · {isolation}</p>
      {/if}
    </header>

    {#if error}
      <p class="c-work-state__error" role="alert"><CodeText text={error} /></p>
    {/if}

    {#if pending}
      <WorkEnvApproval
        {pending}
        {busy}
        error={null}
        onapprove={() => void environments.approve(projectId, worktree.id, pending.run.id)}
        ondecline={() => void environments.decline(projectId, worktree.id, pending.run.id)}
      />
    {/if}

    {#if leftRunning.length > 0}
      <p class="c-env__notice">
        Left running when wtm last closed: {leftRunning.map((s) => s.label).join(', ')}.
        Stop {them} here, or leave {them} running.
      </p>
    {/if}

    <ul class="c-env__services" aria-label="Services">
      {#each view.services as service (service.id)}
        {@const address = addressOf(service)}
        <li class="c-env-service">
          <div class="c-env-service__row">
            <span class="c-dot c-dot--{STATE_DOT[service.state]}" aria-hidden="true"></span>
            <span class="c-env-service__name">{service.label}</span>
            <span
              class="c-env-service__state c-env-service__state--{STATE_DOT[service.state]}"
              >{STATE_WORDS[service.state]}</span
            >
            {#if address}
              <span class="c-env-service__address u-mono">{address}</span>
            {/if}
            {#if service.detached}
              <span class="c-env-service__tag">detached</span>
            {/if}
            <span class="c-env-service__actions">
              {#if service.state === 'healthy' && service.preview}
                <Button size="sm" disabled={busy} onclick={() => open(service)}>Open</Button
                >
              {/if}
              {#if service.state === 'unhealthy'}
                <Button
                  size="sm"
                  disabled={busy}
                  onclick={() =>
                    void environments.check(projectId, worktree.id, service.id)}
                  >Check again</Button
                >
              {/if}
              {#if service.hasLog}
                <Button
                  size="sm"
                  variant="quiet"
                  ariaPressed={shownLog?.service === service.id}
                  onclick={() =>
                    void environments.toggleLog(projectId, worktree.id, service.id)}
                  >Logs</Button
                >
              {/if}
              {#if isRunning(service.state)}
                <Button size="sm" disabled={busy} onclick={() => restart(service.id)}
                  >Restart</Button
                >
                <Button size="sm" disabled={busy} onclick={() => stop(service.id)}
                  >Stop</Button
                >
              {:else}
                <Button
                  size="sm"
                  variant="accent"
                  disabled={busy}
                  onclick={() => start(service.id)}>Start</Button
                >
              {/if}
            </span>
          </div>
          {#if service.detail && service.conflicts.length === 0}
            <p class="c-env-service__detail">
              <CodeText
                text={service.detail}
              />{#if service.updatedAt && service.state !== 'stopped'}
                <span class="c-env-service__when">· {when(service.updatedAt)}</span>{/if}
            </p>
          {/if}
          {#each service.conflicts as conflict (conflict.port)}
            <p class="c-env-service__conflict" role="status">
              {conflictWords(conflict, worktree.id)} wtm leaves it running; stop it yourself,
              or give this worktree another port in its .env.
            </p>
          {/each}
          {#each service.ports.filter((p) => p.problem) as port, index (index)}
            <p class="c-env-service__conflict">Port {port.problem}</p>
          {/each}
          <p class="c-env-service__health">
            Health: <span class="u-mono">{service.health}</span>{#if service.pid}<span
                class="c-env-service__when">· pid {service.pid}</span
              >{/if}
          </p>
        </li>
      {/each}
    </ul>

    {#if shownLog}
      <section class="c-env-log" aria-label="Log of {shownLog.service}">
        <header class="c-env-log__head">
          <span>Log · {shownLog.service}</span>
          {#if shownLog.log?.truncated}<span class="c-env-log__note">the last 64 KB</span
            >{/if}
          <Button
            size="sm"
            variant="quiet"
            onclick={() =>
              void environments.readLog(projectId, worktree.id, shownLog.service)}
            >Refresh</Button
          >
        </header>
        <pre class="c-env-log__text">{shownLog.log
            ? shownLog.log.text.trim() || 'Nothing written yet.'
            : 'Reading…'}</pre>
        {#if shownLog.log}
          <p class="c-env-log__note u-mono">{shownLog.log.path}</p>
        {/if}
      </section>
    {/if}

    {#if view.commands.length > 0}
      <section class="c-env__commands" aria-label="Commands">
        <h3 class="c-env__subtitle">Commands</h3>
        <ul class="c-env__list">
          {#each view.commands as command (command.id)}
            <li class="c-env-command">
              <div class="c-env-command__text">
                <span class="c-env-command__name">{command.label}</span>
                <span class="c-env-command__argv u-mono">{command.argv.join(' ')}</span>
                {#if command.needsApproval}
                  <span class="c-env-command__policy"
                    >Destructive, on {command.resources.join(', ')}, which is shared: needs
                    your approval each time</span
                  >
                {:else if command.destructive}
                  <span class="c-env-command__policy c-env-command__policy--quiet"
                    >Destructive, on this worktree's own {command.resources.join(
                      ', ',
                    )}</span
                  >
                {/if}
              </div>
              <Button
                size="sm"
                variant={command.needsApproval ? 'danger-outline' : 'neutral'}
                disabled={busy || pending !== null}
                onclick={() => void environments.run(projectId, worktree.id, command.id)}
                >{command.needsApproval ? 'Ask to run' : 'Run'}</Button
              >
            </li>
          {/each}
        </ul>
        {#if view.runs.length > 0}
          <ul class="c-env__runs" aria-label="Recent runs">
            {#each view.runs as run (run.id)}
              <li class="c-env-run">
                <span class="c-env-run__state c-env-run__state--{RUN_TONE[run.state]}"
                  >{RUN_WORDS[run.state]}{run.exitCode !== null && run.state !== 'succeeded'
                    ? ` (exit ${run.exitCode})`
                    : ''}</span
                >
                <span class="c-env-run__label">{run.label}</span>
                <span class="c-env-run__when">{when(run.requestedAt)}</span>
                {#if run.output}
                  <pre class="c-env-run__output">{run.output.trim()}</pre>
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
      </section>
    {/if}

    <footer class="c-env__foot">
      {#if view.services.length > 1}
        <Button size="sm" disabled={busy || !anyStopped} onclick={() => start(null)}
          >Start all</Button
        >
        <Button size="sm" disabled={busy || !anyRunning} onclick={() => stop(null)}
          >Stop all</Button
        >
      {/if}
      <label class="c-env__idle">
        Idle stop
        <select
          class="c-select"
          value={view.idleStopMinutes === null ? '' : String(view.idleStopMinutes)}
          onchange={idleChanged}
          disabled={busy}
        >
          {#each IDLE_CHOICES as choice (choice.label)}
            <option value={choice.minutes === null ? '' : String(choice.minutes)}
              >{choice.label}</option
            >
          {/each}
        </select>
        <span class="c-env__idle-note">after this long with wtm in the background</span>
      </label>
    </footer>
  </div>
{/if}
