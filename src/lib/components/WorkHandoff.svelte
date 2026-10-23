<script lang="ts">
  /**
   * The hand-over panel: what an implementation left for you to test, and Revise.
   *
   * Evidence first, in the order you would test it: what was done, round by round in the writer's
   * words; each acceptance criterion with the writer's word on it and the findings that cite it;
   * the checks the app ran, with the end of what they printed; what the loop left and why; the
   * diff; and the environment's controls, to try it. Nothing here says a criterion is met — the
   * writer's word is labelled as its word, beside the checks and the review that are the evidence.
   *
   * # When the evidence no longer describes the worktree
   *
   * An edit by hand, or a commit since, changes what the checks and the review saw. Rust works out
   * which (`handoff_view`, from R6's staleness), and the panel says so above everything, marking
   * each check whose paths the change touched, because a pass on another tree is not a pass.
   *
   * # Revise
   *
   * Your words go to the writer that did the work, in its conversation, and the run goes back to
   * Implementing; `canRevise` is Rust's answer to whether it may, and a refusal says why.
   */
  import { untrack } from 'svelte';

  import {
    CHECK_TONE,
    CHECK_WORDS,
    leftWords,
    roundWords,
    shortSha,
    stopWords,
  } from '../implement';
  import type { ImplementView } from '../ipc/types';
  import { environments } from '../state/environments.svelte';
  import { implement } from '../state/implement.svelte';
  import CodeText from './CodeText.svelte';
  import WorkEnvControl from './WorkEnvControl.svelte';
  import Button from './ui/Button.svelte';

  const {
    projectId,
    issueKey,
    view,
    visible,
    onopenworktree,
  }: {
    projectId: string;
    issueKey: string;
    view: ImplementView;
    visible: boolean;
    onopenworktree: (worktreeId: string) => void;
  } = $props();

  const flowId = $derived(view.flow.id);
  // Read while on screen: the diff and the evidence come from the worktree, not the record.
  $effect(() => {
    const flow = flowId;
    const key = issueKey;
    if (!visible) return;
    untrack(() => void implement.loadHandoff(key, flow));
    return () => untrack(() => implement.forgetHandoff(key));
  });

  const handoff = $derived(implement.handoffs[issueKey] ?? null);
  const worktreeId = $derived(handoff?.worktree ?? null);
  $effect(() => {
    const id = worktreeId;
    if (visible && id) untrack(() => void environments.load(projectId, id));
  });
  const environment = $derived(
    worktreeId ? (environments.views[worktreeId] ?? null) : null,
  );

  const implementation = $derived(view.implementation);
  const handedIn = $derived([...view.rounds].reverse().filter((r) => r.handoff !== null));
  const busy = $derived(implement.busy[issueKey] ?? false);

  let note = $state('');

  async function revise(): Promise<void> {
    if (await implement.revise(projectId, issueKey, flowId, note)) note = '';
  }
</script>

<section class="c-handoff" aria-labelledby="handoff-title">
  <header class="c-handoff__head">
    <h3 class="c-implement__label" id="handoff-title">The hand-over</h3>
    {#if environment?.declared && worktreeId}
      <WorkEnvControl
        {projectId}
        {worktreeId}
        view={environment}
        {onopenworktree}
        onlogs={() => onopenworktree(worktreeId)}
      />
    {/if}
  </header>
  {#if implementation?.stopped}
    <p class="c-handoff__lead">{stopWords(implementation.stopped)}</p>
  {/if}

  {#if handoff?.since}
    <p class="c-handoff__stale" role="note">
      <CodeText
        text={`Changed since the checks ran and the review read it: ${handoff.since.paths
          .map((p) => `\`${p}\``)
          .join(
            ', ',
          )}${handoff.since.complete ? '' : ', and more'}. What follows describes the tree before; Revise sends your edits with the revision, and the checks they touch run again.`}
      />
    </p>
  {/if}

  <section class="c-handoff__part" aria-labelledby="handoff-done">
    <h4 class="c-handoff__heading" id="handoff-done">What was done</h4>
    <ol class="o-plain-list c-handoff__rounds">
      {#each handedIn as round (round.round)}
        <li class="c-handoff__round">
          <p class="c-handoff__round-head">
            <strong>Round {round.round}</strong> · {roundWords(round)}
          </p>
          {#if round.note}
            <p class="c-handoff__asked"><CodeText text={`You asked: ${round.note}`} /></p>
          {/if}
          <p class="c-handoff__summary"><CodeText text={round.handoff?.summary ?? ''} /></p>
        </li>
      {/each}
    </ol>
  </section>

  {#if handoff && handoff.coverage.length > 0}
    <section class="c-handoff__part" aria-labelledby="handoff-criteria">
      <h4 class="c-handoff__heading" id="handoff-criteria">Acceptance criteria</h4>
      <p class="c-handoff__note">
        The writer's word on each, beside the review's findings that cite it. Its word is a
        claim: the checks and the review are the evidence.
      </p>
      <ol class="o-plain-list c-handoff__criteria">
        {#each handoff.coverage as criterion (criterion.criterion)}
          <li class="c-handoff__criterion">
            <p>
              <strong>{criterion.criterion}.</strong>
              <CodeText text={criterion.text} />
            </p>
            {#if criterion.claim}
              <p class="c-handoff__claim">
                <span class="c-status--{criterion.claim.met ? 'ok' : 'warn'}"
                  >The writer says {criterion.claim.met ? 'met' : 'not met'}</span
                >
                — <CodeText text={criterion.claim.evidence} />
              </p>
            {:else}
              <p class="c-handoff__claim c-status--muted">
                The writer said nothing about it.
              </p>
            {/if}
            {#if criterion.findings.length > 0}
              <p class="c-handoff__claim c-status--danger">
                Cited by {criterion.findings.map((f) => `F${f}`).join(', ')}, still standing
              </p>
            {/if}
          </li>
        {/each}
      </ol>
    </section>
  {/if}

  {#if handoff}
    <section class="c-handoff__part" aria-labelledby="handoff-checks">
      <h4 class="c-handoff__heading" id="handoff-checks">Checks the app ran</h4>
      {#if handoff.checks.length === 0}
        <p class="c-handoff__note">No checks are configured for implementing.</p>
      {:else}
        <ul class="o-plain-list c-handoff__checks">
          {#each handoff.checks as check (check.run.id)}
            <li class="c-handoff__check" data-status={check.run.status}>
              <span class="c-handoff__check-head">
                <span class="u-mono">{check.run.check}</span>
                <span class="c-status--{CHECK_TONE[check.run.status]}"
                  >{CHECK_WORDS[check.run.status]}</span
                >
                {#if check.run.exitCode !== null}
                  <span class="c-handoff__why">exit {check.run.exitCode}</span>
                {/if}
                {#if check.stale}
                  <span class="c-badge">stale: changed since</span>
                {/if}
              </span>
              {#if check.tail}
                <details class="c-handoff__read">
                  <summary>What it printed</summary>
                  <pre class="c-handoff__output">{check.tail}</pre>
                </details>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </section>
  {/if}

  {#if implementation && implementation.remaining.length > 0}
    <section class="c-handoff__part" aria-labelledby="handoff-left">
      <h4 class="c-handoff__heading" id="handoff-left">Left for you</h4>
      <ul class="o-plain-list c-implement__left">
        {#each implementation.remaining as left (left.finding)}
          <li>
            <strong>F{left.finding}</strong>
            <CodeText text={left.title} />
            {#if left.location}
              <span class="u-mono c-implement__place"
                >{left.location.path}:{left.location.startLine}</span
              >
            {/if}
            <span class="c-implement__why">— {leftWords(left.because)}</span>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  {#if handoff}
    <section class="c-handoff__part" aria-labelledby="handoff-diff">
      <h4 class="c-handoff__heading" id="handoff-diff">The change</h4>
      <p class="c-handoff__note">
        {handoff.files.length} file{handoff.files.length === 1 ? '' : 's'} from
        <code>{shortSha(handoff.baseSha)}</code>
        to <code>{shortSha(handoff.headSha)}</code>{#if handoff.branch}{' '}on
          <code>{handoff.branch}</code>{/if}.
      </p>
      <ul class="o-plain-list c-handoff__files" aria-label="Files">
        {#each handoff.files as file (file.path)}
          <li class="c-handoff__file">
            <code>{file.path}</code>
            {#if file.added !== null}<span class="c-handoff__added">+{file.added}</span
              >{/if}
            {#if file.removed !== null}<span class="c-handoff__removed"
                >−{file.removed}</span
              >{/if}
          </li>
        {/each}
      </ul>
      {#if handoff.uncommitted.length > 0}
        <p class="c-handoff__note">
          <CodeText
            text={`Not committed: ${handoff.uncommitted.map((p) => `\`${p}\``).join(', ')}.`}
          />
        </p>
      {/if}
      {#if handoff.patch}
        <details class="c-handoff__read">
          <summary>Read the diff</summary>
          <pre class="c-handoff__patch">{handoff.patch}</pre>
          {#if handoff.truncated}
            <p class="c-handoff__note">Cut short here.</p>
          {/if}
        </details>
      {/if}
    </section>

    <form
      class="c-handoff__revise"
      onsubmit={(event) => {
        event.preventDefault();
        void revise();
      }}
    >
      <label class="c-handoff__field">
        What should change?
        <textarea class="c-textarea" rows="3" bind:value={note}></textarea>
      </label>
      <div class="c-handoff__row">
        <Button
          variant="accent"
          type="submit"
          disabled={busy || !handoff.canRevise || note.trim() === ''}>Revise</Button
        >
        <span class="c-handoff__note"
          >Goes back to <span class="u-mono">{implementation?.role}</span>, in its same
          conversation; the checks and the review run again.</span
        >
      </div>
    </form>
  {/if}
</section>
