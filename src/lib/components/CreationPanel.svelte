<script lang="ts">
  /**
   * A worktree the Home agent is creating or removing: which step it is on, and its terminal.
   *
   * The terminal is the real one — the setup runs in a pty and its output is kept for a late
   * attach — so a setup that stops to ask something can be answered here, as it could in the New
   * Worktree form. A removal has one only when a teardown step failed, which is the output worth
   * reading. Nothing here removes or retries anything by itself: a setup that failed keeps its
   * worktree, for the reason ARCHITECTURE §5 gives, and what to do about it is the user's call in
   * the worktree's own view.
   */
  import type { HomeJob } from '../ipc/types';
  import { fleet } from '../state/fleet.svelte';
  import Terminal from './Terminal.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';
  import SessionDot from './ui/SessionDot.svelte';

  const {
    job,
    onopenworktree,
  }: {
    job: HomeJob;
    onopenworktree: (projectId: string, worktreeId: string) => void;
  } = $props();

  const removing = $derived(job.kind === 'remove');
  const word = $derived(
    job.phase === 'running'
      ? removing
        ? 'removing…'
        : 'creating…'
      : job.phase === 'created' || job.phase === 'removed'
        ? job.phase
        : job.phase === 'setup_failed'
          ? 'setup failed'
          : removing
            ? 'not removed'
            : 'not created',
  );
  const status = $derived(
    job.phase === 'running'
      ? 'starting'
      : job.phase === 'created' || job.phase === 'removed'
        ? 'done'
        : 'failed',
  );
  const verb = $derived(removing ? 'Removing' : 'Creating');
  const name = $derived(job.directory.split('/').filter(Boolean).pop() ?? job.directory);
  const percent = $derived(
    job.step ? Math.round((job.step.index / Math.max(job.step.total, 1)) * 100) : 0,
  );
</script>

<section class="c-creation" aria-label="{verb} {name}">
  <header class="c-peek__head">
    <SessionDot {status} />
    <div class="c-peek__titles">
      <h2 class="c-peek__title">{job.projectName} › {name}</h2>
      <p class="c-peek__where">
        {job.branch ? `branch ${job.branch}` : 'detached'} · asked for by Home ·
        <span class="c-peek__status">{word}</span>
      </p>
    </div>
    <span class="c-peek__actions">
      <!-- Not for a worktree that is gone: a removal that failed left it, one that worked did not. -->
      {#if job.worktree && job.phase !== 'running' && job.phase !== 'removed'}
        <Button
          variant="neutral"
          size="sm"
          onclick={() => job.worktree && onopenworktree(job.projectId, job.worktree)}
        >
          Open worktree
        </Button>
      {/if}
      {#if job.phase !== 'running'}
        <Button
          variant="quiet"
          icon="sm"
          title="Dismiss"
          ariaLabel="Dismiss"
          onclick={() => fleet.dismissJob(job.id)}
        >
          <Icon name="close" size={12} />
        </Button>
      {/if}
    </span>
  </header>

  {#if job.phase === 'running'}
    <div class="c-creation__step">
      <div
        class="c-progress"
        role="progressbar"
        aria-valuenow={percent}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-label="{verb} {name}"
      >
        <div class="c-progress__fill" style:width="{percent}%"></div>
      </div>
      <p class="c-creation__label">{job.step?.label ?? 'Starting…'}</p>
    </div>
  {:else if job.error}
    <p class="c-creation__error">{job.error}</p>
  {/if}

  {#if job.setupSession}
    <div class="c-creation__terminal">
      <Terminal session={job.setupSession} />
    </div>
  {:else if job.phase === 'running' && !removing}
    <p class="c-home__hint">The setup's output appears here once it starts.</p>
  {/if}
</section>
