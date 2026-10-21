<script lang="ts">
  /**
   * The Reviews tab for one worktree: Run Review before there is a review, and then the review —
   * its state, its findings grouped by class, the checks the app ran, and what the roles were
   * given to judge by.
   *
   * # Nine states, one block
   *
   * Not reviewed, running, waiting for usage capacity, interrupted, a role failed, stale, reviewed
   * (clean included, with any check that did not pass said so), can't start over billing, and
   * can't run for want of a skill. Each is the `c-work-state` block with its words from
   * `review-run.ts`, so the queue row, the header pill and this block never describe a run two
   * ways. A billing refusal is not a run — nothing was recorded — so it is drawn from the refused
   * action's error rather than from a run.
   *
   * # Stale is asked for, then drawn
   *
   * Whether the findings still hold is `review_staleness`, which asks git. It is asked when a run is
   * shown, when it settles, and when this worktree's listing changes — its head, or whether it has
   * changes — which the window learns on focus. Never on a timer.
   *
   * # The actions are the record's
   *
   * Dismiss, Save as rule and the reruns go through `reviewRuns` and back to Rust; Save as rule
   * opens R7's dialog on the stored finding, by its id. Open in Code goes through `codeRequests`,
   * which takes you to the worktree's Code tab at the line.
   */
  import { untrack } from 'svelte';

  import type { FindingRecord, Worktree } from '../ipc/types';
  import {
    canResume,
    canRunAgain,
    canStop,
    CHECK_TONE,
    CHECK_WORDS,
    checkLines,
    findingGroups,
    GROUP_WORDS,
    headline,
    listedSteps,
    providerName,
    latestRound,
    resumeWords,
    ruleLabel,
    runsOn,
    skippedLine,
    STEP_TONE,
    stepLine,
  } from '../review-run';
  import { codeRequests } from '../state/code-request.svelte';
  import { reviewRuns } from '../state/review-runs.svelte';
  import { workspace } from '../state/workspace.svelte';
  import CodeText from './CodeText.svelte';
  import RuleDialog from './RuleDialog.svelte';
  import Button from './ui/Button.svelte';
  import WorkFinding from './WorkFinding.svelte';

  const {
    projectId,
    worktree,
    reviewing,
    subject,
  }: {
    projectId: string;
    worktree: Worktree;
    /** What the review is of, as its title says it: the branch, or a short head. */
    reviewing: string;
    /** The work item a rule saved from a finding names, such as an issue key or the branch. */
    subject: string | null;
  } = $props();

  const worktreeId = $derived(worktree.id);
  const run = $derived(reviewRuns.latest[worktreeId]);
  const stale = $derived(run ? (reviewRuns.staleness[run.flow.id] ?? null) : null);
  const recipes = $derived(reviewRuns.recipes[projectId] ?? null);
  const busy = $derived(reviewRuns.busy[worktreeId] ?? false);
  const error = $derived(reviewRuns.errors[worktreeId] ?? null);
  const problem = $derived(reviewRuns.problems[worktreeId] ?? null);
  const top = $derived(run ? headline(run, stale) : null);
  const groups = $derived(run ? findingGroups(run.findings) : []);
  const dismissed = $derived(run ? run.findings.filter((f) => f.dismissed !== null) : []);
  const checks = $derived(run ? checkLines(run, stale) : []);
  const showRerun = $derived(
    run !== undefined &&
      run !== null &&
      (stale?.changed === true ||
        run.findings.some(
          (f) => f.dismissed === null && stale?.staleFindings.includes(f.id),
        )) &&
      run.flow.state.end === null &&
      run.flow.state.hold === null &&
      !run.running &&
      run.flow.state.phase === 'consolidated',
  );

  let chosen = $state<string | null>(null);
  const recipe = $derived(chosen ?? run?.review.recipe ?? recipes?.[0]?.id ?? null);
  const chosenRecipe = $derived(
    recipes?.find((r) => r.id === recipe) ?? recipes?.[0] ?? null,
  );
  let saving = $state<FindingRecord | null>(null);

  // Read again whenever the worktree shown changes, by id rather than by object: a worktree
  // listing refresh hands over new objects for the same worktrees.
  $effect(() => {
    const project = projectId;
    const id = worktreeId;
    untrack(() => {
      void reviewRuns.load(project, id);
      void reviewRuns.loadRecipes(project);
    });
  });

  /** What the listing says about the worktree's contents: when it moves, staleness may have. */
  const treeKey = $derived(
    `${worktree.head ?? ''}:${worktree.dirty}:${worktree.staged}:${worktree.untracked}`,
  );
  const settledFlow = $derived(run && !run.running && run.snapshot ? run.flow.id : null);
  $effect(() => {
    const flow = settledFlow;
    void treeKey;
    if (flow !== null) untrack(() => void reviewRuns.checkStaleness(flow));
  });

  function start(): void {
    if (recipe) void reviewRuns.start(projectId, worktreeId, recipe);
  }

  function open(path: string, line: number | null): void {
    codeRequests.open(projectId, worktreeId, path, line);
  }

  /** The command that signs a provider in to its subscription, as its CLI spells it. */
  function signIn(provider: string): string {
    return provider === 'codex' ? 'codex login' : 'claude /login';
  }

  /** Roles that only read, in the snapshot copy: the line the design ends the Applied list with. */
  const readOnly = $derived(
    run !== undefined &&
      run !== null &&
      run.steps.length > 0 &&
      run.steps.every((s) => s.profile === 'reviewer'),
  );
  const inSnapshot = $derived(
    run !== undefined && run !== null && run.steps.every((s) => s.workspace === 'snapshot'),
  );
</script>

{#snippet billing(providers: string[], message: string)}
  <section class="c-work-state c-work-state--danger" aria-labelledby="work-billing-title">
    <span class="c-work-state__eyebrow">Can't start: billing</span>
    <h2 class="c-work-state__title" id="work-billing-title">
      {providers.length > 0
        ? `${providers.map(providerName).join(' and ')} would not bill your subscription`
        : 'A provider would not bill your subscription'}
    </h2>
    <p class="c-work-state__prose">
      <CodeText text={message} /> Review runs use subscriptions only, so nothing was started.
    </p>
    {#if providers.length > 0}
      <p class="c-work-state__note">
        Sign in with {#each providers as provider, index (provider)}{#if index > 0}{' or '}{/if}<code
            >{signIn(provider)}</code
          >{/each}, then check again.
      </p>
    {/if}
    <div class="c-work-state__actions">
      <Button size="sm" disabled={busy || recipe === null} onclick={start}
        >Check again</Button
      >
    </div>
  </section>
{/snippet}

{#if run && top}
  <div class="c-review">
    <p class="c-review__meta">
      <span>Recipe <strong>{chosenRecipe?.label ?? run.review.recipe}</strong></span>
      {#if run.snapshot}
        <span
          >Snapshot <code>{run.snapshot.headSha.slice(0, 7)}</code> with the working tree
          (tree
          <code>{run.snapshot.treeSha.slice(0, 7)}</code>)</span
        >
      {/if}
      <span>{listedSteps(run).filter((s) => s.status !== 'replaced').length} roles</span>
      {#if latestRound(run).length}<span>{latestRound(run).length}</span>{/if}
      <span
        >{new Date(latestRound(run).startedAt).toLocaleTimeString([], {
          hour: 'numeric',
          minute: '2-digit',
        })}</span
      >
      <span>Compared with the <code>{run.review.base}</code> merge base</span>
    </p>

    {#if problem?.kind === 'billing'}
      {@render billing(problem.providers, problem.message)}
    {/if}

    <section class="c-work-state c-work-state--{top.tone}" aria-labelledby="work-run-title">
      <span class="c-work-state__eyebrow">{top.eyebrow}</span>
      <h2 class="c-work-state__title" id="work-run-title"><CodeText text={top.title} /></h2>
      <p class="c-work-state__prose"><CodeText text={top.prose} /></p>

      <ul class="c-run-roles" aria-label="Roles">
        {#each listedSteps(run) as step (step.id)}
          <li class="c-run-roles__row">
            <span
              class="c-run-roles__mark c-run-roles__mark--{STEP_TONE[step.status]}"
              aria-hidden="true"
            ></span>
            <span class="c-run-roles__who">
              {step.role}{step.standsInFor ? ` (for ${step.standsInFor})` : ''} · {runsOn(
                step,
              )}
            </span>
            <span class="c-run-roles__line"><CodeText text={stepLine(run, step)} /></span>
          </li>
        {/each}
      </ul>
      {#if skippedLine(run)}
        <p class="c-work-state__note">{skippedLine(run)}</p>
      {/if}

      <div class="c-work-state__actions">
        {#if showRerun && stale}
          <Button
            variant="accent"
            size="sm"
            disabled={busy || stale.affected.length === 0}
            title={stale.affected.length > 0
              ? `Runs ${stale.affected.join(', ')} against a new snapshot`
              : 'No role read a file that changed'}
            onclick={() => void reviewRuns.rerun(worktreeId, run.flow.id, 'affected')}
          >
            Rerun affected roles ({stale.affected.length})
          </Button>
          <Button
            size="sm"
            disabled={busy}
            onclick={() => void reviewRuns.rerun(worktreeId, run.flow.id, 'all')}
            >Rerun everything</Button
          >
        {/if}
        {#if canResume(run)}
          <Button
            variant={showRerun ? 'neutral' : 'accent'}
            size="sm"
            disabled={busy}
            onclick={() => void reviewRuns.resume(worktreeId, run.flow.id)}
          >
            {resumeWords(run)}
          </Button>
        {/if}
        {#if canStop(run)}
          <Button
            variant="quiet"
            size="sm"
            disabled={busy}
            onclick={() => void reviewRuns.stop(worktreeId, run.flow.id)}
            >Stop review</Button
          >
        {/if}
        {#if canRunAgain(run) && !showRerun && recipe}
          <Button size="sm" disabled={busy} onclick={start}>Run again</Button>
        {/if}
      </div>
      {#if error && problem?.kind !== 'billing'}
        <p class="c-work-state__error" role="alert"><CodeText text={error} /></p>
      {/if}
    </section>

    {#each groups as section (section.group)}
      {#if section.group === 'suggestion'}
        <details class="c-findings" open={groups.length === 1}>
          <summary class="c-findings__heading"
            >{GROUP_WORDS[section.group]} · {section.findings.length}</summary
          >
          {#each section.findings as finding (finding.id)}
            <WorkFinding
              {finding}
              view={run}
              {busy}
              stale={stale?.staleFindings.includes(finding.id) ?? false}
              onopen={open}
              onsaverule={(f) => (saving = f)}
              ondismiss={(f, reason) => void reviewRuns.dismiss(worktreeId, f.id, reason)}
            />
          {/each}
        </details>
      {:else}
        <section class="c-findings" aria-labelledby="work-findings-{section.group}">
          <h3 class="c-findings__heading" id="work-findings-{section.group}">
            {GROUP_WORDS[section.group]} · {section.findings.length}
          </h3>
          {#each section.findings as finding (finding.id)}
            <WorkFinding
              {finding}
              view={run}
              {busy}
              stale={stale?.staleFindings.includes(finding.id) ?? false}
              onopen={open}
              onsaverule={(f) => (saving = f)}
              ondismiss={(f, reason) => void reviewRuns.dismiss(worktreeId, f.id, reason)}
            />
          {/each}
        </section>
      {/if}
    {/each}

    {#if dismissed.length > 0}
      <details class="c-findings">
        <summary class="c-findings__heading">Dismissed · {dismissed.length}</summary>
        {#each dismissed as finding (finding.id)}
          <WorkFinding
            {finding}
            view={run}
            {busy}
            stale={false}
            onopen={open}
            onsaverule={(f) => (saving = f)}
            ondismiss={(f, reason) => void reviewRuns.dismiss(worktreeId, f.id, reason)}
          />
        {/each}
      </details>
    {/if}

    {#if checks.length > 0}
      <section class="c-review-checks" aria-labelledby="work-checks-title">
        <h3 class="c-findings__heading" id="work-checks-title">
          Checks · run by wtm, never on an agent's say-so
        </h3>
        <ul class="c-review-checks__list">
          {#each checks as check (check.id)}
            <li
              class="c-review-checks__item c-review-checks__item--{CHECK_TONE[check.state]}"
            >
              <span class="c-review-checks__mark" aria-hidden="true"></span>
              <span
                >{check.label} · {CHECK_WORDS[check.state]}{#if check.detail}: <CodeText
                    text={check.detail}
                  />{/if}</span
              >
              {#if check.stale}<span class="c-finding__stale">Stale</span>{/if}
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    <p class="c-review__applied">
      Applied:
      {#if run.applied.rules.length > 0}
        rules {run.applied.rules.map((r) => ruleLabel(r.rule, r.revision)).join(', ')}
      {:else}
        no rules
      {/if}
      · {run.applied.sources.length > 0
        ? `instruction sources ${run.applied.sources.join(', ')}`
        : 'no instruction sources'}
      {#if run.applied.skills.length > 0}
        · {run.applied.skills
          .map((s) => `skill /${s.skill} (${providerName(s.provider)})`)
          .join(', ')}
      {/if}
      {#if readOnly}
        · roles ran read-only{inSnapshot ? ' in a snapshot copy' : ''}; none can push, merge
        or reach GitHub
      {/if}
    </p>
  </div>
{:else if problem?.kind === 'billing'}
  {@render billing(problem.providers, problem.message)}
{:else}
  <section class="c-work-state" aria-labelledby="work-review-title">
    <span class="c-work-state__eyebrow">Not reviewed yet</span>
    <h2 class="c-work-state__title" id="work-review-title">Review {reviewing}</h2>
    <p class="c-work-state__prose">
      A review reads a snapshot of this worktree, committed and uncommitted work alike,
      against its merge base. It needs no pull request and changes nothing in your tracker.
    </p>
    {#if recipes && recipes.length > 1}
      <label class="c-work-state__field">
        Recipe
        <!-- The value shown is the recipe Run Review would use, the first until one is picked;
             binding `chosen` alone would show a blank select over a default that still applies. -->
        <select
          class="c-select"
          value={recipe}
          onchange={(event) => (chosen = event.currentTarget.value)}
        >
          {#each recipes as choice (choice.id)}
            <option value={choice.id}>{choice.label} · {choice.roles.length} roles</option>
          {/each}
        </select>
      </label>
    {/if}
    <div class="c-work-state__actions">
      <Button variant="accent" disabled={busy || recipe === null} onclick={start}
        >Run Review</Button
      >
      {#if recipes !== null && recipes.length === 0}
        <span class="c-work-state__note"
          >No review recipes yet: add <code>[workflow]</code> roles and a recipe to your own config.</span
        >
      {:else if chosenRecipe}
        <span class="c-work-state__note">
          {chosenRecipe.roles.map((role) => `${role.id} on ${runsOn(role)}`).join(' · ')}
        </span>
      {/if}
    </div>
    {#if error}
      <p class="c-work-state__error" role="alert"><CodeText text={error} /></p>
    {/if}
  </section>
{/if}

{#if saving}
  <RuleDialog
    {projectId}
    projectName={workspace.activeProject?.name ?? ''}
    mode={{ kind: 'finding', finding: saving, subject }}
    onclose={() => (saving = null)}
    onsaved={() => (saving = null)}
  />
{/if}
