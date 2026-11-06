<script lang="ts">
  import ExtensionPoint from './ExtensionPoint.svelte';
  import { extensionPart, extensionContext } from '../state/extensions.svelte';
  const publicContext = extensionContext();
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
   * 3. **A failed setup is not an error.** The worktree exists; its own view carries the
   *    transcript and the remedies (`CreationRecord`) rather than throwing away what was built.
   *
   * # The run is not this pane's
   *
   * Pressing Create hands the run to `creations`, which keeps its steps, command and terminal
   * for as long as it takes; this pane only draws the one `creations.shown` names. So leaving for
   * another worktree mid-setup loses nothing: the run's ghost tab in the list carries on, and
   * clicking it brings this screen back as it was. A run refused before git made anything comes
   * back here as its form, filled in as it was, with the reason.
   */
  import { commands } from '../ipc/commands';
  import { errorMessage, isWtmError, type Form, type Preview } from '../ipc/types';
  import { untrack } from 'svelte';
  import { creations, currentStep, madeWorktree } from '../state/creations.svelte';
  import { workspace } from '../state/workspace.svelte';
  import CreationRecord from './CreationRecord.svelte';
  import ReviewPanel from './ReviewPanel.svelte';
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

  /** The creation on display, or none for a blank form. */
  const creation = $derived(creations.byId(creations.shown));
  /** Whether this screen is a form: a blank one, or a refused creation's, to try again. */
  const editing = $derived(!creation || creation.phase === 'refused');

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

  let adoptBranch = $state<string | null>(null);
  const acknowledged = $state<string[]>([]);

  /*
   * What the form should hold: blank for a new worktree, or a refused creation's own values so
   * trying again is one click. A running creation leaves the form alone — it is not on screen, and a
   * run's progress must not reset anything. Keyed on this rather than on the id alone because the
   * same creation goes from running to refused without the id changing.
   */
  const formKey = $derived(
    creations.shown === null
      ? 'blank'
      : creation?.phase === 'refused'
        ? `refused:${creation.id}`
        : 'run',
  );
  $effect(() => {
    const key = formKey;
    if (key === 'run') return;
    untrack(() => {
      const refused = key === 'blank' ? undefined : creation;
      for (const field of Object.keys(values)) delete values[field];
      acknowledged.length = 0;
      Object.assign(values, refused?.values ?? {});
      acknowledged.push(...(refused?.acknowledged ?? []));
      adoptBranch = refused?.adoptBranch ?? null;
      preview = null;
    });
  });

  const step = $derived(creation ? currentStep(creation) : null);
  const percent = $derived(
    step && step.total > 0 ? Math.round(((step.index - 1) / step.total) * 100) : 0,
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
    if (!editing || !form) return;
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

  /** Hand the run to `creations`, and show it. A refused creation is retried under its own id. */
  function create() {
    if (!preview) return;
    const id = creations.start({
      projectId,
      values: { ...values },
      adoptBranch,
      acknowledged: [...acknowledged],
      preview,
      retry: creation?.phase === 'refused' ? creation.id : null,
    });
    creations.shown = id;
  }

  function onKeydown(event: KeyboardEvent) {
    // Escape returns to the worktree list. Even mid-run now: the run is `creations`', and its tab
    // brings this screen back. A dialog mounted over this pane owns Escape while it is open, or
    // sibling window listeners would close both on the same keypress.
    if (event.key === 'Escape' && !document.querySelector('[aria-modal="true"]')) {
      event.stopPropagation();
      onclose();
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />
<ExtensionPoint target="component.new-worktree-pane" context={publicContext()}>
  <section
    use:extensionPart={{ target: 'component.new-worktree-pane', context: publicContext }}
    class="c-new-worktree"
    aria-label="New worktree"
  >
    <header class="c-new-worktree__head">
      <div class="c-new-worktree__titles">
        <h1 class="c-pane-title">New Worktree</h1>
      </div>
      {#if creation?.phase === 'running'}
        <span class="c-new-worktree__hint">
          <Icon name="chevron-right" size={12} /> Keeps going if you switch worktrees — its tab
          in the list brings you back here
        </span>
      {/if}
      <Button variant="quiet" size="sm" onclick={onclose}
        >{editing ? 'Cancel' : 'Close'}</Button
      >
    </header>

    <div class="c-new-worktree__body">
      {#if loadError}
        <p class="c-status--danger">{loadError}</p>
      {:else if editing}
        {#if creation?.phase === 'refused'}
          <!-- Nothing was made, so this is the form again, as it was, with why. -->
          <div class="c-new-worktree__refused" role="alert">
            <Icon name="warn" size={14} />
            <p>
              <strong>Couldn’t create it.</strong>
              {creation.error ?? 'The run stopped before git made anything.'} Change the form
              and try again, or discard it with × on its tab.
            </p>
          </div>
        {/if}
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
                  Adopting <code>{adoptBranch}</code>. Its name and directory are used as
                  they are, so the fields below no longer affect them — the rest still apply
                  to setup.
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
        {/if}
      {:else if creation && creation.phase !== 'running' && madeWorktree(creation)}
        <!-- Finished while on screen in a project not in view, so it could not be gone to: its record
           here, the same one its worktree's view shows. -->
        <CreationRecord {creation} />
      {:else if creation}
        <!--
        The run occupies the body. Three things, top to bottom: where the pipeline is, what it
        is running right now, and the live transcript. The transcript mounts immediately — even
        before a session exists — because that is what lets it catch the first line of output
        rather than joining a run already in progress; and on coming back mid-run it replays what
        the session has printed so far.
      -->
        <div class="c-new-worktree__run">
          <div class="c-new-worktree__run-head">
            <h2 class="c-new-worktree__stage">{step?.label ?? 'Starting…'}</h2>
            <span class="c-new-worktree__count">
              {#if step}step {step.index} of {step.total} ·{/if}
              <code>{creation.dirname}</code>
            </span>
          </div>

          <div
            class="c-progress"
            class:is-indeterminate={!step}
            role="progressbar"
            aria-valuenow={percent}
            aria-valuemin="0"
            aria-valuemax="100"
            aria-label="Create progress"
          >
            <div class="c-progress__fill" style:width="{percent}%"></div>
          </div>

          {#if creation.steps.length > 0}
            <ol class="c-steps o-plain-list">
              {#each creation.steps as item (item.id)}
                <li
                  class="c-steps__item"
                  class:is-done={item.done}
                  class:is-active={!item.done}
                >
                  <span class="c-steps__mark">
                    <Icon name={item.done ? 'check' : 'chevron-right'} size={12} />
                  </span>
                  <span>{item.label}</span>
                </li>
              {/each}
            </ol>
          {/if}

          {#if creation.command}
            <div class="c-command">
              <code>{creation.command.argv.join(' ')}</code>
              <span class="c-command__cwd">in {creation.command.cwd}</span>
            </div>
          {/if}

          {#each creation.notes as note (note.id)}
            <p class={note.kind === 'warning' ? 'c-status--warn' : 'c-status--muted'}>
              {note.text}
            </p>
          {/each}

          <div class="c-new-worktree__terminal">
            {#key creation.id}
              <Terminal session={creation.session} />
            {/key}
          </div>
        </div>
      {/if}
    </div>

    <footer class="c-new-worktree__foot">
      <div class="o-row o-row--end">
        {#if editing}
          <Button variant="accent" onclick={create} disabled={!canCreate || previewing}>
            {previewing ? 'Planning…' : creation ? 'Try again' : 'Create worktree'}
          </Button>
        {:else if creation?.phase === 'running'}
          <Button
            variant="neutral"
            onclick={() => creation?.session && commands.ptyKill(creation.session)}
            disabled={!creation.session}
          >
            Cancel setup
          </Button>
        {:else}
          <Button variant="accent" onclick={onclose}>Back to worktrees</Button>
        {/if}
      </div>
    </footer>
  </section>
</ExtensionPoint>
