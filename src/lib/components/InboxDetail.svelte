<script lang="ts">
  /**
   * One inbox item, open: what it is about and the one or two things to do about it.
   *
   * A question gets its own card (`InboxQuestion`). Everything else is this card, told apart by
   * kind:
   *
   * - **A failure, a usage pause or an interruption** offers the run's own Resume, worded for what
   *   it does here, and a way to the role or run it is about. Resuming is what settles the item;
   *   the engine resolves it and the list follows.
   * - **An approval** is a run session's provider asking for a permission. While that session is
   *   alive its prompt is answerable right here, with the same card a pane draws. Once the session
   *   has ended there is nothing left to answer, and the card says so rather than offering a
   *   button that does nothing.
   * - **A ready review** names the worst of what it found; each finding is a link to that finding
   *   in the Reviews tab, which is the exact place the item is about. Reading the review settles
   *   it.
   */
  import { untrack } from 'svelte';

  import { KIND_TONE, KIND_WORDS, filedAt, resumeWords, showWords } from '../inbox';
  import { commands } from '../ipc/commands';
  import type { FindingRecord, InboxItem } from '../ipc/types';
  import { GROUP_WORDS, findingGroups } from '../review-run';
  import { inbox } from '../state/inbox.svelte';
  import { reviewRuns } from '../state/review-runs.svelte';
  import { sessions } from '../state/sessions.svelte';
  import CodeText from './CodeText.svelte';
  import ApprovalCard from './ApprovalCard.svelte';
  import Button from './ui/Button.svelte';

  const { item }: { item: InboxItem } = $props();

  const busy = $derived(inbox.busy[item.id] ?? false);
  const error = $derived(inbox.errors[item.id] ?? null);
  const resume = $derived(resumeWords(item));
  const resuming = $derived(
    item.worktree ? (reviewRuns.busy[item.worktree] ?? false) : false,
  );
  const resumeError = $derived(
    item.worktree ? (reviewRuns.errors[item.worktree] ?? null) : null,
  );

  /** The run session whose provider is asking, while it is still open. */
  const asking = $derived.by(() => {
    if (item.kind !== 'approval' || item.target.kind !== 'step') return null;
    const role = item.target.role;
    const pane = sessions.panes.find(
      (p) => p.flow?.flow === item.flow && p.flow.role === role && p.approvals.length > 0,
    );
    const approval = pane?.approvals[0];
    return pane && approval ? { pane, approval } : null;
  });

  /** A ready review's findings, still standing, worst class first. Read when it is opened. */
  let findings = $state<FindingRecord[] | null>(null);
  $effect(() => {
    const flow = item.kind === 'review_ready' ? item.flow : null;
    untrack(() => {
      findings = null;
      if (flow === null) return;
      void commands
        .reviewRun(flow)
        .then((run) => {
          if (item.flow === flow) findings = run.findings;
        })
        .catch(() => {
          findings = [];
        });
    });
  });
  const groups = $derived(findings ? findingGroups(findings) : []);

  async function answered(reply: Parameters<typeof sessions.answer>[2]): Promise<void> {
    if (!asking) return;
    await sessions.answer(asking.pane.id, asking.approval.id, reply);
    // Answered in its pane's own words: the prompt it was about is settled.
    await inbox.resolve(item);
  }

  function place(finding: FindingRecord): string {
    const at = finding.location;
    if (!at) return 'the whole change';
    return finding.lines ? `${at.path}:${at.startLine}` : at.path;
  }
</script>

<article class="c-inbox-card" aria-labelledby="inbox-card-{item.id}">
  <div class="c-inbox-card__meta">
    <span class="c-inbox-card__badge c-inbox-card__badge--{KIND_TONE[item.kind]}"
      >{KIND_WORDS[item.kind]}</span
    >
    <span>{inbox.placeOf(item)} · {filedAt(item.createdAt)}</span>
  </div>
  <h2 class="c-inbox-card__title" id="inbox-card-{item.id}">
    <CodeText text={item.title} />
  </h2>

  {#if item.kind === 'review_ready' && groups.length > 0}
    {#each groups as section (section.group)}
      <section class="c-inbox-card__findings" aria-label={GROUP_WORDS[section.group]}>
        <span class="c-inbox-card__label"
          >{GROUP_WORDS[section.group]} · {section.findings.length}</span
        >
        <ul class="c-inbox-card__list">
          {#each section.findings as finding (finding.id)}
            <li>
              <Button
                variant="link"
                size="sm"
                title="Show this finding in the Reviews tab"
                onclick={() => inbox.open(item, { kind: 'finding', id: finding.id })}
                ><CodeText text={finding.title} /></Button
              >
              <span class="c-inbox-card__where u-mono">{place(finding)}</span>
            </li>
          {/each}
        </ul>
      </section>
    {/each}
  {:else}
    <p class="c-inbox-card__body"><CodeText text={item.body} /></p>
  {/if}

  {#if item.kind === 'approval'}
    {#if asking}
      <div class="c-inbox-card__approval">
        <ApprovalCard
          request={asking.approval.request}
          focusOnMount={false}
          onanswer={(reply) => void answered(reply)}
        />
      </div>
    {:else}
      <p class="c-inbox-card__note">
        The session that asked has ended, so there is nothing left to answer. It is kept
        here because a run's profile should never need to ask: dismiss it once you have
        looked.
      </p>
    {/if}
  {/if}

  <div class="c-inbox-card__actions">
    {#if resume && item.worktree}
      {@const worktree = item.worktree}
      <Button
        variant="accent"
        size="sm"
        disabled={resuming}
        onclick={() => void reviewRuns.resume(worktree, item.flow)}>{resume}</Button
      >
    {/if}
    <Button
      variant={resume ? 'neutral' : 'accent'}
      size="sm"
      onclick={() => inbox.open(item)}>{showWords(item)}</Button
    >
    <span class="c-inbox-card__spacer"></span>
    <Button
      variant="quiet"
      size="sm"
      disabled={busy}
      onclick={() => void inbox.dismiss(item)}>Dismiss</Button
    >
  </div>
  {#if error}
    <p class="c-inbox-card__error" role="alert"><CodeText text={error} /></p>
  {:else if resumeError}
    <p class="c-inbox-card__error" role="alert"><CodeText text={resumeError} /></p>
  {/if}
</article>
