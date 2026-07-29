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
  import SchemaForm from './SchemaForm.svelte';
  import Terminal from './Terminal.svelte';

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

  let phase = $state<Phase>('form');
  let adoptBranch = $state<string | null>(null);
  const acknowledged = $state<string[]>([]);

  let session = $state<string | null>(null);
  let outcome = $state<CreateOutcome | null>(null);
  let runError = $state<string | null>(null);

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
    void commands
      .worktreeForm(projectId)
      .then((f) => {
        form = f;
      })
      .catch((e) => {
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
          notes = [...notes, { id: payload.id, text: payload.message, kind: 'warning' }];
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
    if (!worktree) return;
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
    }
  }

  async function removeIt() {
    const worktree = outcome && 'worktree' in outcome ? outcome.worktree : null;
    if (!worktree) return;
    try {
      await commands.removeWorktree({
        projectId,
        worktreeId: worktree.id,
        deleteBranch: true,
        force: true,
        acknowledged: [],
      });
      await workspace.refreshWorktrees();
      onclose();
    } catch (e) {
      runError = errorMessage(e);
    }
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

<section class="pane" aria-label="New worktree">
  <header class="head">
    <div class="titles">
      <h1>New Worktree</h1>
    </div>
    <button class="back" onclick={onclose} disabled={phase === 'running'}>
      {phase === 'done' ? 'Done' : 'Cancel'}
    </button>
  </header>

  <div class="body">
    {#if loadError}
      <p class="error">{loadError}</p>
    {:else if phase === 'form'}
      {#if !form}
        <p class="muted">Loading the project's form…</p>
      {:else}
        <div class="columns">
          <div class="col form-col">
            <h2>Details</h2>

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
              <p class="adopting">
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

          <div class="col review-col">
            <h2>Review</h2>
            {#if previewError}
              <p class="error">{previewError}</p>
            {:else if preview}
              <ReviewPanel
                {preview}
                {acknowledged}
                {adoptBranch}
                onadopt={(branch) => (adoptBranch = branch)}
              />
            {:else if previewing}
              <p class="muted">Planning…</p>
            {:else}
              <p class="muted">Fill in the required fields to see what will be created.</p>
            {/if}
          </div>
        </div>

        {#if runError}
          <p class="error">{runError}</p>
        {/if}
      {/if}
    {:else}
      <!--
        The run occupies the body. Three things, top to bottom: where the pipeline is, what it
        is running right now, and the live transcript. The transcript mounts immediately — even
        before a session exists — because that is what lets it catch the first line of output
        rather than joining a run already in progress.
      -->
      <div class="run">
        <div class="runhead">
          {#if phase === 'running'}
            <h2 class="stage">{currentStep?.label ?? 'Starting…'}</h2>
            <span class="count">
              {#if currentStep}step {currentStep.index} of {currentStep.total}{/if}
            </span>
          {:else if outcome?.kind === 'created'}
            <h2 class="stage ok">
              Created <code>{outcome.worktree.dirname}</code>
            </h2>
            <span class="count">on {outcome.worktree.branch ?? 'a detached HEAD'}</span>
          {:else if outcome?.kind === 'setup_failed'}
            <h2 class="stage warn">
              Setup {outcome.outcome.kind === 'timed_out' ? 'timed out' : 'failed'}
            </h2>
            <span class="count">the worktree was created and kept</span>
          {:else if outcome?.kind === 'cancelled'}
            <h2 class="stage warn">Cancelled</h2>
          {:else}
            <h2 class="stage">Finished</h2>
          {/if}
        </div>

        <div
          class="bar"
          class:indeterminate={phase === 'running' && !currentStep}
          role="progressbar"
          aria-valuenow={percent}
          aria-valuemin="0"
          aria-valuemax="100"
          aria-label="Create progress"
        >
          <div class="fill" style:width="{percent}%"></div>
        </div>

        {#if steps.length > 0}
          <ol class="steps">
            {#each steps as step (step.id)}
              <li class:done={step.done} class:active={!step.done && phase === 'running'}>
                <span class="mark" aria-hidden="true">{step.done ? '✓' : '›'}</span>
                <span class="label">{step.label}</span>
              </li>
            {/each}
          </ol>
        {/if}

        {#if currentCommand}
          <div class="cmd">
            <code>{currentCommand.argv.join(' ')}</code>
            <span class="cwd">in {currentCommand.cwd}</span>
          </div>
        {/if}

        {#each notes as note (note.id)}
          <p class={note.kind === 'warning' ? 'warn' : 'muted'}>{note.text}</p>
        {/each}

        {#if outcome?.kind === 'setup_failed'}
          <p class="note">
            It has <strong>not</strong> been removed. Setup may already have written an environment
            file, allocated ports or cloned a database volume — deleting the worktree would leak
            those and lose work that is usually one command from fixed.
          </p>
          <div class="remedies">
            <button class="secondary" onclick={retrySetup}>Re-run setup</button>
            <button class="secondary danger" onclick={removeIt}>Remove the worktree</button>
          </div>
        {/if}

        {#if runError}
          <p class="error">{runError}</p>
        {/if}

        <div class="termwrap">
          <Terminal {session} />
        </div>
      </div>
    {/if}
  </div>

  <footer>
    <div class="actions">
      {#if phase === 'form'}
        <button class="primary" onclick={create} disabled={!canCreate || previewing}>
          {previewing ? 'Planning…' : 'Create worktree'}
        </button>
      {:else if phase === 'running'}
        <button
          class="secondary"
          onclick={() => session && commands.ptyKill(session)}
          disabled={!session}
        >
          Cancel setup
        </button>
      {:else}
        <button class="primary" onclick={onclose}>Back to worktrees</button>
      {/if}
    </div>
  </footer>
</section>

<style>
  .pane {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    background: var(--bg);
  }

  .head {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    padding: var(--sp-4) var(--sp-5) var(--sp-3);
  }

  .titles {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }

  h1 {
    font-size: var(--step-2);
    font-weight: 600;
    letter-spacing: -0.01em;
  }

  h2 {
    font-size: var(--step--1);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-muted);
    margin-bottom: var(--sp-3);
  }

  /* The run view. Fills the body so the transcript gets the room it needs. */
  .run {
    flex: 1 1 auto;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }

  .runhead {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--sp-3);
    flex-wrap: wrap;
  }

  .stage {
    font-size: var(--step-1);
    font-weight: 600;
    text-transform: none;
    letter-spacing: -0.01em;
    color: var(--fg);
    margin: 0;
  }

  .stage code {
    font-size: inherit;
  }

  .count {
    font-size: var(--step--2);
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }

  .bar {
    flex: 0 0 auto;
    height: 4px;
    border-radius: 999px;
    background: var(--bg-active);
    overflow: hidden;
  }

  .fill {
    height: 100%;
    background: var(--accent);
    border-radius: inherit;
    transition: width var(--dur-slow) var(--ease);
  }

  /* Before the first stage lands there is no honest percentage to show, so sweep instead of
     claiming zero progress. */
  .bar.indeterminate .fill {
    width: 35% !important;
    animation: sweep 1.4s var(--ease) infinite;
  }

  @keyframes sweep {
    0% {
      transform: translateX(-100%);
    }
    100% {
      transform: translateX(340%);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .bar.indeterminate .fill {
      animation: none;
    }
    .fill {
      transition: none;
    }
  }

  .steps {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: var(--step--1);
    list-style: none;
  }

  .steps li {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    color: var(--fg-muted);
  }

  .steps li.active {
    color: var(--fg);
    font-weight: 500;
  }

  .steps .mark {
    width: 1em;
    flex: 0 0 auto;
    text-align: center;
  }

  .steps li.done .mark {
    color: var(--ok);
  }

  .steps li.active .mark {
    color: var(--accent);
  }

  .cmd {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: var(--sp-2) var(--sp-3);
    background: var(--bg-code);
    border: 1px solid var(--border);
    border-radius: var(--r-md);
  }

  .cmd code {
    font-size: var(--step--2);
    overflow-wrap: anywhere;
  }

  .cwd {
    font-size: var(--step--2);
    color: var(--fg-muted);
  }

  .back {
    font-size: var(--step--1);
    color: var(--fg-muted);
    padding: 4px 8px;
    border-radius: var(--r-md);
  }

  .back:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .back:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .body {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    padding: 0 var(--sp-5) var(--sp-4);
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }

  /* Form beside review, so what you type and what it produces are visible together — the
     thing a modal could not do. Collapses to one column when the window is narrow. */
  .columns {
    display: grid;
    grid-template-columns: minmax(280px, 1fr) minmax(300px, 1.1fr);
    gap: var(--sp-6);
    align-items: start;
  }

  @media (max-width: 900px) {
    .columns {
      grid-template-columns: 1fr;
      gap: var(--sp-5);
    }
  }

  .col {
    min-width: 0;
  }

  /* Info-toned rather than warning-toned: adopting a branch is a normal choice, not a
     mistake. It only needs to be legible, not alarming. */
  .adopting {
    margin-bottom: var(--sp-3);
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid color-mix(in oklab, var(--info) 35%, transparent);
    border-radius: var(--r-md);
    background: color-mix(in oklab, var(--info) 8%, transparent);
    color: var(--fg-muted);
    font-size: var(--step--2);
    line-height: 1.5;
  }

  .adopting code {
    color: var(--fg);
  }

  .review-col {
    border-left: 1px solid var(--border);
    padding-left: var(--sp-5);
  }

  @media (max-width: 900px) {
    .review-col {
      border-left: none;
      padding-left: 0;
      border-top: 1px solid var(--border);
      padding-top: var(--sp-4);
    }
  }

  footer {
    flex: 0 0 auto;
    border-top: 1px solid var(--border);
    padding: var(--sp-3) var(--sp-5);
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--sp-2);
  }

  .actions button,
  .remedies button {
    padding: 7px 14px;
    border-radius: var(--r-md);
    font-size: var(--step--1);
    font-weight: 500;
  }

  .secondary {
    border: 1px solid var(--border-strong);
    color: var(--fg);
  }

  .secondary:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  /* Same treatment as the detail pane's Remove button — one look for "this destroys
     something", so the two cannot drift into meaning different things. */
  .secondary.danger {
    color: var(--danger);
    border-color: color-mix(in oklab, var(--danger) 40%, transparent);
    background: color-mix(in oklab, var(--danger) 8%, transparent);
  }

  .secondary.danger:hover:not(:disabled) {
    background: var(--danger);
    border-color: var(--danger);
    color: var(--fg-on-accent);
  }

  .primary {
    background: var(--accent);
    color: var(--fg-on-accent);
  }

  .primary:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .primary:disabled,
  .secondary:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .remedies {
    display: flex;
    gap: var(--sp-2);
  }

  /* Takes the remaining height rather than a fixed box: the transcript is the thing you watch,
     and it should grow with the window instead of scrolling inside a short frame. */
  .termwrap {
    flex: 1 1 auto;
    min-height: 240px;
    display: flex;
    flex-direction: column;
  }

  .note {
    font-size: var(--step--2);
    color: var(--fg-muted);
    line-height: 1.6;
    max-width: 76ch;
  }

  .error {
    color: var(--danger);
    font-size: var(--step--1);
  }
  .ok {
    color: var(--ok);
    font-size: var(--step--1);
  }
  .warn {
    color: var(--warn);
    font-size: var(--step--1);
  }
  .muted {
    color: var(--fg-muted);
    font-size: var(--step--1);
  }
</style>
