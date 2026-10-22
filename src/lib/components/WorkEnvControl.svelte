<script lang="ts">
  /**
   * The workspace header's environment control: the plan's §2.6 Start / Stop / Restart / Open
   * preview / Logs, for every service at once, with the worst state among them as a dot and a word.
   *
   * The worst state rather than a count, because it is the one that decides whether you can test:
   * one failed service among three healthy ones is a failed environment. Open preview opens the
   * first healthy service with a preview, verified first like every Open (`open-preview.ts`); Logs
   * goes to the Environment tab with that service's log open, since a log needs the room.
   *
   * Shown only for a worktree whose project declares an `[environment]`.
   */
  import { isRunning, previewable, summary } from '../environment';
  import type { EnvironmentView } from '../ipc/types';
  import { openPreview } from '../open-preview';
  import { environments } from '../state/environments.svelte';
  import Button from './ui/Button.svelte';

  const {
    projectId,
    worktreeId,
    view,
    onopenworktree,
    onlogs,
  }: {
    projectId: string;
    worktreeId: string;
    view: EnvironmentView;
    onopenworktree: (worktreeId: string) => void;
    /** Show the Environment tab. */
    onlogs: () => void;
  } = $props();

  const busy = $derived(environments.busy[worktreeId] ?? false);
  const said = $derived(summary(view));
  const anyRunning = $derived(view.services.some((s) => isRunning(s.state)));
  const anyStopped = $derived(view.services.some((s) => !isRunning(s.state)));
  const preview = $derived(previewable(view));
  const logged = $derived(view.services.find((s) => s.hasLog) ?? null);

  function logs(): void {
    onlogs();
    if (logged && environments.logs[worktreeId]?.service !== logged.id) {
      void environments.toggleLog(projectId, worktreeId, logged.id);
    }
  }
</script>

<div class="c-env-control" role="group" aria-label="Environment">
  <span class="c-env-control__state">
    <span class="c-dot c-dot--{said.tone}" aria-hidden="true"></span>
    Env: {said.label}
  </span>
  <Button
    size="sm"
    disabled={busy || !anyStopped}
    onclick={() => void environments.start(projectId, worktreeId, null)}>Start</Button
  >
  <Button
    size="sm"
    disabled={busy || !anyRunning}
    onclick={() => void environments.stop(projectId, worktreeId, null)}>Stop</Button
  >
  <Button
    size="sm"
    disabled={busy || !anyRunning}
    onclick={() => void environments.restart(projectId, worktreeId, null)}>Restart</Button
  >
  <Button
    size="sm"
    disabled={busy || preview === null}
    title={preview?.preview ?? 'Nothing healthy has a preview yet'}
    onclick={() =>
      preview && void openPreview(projectId, worktreeId, preview.id, onopenworktree)}
    >Open preview</Button
  >
  <Button size="sm" variant="quiet" disabled={logged === null} onclick={logs}>Logs</Button>
</div>
