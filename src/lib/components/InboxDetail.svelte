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
   * - **A publish** — an approval gone stale because the pull request moved, or one that failed or
   *   stopped partway — is the publication's own card (`WorkPublication`), with what settles it:
   *   Retry, the pending-review choices, or re-review and publish anyway. It reads the run the
   *   Reviews tab reads, so a retry here updates both. A local fix's route that stopped — their
   *   branch moved, or a push failed — is the route's own card (`WorkRoute`) the same way.
   * - **A ready review** names the worst of what it found; each finding is a link to that finding
   *   in the Reviews tab, which is the exact place the item is about. Reading the review settles
   *   it.
   */
  import { untrack } from 'svelte';

  import {
    KIND_TONE,
    KIND_WORDS,
    filedAt,
    isEnvironmentApproval,
    resumeWords,
    showWords,
  } from '../inbox';
  import { commands } from '../ipc/commands';
  import type {
    FindingRecord,
    InboxItem,
    PendingCommandView,
    PublicationView,
  } from '../ipc/types';
  import { draftOf } from '../publish';
  import { GROUP_WORDS, findingGroups } from '../review-run';
  import { environments } from '../state/environments.svelte';
  import { inbox } from '../state/inbox.svelte';
  import { reviewRuns } from '../state/review-runs.svelte';
  import { sessions } from '../state/sessions.svelte';
  import { work } from '../state/work.svelte';
  import { workspace } from '../state/workspace.svelte';
  import CodeText from './CodeText.svelte';
  import ApprovalCard from './ApprovalCard.svelte';
  import Button from './ui/Button.svelte';
  import WorkEnvApproval from './WorkEnvApproval.svelte';
  import WorkPublication from './WorkPublication.svelte';
  import WorkRoute from './WorkRoute.svelte';

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

  /**
   * A destructive environment command waiting for approval, by the environment's flow: an approval
   * item filed against a flow rather than a run's step. Read when it is opened, and answered here
   * exactly as in the Environment tab.
   */
  let pendingCommand = $state<PendingCommandView | null | undefined>(undefined);
  $effect(() => {
    const flow = item.kind === 'approval' && item.target.kind === 'flow' ? item.flow : null;
    untrack(() => {
      pendingCommand = undefined;
      if (flow === null) return;
      void commands
        .pendingEnvironmentCommand(flow)
        .then((pending) => {
          if (item.flow === flow) pendingCommand = pending;
        })
        .catch(() => {
          pendingCommand = null;
        });
    });
  });
  const commandBusy = $derived(
    pendingCommand ? (environments.busy[pendingCommand.worktree] ?? false) : false,
  );
  const commandError = $derived(
    pendingCommand ? (environments.errors[pendingCommand.worktree] ?? null) : null,
  );

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

  /** The publish an item is about, by its approval: live from the worktree's run when shown there. */
  const approvalId = $derived(
    item.target.kind === 'approval' ? item.target.approval : null,
  );
  let fetched = $state<PublicationView | null>(null);
  $effect(() => {
    const id = approvalId;
    const project = item.project;
    const worktree = item.worktree;
    untrack(() => {
      fetched = null;
      if (id === null) return;
      if (project && worktree && reviewRuns.latest[worktree] === undefined) {
        void reviewRuns.load(project, worktree);
      }
      void commands
        .reviewPublication(id)
        .then((view) => {
          if (approvalId === id) fetched = view;
        })
        .catch(() => {});
    });
  });
  const publication = $derived.by(() => {
    if (approvalId === null) return null;
    const live = item.worktree
      ? reviewRuns.latest[item.worktree]?.pullRequest?.publications.find(
          (p) => p.approval.id === approvalId,
        )
      : undefined;
    return live ?? fetched;
  });
  /** A local fix's route the item is about, live from the worktree's run. */
  const route = $derived.by(() => {
    if (approvalId === null || !item.worktree) return null;
    return (
      reviewRuns.latest[item.worktree]?.pullRequest?.routes.find(
        (r) => r.approval.id === approvalId,
      ) ?? null
    );
  });
  const routing = $derived(
    item.worktree ? (reviewRuns.busy[`route:${item.worktree}`] ?? false) : false,
  );
  const routeError = $derived(
    item.worktree ? (reviewRuns.errors[`route:${item.worktree}`] ?? null) : null,
  );
  const publishing = $derived(
    item.worktree ? (reviewRuns.busy[`publication:${item.worktree}`] ?? false) : false,
  );
  const publishError = $derived(
    item.worktree ? (reviewRuns.errors[`publication:${item.worktree}`] ?? null) : null,
  );

  function act(action: () => Promise<PublicationView>): void {
    if (!item.worktree) return;
    void reviewRuns.publication(item.worktree, item.flow, action);
  }

  async function rereviewInstead(stale: PublicationView, head: string): Promise<void> {
    const [project, worktree] = [item.project, item.worktree];
    if (!project || !worktree) return;
    await reviewRuns.publication(worktree, item.flow, () =>
      commands.cancelPublication(stale.approval.id),
    );
    await reviewRuns.rereview(project, worktree, item.flow, head);
  }

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

  {#if publication}
    <WorkPublication
      view={publication}
      head={null}
      busy={publishing}
      error={publishError}
      onretry={() => act(() => commands.retryPublication(publication.approval.id))}
      onadd={() => act(() => commands.addToPendingReview(publication.approval.id))}
      onleave={() => act(() => commands.cancelPublication(publication.approval.id))}
      onrereview={publication.state === 'stale'
        ? () => {
            const stopped = publication.operations.find(
              (op) => op.attention?.kind === 'head_moved',
            );
            if (stopped?.attention?.kind === 'head_moved') {
              void rereviewInstead(publication, stopped.attention.current);
            }
          }
        : null}
      onagainst={(head) => {
        const hash = publication.approval.binding.payload;
        const worktree = item.worktree;
        if (!hash || !worktree) return;
        act(() =>
          reviewRuns.approvePublish(
            worktree,
            item.flow,
            draftOf(publication.review),
            hash,
            head,
          ),
        );
      }}
      onopen={(url) => void commands.openUrl(url)}
    />
  {:else if route}
    {@const worktree = item.worktree ?? ''}
    <WorkRoute
      view={route}
      busy={routing}
      error={routeError}
      onretry={() =>
        void reviewRuns.route(worktree, item.flow, () =>
          commands.retryFixRoute(route.approval.id),
        )}
      onleave={() =>
        void reviewRuns.route(worktree, item.flow, () =>
          commands.cancelFixRoute(route.approval.id),
        )}
      onopen={(url) => void commands.openUrl(url)}
    />
  {:else if item.kind === 'approval' && item.target.kind === 'approval'}
    <p class="c-inbox-card__note">Reading this publish…</p>
  {:else if item.kind === 'approval' && item.target.kind === 'flow'}
    {#if pendingCommand}
      {@const pending = pendingCommand}
      <WorkEnvApproval
        {pending}
        busy={commandBusy}
        error={commandError}
        onapprove={() =>
          void environments.approve(pending.project, pending.worktree, pending.run.id)}
        ondecline={() =>
          void environments.decline(pending.project, pending.worktree, pending.run.id)}
      />
    {:else if pendingCommand === null}
      <p class="c-inbox-card__note">
        Nothing is waiting any more: the command was approved, declined, or went stale.
      </p>
    {:else}
      <p class="c-inbox-card__note">Reading this command…</p>
    {/if}
  {:else if item.kind === 'approval'}
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
      variant={resume || publication || route ? 'neutral' : 'accent'}
      size="sm"
      onclick={() => {
        // An environment's approval goes to its Environment tab when that worktree is in the
        // project on screen; anywhere else, the run's own route.
        if (
          isEnvironmentApproval(item) &&
          item.worktree &&
          item.project === workspace.activeProjectId
        ) {
          work.pick(item.worktree);
          work.tab = 'environment';
          return;
        }
        inbox.open(
          item,
          approvalId !== null ? { kind: 'publication', id: approvalId } : undefined,
        );
      }}>{showWords(item)}</Button
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
