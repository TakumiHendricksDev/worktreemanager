<script lang="ts">
  /**
   * Fix the findings of a review of your own work: pick them and a writer, and later read what the
   * writer committed.
   *
   * # Two moments, one sheet
   *
   * `pick` is the question: which of the standing findings that name a file, and which writer role.
   * Starting closes the sheet, because the Reviews tab is where the writer's progress and Stop fix
   * live. `read` is the answer: the files the fix changed, anything it left uncommitted, and the
   * patch, from the same diff a teammate's fix is read by.
   *
   * # What is not here
   *
   * A teammate's pull request fixes from its outcome sheet (`WorkPrOutcome`) and ends in a route
   * you approve, a destination and a diff pinned by an approval. Your own work has no destination:
   * the review worktree is yours, so the app's commits land on your branch and the fix is done when
   * you have read them. There is therefore nothing to approve and no preview — only a diff to read,
   * and Review the fix, which is a new review of the worktree as it now stands.
   *
   * # Refusals are the app's
   *
   * Whether a fix can start is Rust's to say (an implementation that is still open owns its own
   * fix loop; uncommitted work is not a clean start). This sheet shows the refusal where the click
   * was, and changes nothing.
   */
  import { untrack } from 'svelte';

  import type { FixDiffView, ReviewRunView, StartFix } from '../ipc/types';
  import { isWtmError } from '../ipc/types';
  import { SEVERITY_WORDS } from '../knowledge';
  import { short } from '../pr-review';
  import { currentFix, fixableFindings, fixToSend, leaseLine } from '../local-fix';
  import { findingPlace, pickedByDefault } from '../publish';
  import CodeText from './CodeText.svelte';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';

  const {
    view,
    mode,
    onclose,
    onstart,
    ondiff,
    onreview,
  }: {
    view: ReviewRunView;
    mode: 'pick' | 'read';
    onclose: () => void;
    /** Start the fix. Rejects with the app's error. */
    onstart: (request: StartFix) => Promise<void>;
    /** The current fix's diff. Rejects with the app's error. */
    ondiff: () => Promise<FixDiffView>;
    /** Review the worktree again, as it now stands. */
    onreview: () => void;
  } = $props();

  const fixable = $derived(fixableFindings(view));
  const last = $derived(currentFix(view));
  const ready = $derived(fixToSend(view));
  const lease = $derived(leaseLine(view.leases));

  // Read once, when the sheet opens; what you pick is yours from then on.
  let picks = $state<Record<number, boolean>>(
    untrack(() => Object.fromEntries(fixable.map((f) => [f.id, pickedByDefault(f)]))),
  );
  let writer = $state(untrack(() => view.writers[0]?.id ?? ''));
  const request = $derived<StartFix>({
    findings: fixable.filter((f) => picks[f.id]).map((f) => f.id),
    role: writer,
  });

  let working = $state(false);
  let problem = $state<string | null>(null);
  let diff = $state<FixDiffView | null>(null);

  function said(error: unknown): string {
    return isWtmError(error) ? error.message : String(error);
  }

  async function start(event: Event): Promise<void> {
    event.preventDefault();
    if (mode !== 'pick' || working) return;
    working = true;
    problem = null;
    try {
      await onstart($state.snapshot(request));
      onclose();
    } catch (error) {
      problem = said(error);
    } finally {
      working = false;
    }
  }

  // The diff is read once, when the sheet opens on a ready fix.
  $effect(() => {
    if (mode !== 'read') return;
    untrack(() => {
      ondiff().then(
        (read) => (diff = read),
        (error: unknown) => (problem = said(error)),
      );
    });
  });
</script>

<Dialog
  title={mode === 'pick' ? 'Fix findings' : 'What the fix changed'}
  {onclose}
  onsubmit={start}
  closeDisabled={working}
  wide
>
  {#snippet body()}
    <div class="c-fix-sheet">
      {#if mode === 'pick'}
        {#if last && last.state !== 'ready'}
          <p class="c-fix-sheet__note">
            The last fix {last.state === 'failed'
              ? 'did not finish'
              : 'was stopped'}{last.detail ? `: ${last.detail}` : '.'} Whatever it committed is
            on your branch.
          </p>
        {/if}
        {#if lease}
          <p class="c-fix-sheet__note"><CodeText text={lease} /></p>
        {/if}
        <fieldset class="c-fix-sheet__picks">
          <legend class="c-publication__legend">Findings to fix</legend>
          {#each fixable as finding (finding.id)}
            <label class="c-fix-sheet__pick">
              <input type="checkbox" bind:checked={picks[finding.id]} disabled={working} />
              <span
                class="c-publication__severity"
                class:is-blocking={finding.severity === 'blocking'}
                class:is-notable={finding.severity === 'notable'}
                >{SEVERITY_WORDS[finding.severity]}</span
              >
              <code>{findingPlace(finding)}</code>
              <CodeText text={finding.title} />
            </label>
          {/each}
        </fieldset>
        {#if view.findings.some((f) => f.dismissed === null && f.location === null)}
          <p class="c-fix-sheet__note">
            Findings about the change as a whole name no file to hold a writer to, so they
            are not listed.
          </p>
        {/if}
        {#if view.writers.length > 0}
          <label class="c-fix-sheet__field">
            <span class="c-fix-sheet__label">Writer</span>
            <select class="c-select" bind:value={writer} disabled={working}>
              {#each view.writers as choice (choice.id)}
                <option value={choice.id}
                  >{choice.id} · {choice.provider}{choice.model
                    ? ` ${choice.model}`
                    : ''}</option
                >
              {/each}
            </select>
          </label>
          <p class="c-fix-sheet__note">
            The writer may change only the picked findings' files. It cannot commit or push:
            wtm commits for it on your branch, with your git config and signing and with
            hooks off, and a file outside the scope becomes a question in the inbox. Start
            from a clean worktree at what was reviewed, so the diff is the writer's alone.
          </p>
        {:else}
          <p class="c-fix-sheet__note">
            No writer role yet: add a <code>[workflow.role.&lt;id&gt;]</code> with
            <code>profile = "writer"</code> to your own config.
          </p>
        {/if}
      {:else if !diff && !problem}
        <p class="c-fix-sheet__note">Reading the fix…</p>
      {:else if diff}
        <p class="c-local-fix__head">
          <code>{diff.fix.role}</code> fixed {diff.fix.findings.length === 1
            ? '1 finding'
            : `${diff.fix.findings.length} findings`} as <code>{short(diff.head)}</code> on
          <code>{short(diff.base)}</code>
        </p>
        <ul class="o-plain-list c-local-fix__files" aria-label="Files">
          {#each diff.files as file (file.path)}
            <li class="c-local-fix__file">
              <code>{file.path}</code>
              {#if file.added !== null}<span class="c-local-fix__added">+{file.added}</span
                >{/if}
              {#if file.removed !== null}<span class="c-local-fix__removed"
                  >−{file.removed}</span
                >{/if}
            </li>
          {/each}
        </ul>
        {#if diff.uncommitted.length > 0}
          <p class="c-local-fix__left" role="note">
            <CodeText
              text={`Not committed: ${diff.uncommitted.map((p) => `\`${p}\``).join(', ')} ${
                diff.outliers.length > 0
                  ? "— outside the findings' files, waiting on your answer in the inbox."
                  : 'left uncommitted by the writer.'
              }`}
            />
          </p>
        {/if}
        <details class="c-local-fix__read" open>
          <summary>The diff</summary>
          <pre class="c-local-fix__patch">{diff.patch}</pre>
          {#if diff.truncated}
            <p class="c-fix-sheet__note">
              Cut short here; <code>git diff</code> has all of it.
            </p>
          {/if}
        </details>
      {/if}
      {#if problem}
        <p class="c-work-state__error" role="alert"><CodeText text={problem} /></p>
      {/if}
    </div>
  {/snippet}
  {#snippet footer()}
    {#if mode === 'pick'}
      <p class="c-fix-sheet__summary">
        Nothing leaves this Mac: the fix is committed on your branch, for you to read.
      </p>
      <Button onclick={onclose} disabled={working}>Cancel</Button>
      <Button
        variant="accent"
        type="submit"
        disabled={working ||
          request.findings.length === 0 ||
          !request.role ||
          view.leases.length > 0}
      >
        {working ? 'Starting…' : 'Start the fix'}
      </Button>
    {:else}
      <p class="c-fix-sheet__summary">
        {ready
          ? 'It is on your branch. Review it again to see whether the findings are gone.'
          : ''}
      </p>
      <Button onclick={onclose}>Close</Button>
      <Button
        variant="accent"
        disabled={ready === null}
        onclick={() => {
          onreview();
          onclose();
        }}>Review the fix</Button
      >
    {/if}
  {/snippet}
</Dialog>
