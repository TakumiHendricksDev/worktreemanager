<script lang="ts">
  /**
   * The New Worktree dialog: form → review → run.
   *
   * The form comes entirely from the project's config, and so do the dropdown contents. This
   * component knows nothing about any particular project's workflow.
   *
   * Rendered as a **pane**, not a modal. The form, the review screen and a live terminal do not
   * fit comfortably in a dialog, and a modal implies a quick decision when this is a task you
   * watch — a setup run can take minutes. It replaces the detail pane while creating, and hands
   * it back when finished.
   *
   * Three deliberate behaviours:
   *
   * 1. **Preview is debounced, not on-submit.** The pipeline's stages 1–6 mutate nothing, so
   *    the review screen can update as you type. Debounced because a `[[lookup]]` is a network
   *    call and a preview per keystroke would hammer it.
   * 2. **Create is disabled until preflight is clear** — or until every overridable failure is
   *    explicitly acknowledged. It cannot be clicked into a state git will refuse.
   * 3. **A failed setup is not an error.** The worktree exists; the dialog switches to the
   *    transcript and offers the remedies rather than throwing away what was built.
   */
  import { commands } from '../ipc/commands';
  import {
    errorMessage,
    isWtmError,
    type CreateOutcome,
    type Form,
    type Preview,
    type ProgressEvent,
  } from '../ipc/types';
  import { listen } from '@tauri-apps/api/event';
  import { workspace } from '../state/workspace.svelte';
  import ReviewPanel from './ReviewPanel.svelte';
  import RemoveWorktreeDialog from './RemoveWorktreeDialog.svelte';
  import SchemaForm from './SchemaForm.svelte';
  import Terminal from './Terminal.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    projectId,
    onclose,
  }: {
    projectId: string;
    /** Return to the worktree detail view. */
    onclose: () => void;
  } = $props();

  /** A preview costs a lookup command, so wait for typing to settle. */
  const PREVIEW_DEBOUNCE_MS = 400;

  type Phase = 'form' | 'running' | 'done';

  let form = $state<Form | null>(null);
  let loadError = $state<string | null>(null);
  const values = $state<Record<string, string>>({});

  let preview = $state<Preview | null>(null);
  let previewError = $state<string | null>(null);
  /** Field-level problems, keyed by field, from a validation rejection. */
  let fieldProblems = $state<Record<string, string>>({});
  let previewing = $state(false);
  /**
   * Monotonic id of the newest preview request.
   *
   * Deliberately not `$state`: nothing renders it, and making it reactive would make the
   * effect that bumps it depend on itself.
   */
  let previewSeq = 0;
  /** Same shape as `previewSeq`: a stale form response must not clobber a newer project's. */
  let formSeq = 0;

  let phase = $state<Phase>('form');
  let adoptBranch = $state<string | null>(null);
  const acknowledged = $state<string[]>([]);

  let session = $state<string | null>(null);
  let outcome = $state<CreateOutcome | null>(null);
  let runError = $state<string | null>(null);
  let confirmingRemove = $state(false);
  let acting = $state(false);

  const removable = $derived(outcome && 'worktree' in outcome ? outcome.worktree : null);

  /**
   * What the run is doing, as it does it.
   *
   * The first version put a single stage label in the header and left the body empty, which is
   * the worst of both: the one thing worth watching was the smallest text on screen, and a
   * multi-minute setup looked like a hung window. The pipeline already emits typed progress —
   * this keeps it and renders it where the eye is.
   */
  type Step = {
    id: string;
    label: string;
    index: number;
    total: number;
    /** Set once a later stage begins, which is what "this one finished" means here. */
    done: boolean;
  };

  let steps = $state<Step[]>([]);
  let currentCommand = $state<{ argv: string[]; cwd: string } | null>(null);
  let notes = $state<{ id: string; text: string; kind: 'warning' | 'note' }[]>([]);

  const currentStep = $derived(steps.find((s) => !s.done) ?? null);
  const percent = $derived(
    currentStep && currentStep.total > 0
      ? Math.round(((currentStep.index - 1) / currentStep.total) * 100)
      : phase === 'done'
        ? 100
        : 0,
  );

  const canCreate = $derived(
    !!preview &&
      preview.preflight
        .filter((p) => p.severity === 'error')
        .every((p) => p.overridable && acknowledged.includes(p.id)),
  );

  $effect(() => {
    const id = projectId;
    const seq = ++formSeq;
    void commands
      .worktreeForm(id)
      .then((f) => {
        if (seq !== formSeq) return;
        form = f;
      })
      .catch((e) => {
        if (seq !== formSeq) return;
        loadError = errorMessage(e);
      });
  });

  // Live preview. Depends on every value, so editing any field re-plans.
  $effect(() => {
    if (phase !== 'form' || !form) return;
    // Touch the values so this effect tracks them.
    const snapshot = JSON.stringify(values);
    const chosen = adoptBranch;

    // Which preview is current. The debounce cancels a *pending* request, but nothing can
    // cancel one already in flight — and a preview runs the project's `[[lookup]]` commands,
    // which for an issue tracker is a network call of very variable latency. So edit an issue
    // key while the previous lookup is still running and two requests overlap; without this,
    // whichever *resolves* last wins rather than whichever was *issued* last, and a stale
    // answer landing second leaves the review pane showing the previous issue with nothing
    // to dislodge it until you type again. Same guard the worktree list uses.
    const seq = ++previewSeq;
    const current = () => seq === previewSeq;

    const timer = setTimeout(() => {
      previewing = true;
      void commands
        .previewWorktree(projectId, JSON.parse(snapshot) as Record<string, string>, chosen)
        .then((result) => {
          if (!current()) return;
          preview = result;
          previewError = null;
          fieldProblems = {};
        })
        .catch((e) => {
          if (!current()) return;
          preview = null;
          // A validation rejection carries per-field detail; show it inline rather than as
          // one opaque banner.
          if (isWtmError(e) && e.kind === 'validation') {
            const detail = e.detail as {
              validation?: { field: string; message: string }[];
            };
            const problems: Record<string, string> = {};
            for (const problem of detail?.validation ?? []) {
              problems[problem.field] = problem.message;
            }
            fieldProblems = problems;
            previewError = Object.keys(problems).length > 0 ? null : errorMessage(e);
          } else {
            fieldProblems = {};
            previewError = errorMessage(e);
          }
        })
        .finally(() => {
          // Only the current request may clear the indicator, or a stale one finishing
          // first would report "done" while the real answer is still on its way.
          if (current()) previewing = false;
        });
    }, PREVIEW_DEBOUNCE_MS);

    return () => clearTimeout(timer);
  });

  // Pipeline progress. Subscribed for the whole life of the pane, not just while running, so
  // nothing emitted between pressing Create and the first render is missed.
  $effect(() => {
    const unlisten = listen<ProgressEvent>('wtm:progress', (event) => {
      const payload = event.payload;
      switch (payload.kind) {
        case 'stage': {
          // A stage beginning is also the signal that every earlier one is over; the pipeline
          // does not emit a separate "finished", and inferring it here beats adding one.
          steps = [
            ...steps.filter((s) => s.id !== payload.id).map((s) => ({ ...s, done: true })),
            {
              id: payload.id,
              label: payload.label,
              index: payload.index,
              total: payload.total,
              done: payload.id === 'done',
            },
          ];
          currentCommand = null;
          break;
        }
        case 'command_started':
          currentCommand = { argv: payload.argv, cwd: payload.cwd };
          break;
        case 'command_finished':
          currentCommand = null;
          break;
        case 'session_started':
          // The only moment the live transcript becomes attachable. Before this landed, the
          // session id arrived with the *return value* — after the run was already over.
          session = payload.session;
          break;
        case 'warning':
          notes = [
            ...notes,
            {
              id: `warn-${notes.length}-${payload.id}`,
              text: payload.message,
              kind: 'warning',
            },
          ];
          break;
        case 'note':
          notes = [
            ...notes,
            { id: `note-${notes.length}`, text: payload.message, kind: 'note' },
          ];
          break;
        default:
          // Lookup start/finish are already reflected in the review panel.
          break;
      }
    });
    return () => void unlisten.then((off) => off());
  });

  async function create() {
    phase = 'running';
    runError = null;
    steps = [];
    notes = [];
    currentCommand = null;
    session = null;
    try {
      const result = await commands.createWorktree({
        projectId,
        values: { ...values },
        adoptBranch,
        acknowledged: [...acknowledged],
        rows: 24,
        cols: 100,
      });
      outcome = result;
      // Normally `session_started` has already set this mid-run; these are the fallbacks for a
      // run that produced no session at all (no setup command, or a spawn that failed).
      if (result.kind === 'created') session ??= result.setupSession;
      if (result.kind === 'setup_failed') session ??= result.session;
      if (result.kind === 'cancelled') session ??= result.session;
      phase = 'done';
      await workspace.refreshWorktrees();
      // Select what was just made, so closing the dialog lands on it.
      if (result.kind !== 'cancelled' && result.worktree) {
        workspace.select(result.worktree.id);
      }
    } catch (e) {
      runError = errorMessage(e);
      phase = 'form';
    }
  }

  async function retrySetup() {
    const worktree = outcome && 'worktree' in outcome ? outcome.worktree : null;
    if (!worktree || acting) return;
    acting = true;
    runError = null;
    phase = 'running';
    notes = [];
    currentCommand = null;
    session = null;
    try {
      const result = await commands.runSetup({
        projectId,
        worktreeId: worktree.id,
        extraArgs: [],
        rows: 24,
        cols: 100,
      });
      session ??= result.session;
      if (result.success)
        outcome = { kind: 'created', worktree, setupSession: result.session };
      phase = 'done';
      await workspace.refreshWorktrees();
    } catch (e) {
      runError = errorMessage(e);
      phase = 'done';
    } finally {
      acting = false;
    }
  }

  function removeIt() {
    if (!removable || acting) return;
    confirmingRemove = true;
  }

  function onRemoveClosed() {
    confirmingRemove = false;
    const id = removable?.id;
    void workspace.refreshWorktrees().then(() => {
      if (id && !workspace.worktrees.some((w) => w.id === id)) onclose();
    });
  }

  function onKeydown(event: KeyboardEvent) {
    // Escape returns to the worktree list, but only while nothing is running — losing sight of
    // a live setup transcript would be worse than an extra click.
    if (event.key === 'Escape' && phase !== 'running') {
      event.stopPropagation();
      onclose();
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<section class="c-new-worktree" aria-label="New worktree">
  <header class="c-new-worktree__head">
    <div class="c-new-worktree__titles">
      <h1 class="c-pane-title">New Worktree</h1>
    </div>
    <Button variant="quiet" size="sm" onclick={onclose} disabled={phase === 'running'}
      >{phase === 'done' ? 'Done' : 'Cancel'}</Button
    >
  </header>

  <div class="c-new-worktree__body">
    {#if loadError}
      <p class="c-status--danger">{loadError}</p>
    {:else if phase === 'form'}
      {#if !form}
        <p class="c-status--muted">Loading the project's form…</p>
      {:else}
        <div class="c-new-worktree__columns">
          <div class="c-new-worktree__col">
            <h2 class="c-section-heading">Details</h2>

            <!--
              Adopting an existing branch takes the branch *and* the directory from that
              branch, so the naming templates never run and the fields feeding them have
              nothing left to affect. Without saying so, the form still looks like it is
              driving the outcome — you can watch `1234` normalize to `ACME-1234` beside a
              field that is no longer naming anything.

              Stated at the top and marked per field, because the two answer different
              questions: the banner says what changed, the dimming says which inputs. And it
              is careful to say the rest still apply — the fields that gate setup arguments
              very much do, and "the form is ignored" would be a dangerous thing to imply.
            -->
            {#if adoptBranch}
              <p class="c-new-worktree__adopting">
                Adopting <code>{adoptBranch}</code>. Its name and directory are used as they
                are, so the fields below no longer affect them — the rest still apply to
                setup.
              </p>
            {/if}

            <SchemaForm
              projectId={form.projectId}
              fields={form.fields}
              {values}
              problems={fieldProblems}
              normalized={preview?.normalized ?? {}}
              inert={adoptBranch ? (preview?.namingFields ?? []) : []}
              inertReason="Not used — the adopted branch supplies this."
            />
          </div>

          <div class="c-new-worktree__col c-new-worktree__review-col">
            <h2 class="c-section-heading">Review</h2>
            {#if previewError}
              <p class="c-status--danger">{previewError}</p>
            {:else if preview}
              <ReviewPanel
                {preview}
                {acknowledged}
                {adoptBranch}
                onadopt={(branch) => (adoptBranch = branch)}
              />
            {:else if previewing}
              <p class="c-status--muted">Planning…</p>
            {:else}
              <p class="c-status--muted">
                Fill in the required fields to see what will be created.
              </p>
            {/if}
          </div>
        </div>

        {#if runError}
          <p class="c-status--danger">{runError}</p>
        {/if}
      {/if}
    {:else}
      <!--
        The run occupies the body. Three things, top to bottom: where the pipeline is, what it
        is running right now, and the live transcript. The transcript mounts immediately — even
        before a session exists — because that is what lets it catch the first line of output
        rather than joining a run already in progress.
      -->
      <div class="c-new-worktree__run">
        <div class="c-new-worktree__run-head">
          {#if phase === 'running'}
            <h2 class="c-new-worktree__stage">{currentStep?.label ?? 'Starting…'}</h2>
            <span class="c-new-worktree__count">
              {#if currentStep}step {currentStep.index} of {currentStep.total}{/if}
            </span>
          {:else if outcome?.kind === 'created'}
            <h2 class="c-new-worktree__stage c-status--ok">
              Created <code>{outcome.worktree.dirname}</code>
            </h2>
            <span class="c-new-worktree__count"
              >on {outcome.worktree.branch ?? 'a detached HEAD'}</span
            >
          {:else if outcome?.kind === 'setup_failed'}
            <h2 class="c-new-worktree__stage c-status--warn">
              Setup {outcome.outcome.kind === 'timed_out' ? 'timed out' : 'failed'}
            </h2>
            <span class="c-new-worktree__count">the worktree was created and kept</span>
          {:else if outcome?.kind === 'cancelled'}
            <h2 class="c-new-worktree__stage c-status--warn">Cancelled</h2>
          {:else}
            <h2 class="c-new-worktree__stage">Finished</h2>
          {/if}
        </div>

        <div
          class="c-progress"
          class:is-indeterminate={phase === 'running' && !currentStep}
          role="progressbar"
          aria-valuenow={percent}
          aria-valuemin="0"
          aria-valuemax="100"
          aria-label="Create progress"
        >
          <div class="c-progress__fill" style:width="{percent}%"></div>
        </div>

        {#if steps.length > 0}
          <ol class="c-steps o-plain-list">
            {#each steps as step (step.id)}
              <li
                class="c-steps__item"
                class:is-done={step.done}
                class:is-active={!step.done && phase === 'running'}
              >
                <span class="c-steps__mark">
                  <Icon name={step.done ? 'check' : 'chevron-right'} size={12} />
                </span>
                <span>{step.label}</span>
              </li>
            {/each}
          </ol>
        {/if}

        {#if currentCommand}
          <div class="c-command">
            <code>{currentCommand.argv.join(' ')}</code>
            <span class="c-command__cwd">in {currentCommand.cwd}</span>
          </div>
        {/if}

        {#each notes as note (note.id)}
          <p class={note.kind === 'warning' ? 'c-status--warn' : 'c-status--muted'}>
            {note.text}
          </p>
        {/each}

        {#if outcome?.kind === 'setup_failed'}
          <p class="c-note">
            It has <strong>not</strong> been removed. Setup may already have written an environment
            file, allocated ports or cloned a database volume — deleting the worktree would leak
            those and lose work that is usually one command from fixed.
          </p>
          <div class="c-new-worktree__remedies">
            <Button
              variant="neutral"
              onclick={retrySetup}
              disabled={acting || confirmingRemove}>Re-run setup</Button
            >
            <Button
              variant="danger-outline"
              onclick={removeIt}
              disabled={acting || confirmingRemove}>Remove the worktree</Button
            >
          </div>
        {/if}

        {#if runError}
          <p class="c-status--danger">{runError}</p>
        {/if}

        <div class="c-new-worktree__terminal">
          <Terminal {session} />
        </div>
      </div>
    {/if}
  </div>

  <footer class="c-new-worktree__foot">
    <div class="o-row o-row--end">
      {#if phase === 'form'}
        <Button variant="accent" onclick={create} disabled={!canCreate || previewing}>
          {previewing ? 'Planning…' : 'Create worktree'}
        </Button>
      {:else if phase === 'running'}
        <Button
          variant="neutral"
          onclick={() => session && commands.ptyKill(session)}
          disabled={!session}
        >
          Cancel setup
        </Button>
      {:else}
        <Button variant="accent" onclick={onclose}>Back to worktrees</Button>
      {/if}
    </div>
  </footer>
</section>

{#if confirmingRemove && removable}
  <RemoveWorktreeDialog {projectId} worktree={removable} onclose={onRemoveClosed} />
{/if}
