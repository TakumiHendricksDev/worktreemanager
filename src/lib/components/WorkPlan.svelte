<script lang="ts">
  /**
   * An issue's Plan tab: its planner's questions, the plan it handed in, your decision, and what it
   * was given to plan from.
   *
   * # In the order you act on it
   *
   * 1. **Questions** first, open ones on top: an open one is what the run waits for. Each is the
   *    same card as the inbox's (`InboxQuestion`), so answering or discussing here is answering or
   *    discussing there.
   * 2. **The plan**, the newest revision whole, with the issue's changes since it was made when
   *    there are any: a plan may not answer an edit made after it.
   * 3. **The decision.** Approve, approve and skip design (when the plan asked for design), or ask
   *    for changes, which resumes the planner's conversation with your note. An approval pins the
   *    plan's revision and the issue's, and is shown stale, with the diff, once either moves on.
   * 4. **Context**, folded: everything the planner was given, as it was recorded when it started.
   * 5. **Sessions**, folded: each process, and the one provider conversation they continued.
   *
   * # The issue's revision you approve against
   *
   * Approve sends the revision of the issue this tab shows (`issueNow`). Rust reads the issue again
   * and refuses when it differs: what you approve is the plan for the issue in front of you.
   */
  import { untrack } from 'svelte';

  import type { IssueChange } from '../ipc/types';
  import { conversation, planRunWords, sessionWords, standingWords } from '../plan';
  import { plan } from '../state/plan.svelte';
  import { filedAt as formatTime } from '../inbox';
  import CodeText from './CodeText.svelte';
  import InboxQuestion from './InboxQuestion.svelte';
  import Button from './ui/Button.svelte';

  const {
    projectId,
    issueKey,
    visible,
    onopenfile,
  }: {
    projectId: string;
    issueKey: string;
    visible: boolean;
    onopenfile: (path: string, line: number | null) => void;
  } = $props();

  // Read when shown, untracked: `load` writes the store it would otherwise be tracking.
  $effect(() => {
    const project = projectId;
    const key = issueKey;
    if (visible) untrack(() => void plan.load(project, key));
  });

  const view = $derived(plan.views[issueKey] ?? null);
  const busy = $derived(plan.busy[issueKey] ?? false);
  const error = $derived(plan.errors[issueKey] ?? null);
  const latest = $derived(
    view && view.plans.length > 0 ? view.plans[view.plans.length - 1] : null,
  );
  const open = $derived(view?.questions.filter((q) => q.question.answer === null) ?? []);
  const answered = $derived(
    view?.questions.filter((q) => q.question.answer !== null) ?? [],
  );
  const deciding = $derived(
    view !== null &&
      latest !== null &&
      view.flow.state.end === null &&
      view.flow.state.phase === 'planned' &&
      !view.running &&
      (view.flow.state.hold === null || view.flow.state.hold.kind === 'awaiting_approval'),
  );
  const canResume = $derived(
    view !== null &&
      !view.running &&
      view.flow.state.end === null &&
      view.flow.state.phase === 'investigating' &&
      (view.flow.state.hold === null ||
        view.flow.state.hold.kind === 'interrupted' ||
        view.flow.state.hold.kind === 'waiting_capacity'),
  );
  let changing = $state(false);
  let note = $state('');

  async function requestChanges(): Promise<void> {
    if (!view || !latest || note.trim() === '') return;
    const ok = await plan.requestChanges(
      projectId,
      issueKey,
      view.flow.id,
      latest.revision,
      note,
    );
    if (ok) {
      note = '';
      changing = false;
    }
  }

  function approve(skipDesign: boolean): void {
    const shown = view?.issueRevisionNow;
    if (!view || !latest || !shown) return;
    // The issue as this tab shows it; Rust reads it again and refuses if it has moved on since.
    void plan.approve(
      projectId,
      issueKey,
      view.flow.id,
      latest.revision,
      shown,
      skipDesign,
    );
  }

  function changeLines(change: IssueChange): { kind: string; text: string }[] {
    if (change.lines.length > 0) return change.lines;
    return [
      ...(change.before === null ? [] : [{ kind: 'removed', text: change.before }]),
      ...(change.after === null ? [] : [{ kind: 'added', text: change.after }]),
    ];
  }
</script>

{#snippet diff(changes: IssueChange[])}
  <div class="c-plan-diff">
    {#each changes as change (change.part)}
      <section class="c-plan-diff__part" aria-label={change.part}>
        <span class="c-plan__label">{change.part}</span>
        <ol class="c-plan-diff__lines">
          {#each changeLines(change) as line, index (index)}
            <li class="c-plan-diff__line c-plan-diff__line--{line.kind}">
              <span class="c-plan-diff__sign" aria-hidden="true"
                >{line.kind === 'added' ? '+' : line.kind === 'removed' ? '−' : ' '}</span
              ><span class="u-visually-hidden"
                >{line.kind === 'added'
                  ? 'Added: '
                  : line.kind === 'removed'
                    ? 'Removed: '
                    : ''}</span
              >{line.text || ' '}
            </li>
          {/each}
        </ol>
      </section>
    {/each}
  </div>
{/snippet}

<div class="c-plan">
  {#if error}
    <p class="c-plan__error c-status--danger" role="alert"><CodeText text={error} /></p>
  {/if}

  {#if !view}
    <div class="c-placeholder">
      <p>
        {issueKey} has no run yet. Start work gives it a worktree; its planner starts there.
      </p>
    </div>
  {:else}
    <header class="c-plan__head">
      <h2 class="c-plan__title">{planRunWords(view)}</h2>
      <span class="c-plan__actions">
        {#if view.canStart}
          <Button
            variant="accent"
            size="sm"
            disabled={busy}
            title="Run the planner in the issue's worktree, read-only"
            onclick={() => void plan.start(projectId, issueKey, view.flow.id)}>Plan</Button
          >
        {/if}
        {#if canResume}
          <Button
            variant="accent"
            size="sm"
            disabled={busy}
            title="Continue the planner's conversation where it was cut off"
            onclick={() => void plan.resume(projectId, issueKey, view.flow.id)}
            >Resume planner</Button
          >
        {/if}
        {#if view.planning}
          <Button
            size="sm"
            disabled={busy}
            title="Read the issue from the tracker again: what an approval is measured against"
            onclick={() => void plan.checkIssue(projectId, issueKey, view.flow.id)}
            >Check the issue</Button
          >
        {/if}
      </span>
    </header>

    {#if !view.planner && !view.planning}
      <p class="c-plan__note">
        This project names no planner, so the run waits at “Ready to plan”. Add
        <span class="u-mono">planner = "&lt;role&gt;"</span> to its
        <span class="u-mono">[workflow]</span> in your own config, naming a reviewer role whose
        output is a plan.
      </p>
    {:else if view.running && view.plans.length === 0 && open.length === 0}
      <p class="c-plan__note">
        {view.flow.state.phase === 'preparing'
          ? 'Reading the issue, its sprint and the repository’s instructions…'
          : `The planner (${view.planning?.role ?? 'planner'}) is investigating, read-only, in the issue’s worktree.`}
      </p>
    {/if}

    {#each open as thread (thread.question.id)}
      <InboxQuestion question={thread.question} {onopenfile} />
    {/each}

    {#if latest}
      <section class="c-plan-card" aria-labelledby="plan-revision-{latest.revision}">
        <div class="c-plan-card__meta">
          <span class="c-plan-card__badge">Plan · revision {latest.revision}</span>
          <span>handed in {formatTime(latest.submittedAt)}</span>
        </div>
        <h3 class="c-plan-card__summary" id="plan-revision-{latest.revision}">
          <CodeText text={latest.plan.summary} />
        </h3>
        <!-- Once an approval is stale, its own diff below says the same and says it against what
             you approved, so this one stands aside. -->
        {#if view.issueChanges.length > 0 && view.standing.kind !== 'stale'}
          <div class="c-plan__warn c-status--warn" role="note">
            {issueKey} changed after the planner read it. This plan may not answer what changed.
          </div>
          {@render diff(view.issueChanges)}
        {/if}
        <div class="c-plan-card__grid">
          <section class="c-plan-card__part">
            <span class="c-plan__label">Steps</span>
            <ol class="c-plan-card__list">
              {#each latest.plan.steps as step, index (index)}
                <li><CodeText text={step} /></li>
              {/each}
            </ol>
          </section>
          <section class="c-plan-card__part">
            <span class="c-plan__label">Acceptance criteria</span>
            <ul class="c-plan-card__list">
              {#each latest.plan.acceptanceCriteria as criterion, index (index)}
                <li><CodeText text={criterion} /></li>
              {/each}
            </ul>
          </section>
          <section class="c-plan-card__part">
            <span class="c-plan__label">Verification</span>
            <ul class="c-plan-card__list">
              {#each latest.plan.verification as check, index (index)}
                <li><CodeText text={check} /></li>
              {/each}
            </ul>
          </section>
          <section class="c-plan-card__part">
            <span class="c-plan__label">Areas it expects to touch</span>
            <ul class="c-plan-card__list">
              {#each latest.plan.areas as area, index (index)}
                <li><span class="u-mono">{area}</span></li>
              {/each}
            </ul>
          </section>
        </div>
        <p class="c-plan-card__design">
          <strong>Design:</strong>
          {latest.plan.designNeeded ? 'needed' : 'not needed'}{latest.plan.designReason
            ? ` — ${latest.plan.designReason}`
            : ''}
        </p>
        {#if (latest.plan.risks ?? []).length > 0}
          <section class="c-plan-card__part">
            <span class="c-plan__label">Risks</span>
            <ul class="c-plan-card__list">
              {#each latest.plan.risks ?? [] as risk, index (index)}
                <li><CodeText text={risk} /></li>
              {/each}
            </ul>
          </section>
        {/if}

        <p
          class="c-plan__standing c-status--{view.standing.kind === 'approved'
            ? 'ok'
            : view.standing.kind === 'stale'
              ? 'warn'
              : 'muted'}"
        >
          {standingWords(view.standing)}
        </p>
        {#if view.standing.kind === 'stale'}
          {@render diff(view.standing.diff)}
        {/if}

        {#if deciding}
          <div class="c-plan__decide">
            <Button
              variant="accent"
              disabled={busy || !view.issueRevisionNow}
              title="Approve this revision for the issue as it reads now. Nothing is implemented in this version of wtm."
              onclick={() => approve(false)}
              >{view.standing.kind === 'stale' ? 'Approve again' : 'Approve'}</Button
            >
            {#if latest.plan.designNeeded}
              <Button
                disabled={busy || !view.issueRevisionNow}
                title="Approve, and go straight to implementing without a design pass"
                onclick={() => approve(true)}>Approve and skip design</Button
              >
            {/if}
            <Button
              disabled={busy}
              ariaExpanded={changing}
              onclick={() => (changing = !changing)}>Request changes</Button
            >
          </div>
          {#if changing}
            <form
              class="c-plan__changes"
              onsubmit={(event) => {
                event.preventDefault();
                void requestChanges();
              }}
            >
              <label class="c-plan__field">
                What should change?
                <textarea class="c-textarea" rows="3" bind:value={note}></textarea>
              </label>
              <div class="c-plan__decide">
                <Button variant="accent" type="submit" disabled={busy || note.trim() === ''}
                  >Send to the planner</Button
                >
                <Button variant="quiet" onclick={() => (changing = false)}>Cancel</Button>
              </div>
              <p class="c-plan__note">
                The planner gets your note in the same conversation and hands in the next
                revision. Nothing is posted anywhere.
              </p>
            </form>
          {/if}
        {/if}
      </section>
    {/if}

    {#if view.plans.length > 1 || view.decisions.length > 0}
      <details class="c-plan-history">
        <summary>Earlier revisions and decisions</summary>
        <ul class="c-plan-history__list">
          {#each view.plans.slice(0, -1) as earlier (earlier.id)}
            <li>
              Revision {earlier.revision}, {formatTime(earlier.submittedAt)}:
              <CodeText text={earlier.plan.summary} />
            </li>
          {/each}
          {#each view.decisions as decision (decision.id)}
            <li>
              {formatTime(decision.at)} · revision {decision.revision}:
              {decision.verdict.kind === 'approved'
                ? `approved${decision.verdict.skipDesign ? ', design skipped' : ''}`
                : `changes asked for — “${decision.verdict.note}”`}
            </li>
          {/each}
        </ul>
      </details>
    {/if}

    {#each answered as thread (thread.question.id)}
      <InboxQuestion question={thread.question} {onopenfile} />
    {/each}

    {#if view.planning?.context}
      {@const context = view.planning.context}
      <details class="c-plan-context">
        <summary>Context the planner was given</summary>
        <dl class="o-facts o-facts--wide">
          <dt>Issue</dt>
          <dd>
            <span class="u-mono">{context.issue.key}</span> · {context.issue.summary} ({context.status})
          </dd>
          {#if context.issue.description}
            <dt>Description</dt>
            <dd><p class="c-plan-context__text">{context.issue.description}</p></dd>
          {/if}
          {#each context.issue.fields as field (field.name)}
            <dt>{field.name}</dt>
            <dd>{field.value ?? '—'}</dd>
          {/each}
          <dt>Sprint</dt>
          <dd>
            {context.sprint ?? 'none read'}{context.sprintGoal
              ? ` · Goal: ${context.sprintGoal}`
              : ''}
          </dd>
          <dt>Dependencies</dt>
          <dd>
            {#if context.dependencies.length > 0}
              <ul class="o-plain-list">
                {#each context.dependencies as dependency (dependency.relation + dependency.key)}
                  <li>
                    {dependency.relation}
                    <span class="u-mono">{dependency.key}</span>{dependency.summary
                      ? ` · ${dependency.summary}`
                      : ''}{dependency.status ? ` (${dependency.status})` : ''}
                  </li>
                {/each}
              </ul>
            {:else}
              None linked.
            {/if}
          </dd>
          <dt>Instructions</dt>
          <dd>
            {#if context.instructionSources.length > 0}
              <ul class="o-plain-list">
                {#each context.instructionSources as source (source)}
                  <li><span class="u-mono">{source}</span></li>
                {/each}
              </ul>
            {:else}
              None found, or the role does not take them.
            {/if}
          </dd>
          <dt>Your rules</dt>
          <dd>
            {#if context.rules.length > 0}
              <ul class="o-plain-list">
                {#each context.rules as rule (rule.rule)}
                  <li>
                    R-{rule.rule} v{rule.revision} · {rule.kind} · {rule.text}
                  </li>
                {/each}
              </ul>
            {:else}
              None applied.
            {/if}
          </dd>
          <dt>Skills</dt>
          <dd>{context.skills.length > 0 ? context.skills.join(', ') : 'None named.'}</dd>
          <dt>Planner</dt>
          <dd>
            {context.role} · {context.provider}{context.model ? ` ${context.model}` : ''} · read-only
            · read {formatTime(context.readAt)}
          </dd>
        </dl>
      </details>
    {/if}

    {#if view.sessions.length > 0}
      <details class="c-plan-context" open={open.length > 0}>
        <summary>Sessions · one conversation</summary>
        <ul class="o-plain-list c-plan-sessions">
          {#each view.sessions as session (session.id)}
            <li>
              {sessionWords(session)} · conversation
              <span class="u-mono" title={session.providerSession ?? ''}
                >{conversation(session.providerSession)}</span
              >
            </li>
          {/each}
        </ul>
      </details>
    {/if}
  {/if}
</div>
