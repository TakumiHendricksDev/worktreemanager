<script lang="ts">
  /**
   * Settings › Knowledge: one project's review rules, and the repository instructions they sit
   * beside.
   *
   * # Two columns, from the approved design
   *
   * The list and filters on the left, the chosen rule's detail and history on the right, and the
   * repository's instruction sources under the detail, where the precedence between the two is
   * stated. Settings widens while this section is open; below the dialog's narrow width the
   * columns stack.
   *
   * # Proposals stay visible under Active
   *
   * An agent's proposal does nothing until you decide on it, so it is drawn beneath the active
   * list as well as under its own filter, where the design put it: a proposal that only appears
   * when you go looking for it is one that waits forever.
   *
   * # What is not here
   *
   * Conflicts between a rule and the repository's instructions are found by the conventions role
   * during a review, and no review has run in this build, so no rule is marked as conflicting.
   * "Propose to repository" is drawn and disabled, as the plan defers it.
   */
  import { commands } from '../ipc/commands';
  import { errorMessage } from '../ipc/types';
  import {
    FILTERS,
    KIND_WORDS,
    appliedWords,
    areaWords,
    countByFilter,
    current,
    dayWords,
    filterOf,
    originWords,
    revisionAuthor,
    ruleLabel,
    scopeParts,
    stateWords,
    usageWords,
    type InstructionSources,
    type RuleChange,
    type RuleFilter,
    type RuleView,
  } from '../knowledge';
  import RuleDialog, { type RuleDialogMode } from './RuleDialog.svelte';
  import Banner from './ui/Banner.svelte';
  import Button from './ui/Button.svelte';

  const { projectId, projectName }: { projectId: string; projectName: string } = $props();

  let rules = $state<RuleView[] | null>(null);
  let sources = $state<InstructionSources | null>(null);
  let loadError = $state<string | null>(null);
  let sourcesError = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let filter = $state<RuleFilter>('active');
  let selectedId = $state<number | null>(null);
  let dialog = $state<RuleDialogMode | null>(null);
  let busy = $state(false);
  let showSourcesHelp = $state(false);

  const counts = $derived(countByFilter(rules ?? []));
  const listed = $derived((rules ?? []).filter((r) => filterOf(r) === filter));
  const proposals = $derived((rules ?? []).filter((r) => filterOf(r) === 'proposed'));
  const selected = $derived(
    (rules ?? []).find((r) => r.id === selectedId) ?? listed[0] ?? null,
  );

  /*
   * Rules and sources load separately: the sources read git and the disk, and a repository git
   * cannot list should not hide the rules, which come from the app's own database.
   */
  async function load(project: string) {
    loadError = null;
    try {
      const next = await commands.listRules(project);
      if (project === projectId) rules = next;
    } catch (e) {
      loadError = errorMessage(e);
    }
  }

  async function loadSources(project: string) {
    sourcesError = null;
    try {
      const next = await commands.instructionSources(project);
      if (project === projectId) sources = next;
    } catch (e) {
      sourcesError = errorMessage(e);
    }
  }

  $effect(() => {
    const project = projectId;
    rules = null;
    sources = null;
    selectedId = null;
    void load(project);
    void loadSources(project);
  });

  function choose(next: RuleFilter) {
    filter = next;
    selectedId = null;
  }

  async function change(rule: RuleView, action: RuleChange) {
    if (busy) return;
    busy = true;
    actionError = null;
    try {
      const changed = await commands.changeRule(projectId, rule.id, action);
      await load(projectId);
      // Follow the rule to the filter it now belongs under, so the pane does not jump elsewhere.
      const next = filterOf(changed);
      if (next) filter = next;
      selectedId = changed.id;
    } catch (e) {
      actionError = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  async function saved(rule: RuleView) {
    dialog = null;
    await load(projectId);
    const next = filterOf(rule);
    if (next) filter = next;
    selectedId = rule.id;
  }

  function show(id: number) {
    const rule = (rules ?? []).find((r) => r.id === id);
    const next = rule ? filterOf(rule) : null;
    if (next) filter = next;
    selectedId = id;
  }
</script>

<div class="c-knowledge">
  <section class="c-knowledge__rules" aria-labelledby="knowledge-title">
    <div class="c-knowledge__head">
      <h3 id="knowledge-title" class="c-knowledge__title">Review knowledge</h3>
      <Button variant="accent" size="sm" onclick={() => (dialog = { kind: 'new' })}>
        New rule
      </Button>
    </div>
    <p class="c-knowledge__intro">
      Your private rules for {projectName}. Planning and review load the ones whose scope
      matches the files involved.
    </p>

    {#if loadError}
      <Banner>
        {loadError}
        {#snippet action()}
          <Button size="sm" onclick={() => void load(projectId)}>Retry</Button>
        {/snippet}
      </Banner>
    {/if}

    <div class="c-knowledge__filters" role="group" aria-label="Filter rules">
      {#each FILTERS as option (option.id)}
        <button
          type="button"
          class="c-knowledge__filter"
          class:is-pressed={filter === option.id}
          aria-pressed={filter === option.id}
          onclick={() => choose(option.id)}
        >
          {option.label} <span class="c-knowledge__count">{counts[option.id]}</span>
        </button>
      {/each}
    </div>

    {#if rules === null && !loadError}
      <p class="c-knowledge__empty">Loading rules…</p>
    {:else if rules !== null && rules.length === 0}
      <p class="c-knowledge__empty">
        No rules yet. Save one from a finding in a review, or write one with New rule.
      </p>
    {:else if filter === 'proposed'}
      {#if proposals.length === 0}
        <p class="c-knowledge__empty">{FILTERS[1]?.empty}</p>
      {/if}
    {:else if listed.length === 0}
      <p class="c-knowledge__empty">{FILTERS.find((f) => f.id === filter)?.empty}</p>
    {:else}
      <ul class="o-plain-list c-knowledge__list">
        {#each listed as rule (rule.id)}
          {@const revision = current(rule)}
          <li>
            <button
              type="button"
              class="c-rule-row"
              class:is-selected={selected?.id === rule.id}
              aria-current={selected?.id === rule.id ? 'true' : undefined}
              onclick={() => (selectedId = rule.id)}
            >
              <span class="c-rule-row__meta">
                {ruleLabel(rule.id)} v{revision.number} ·
                <span
                  class="c-rule-row__kind"
                  class:c-rule-row__kind--required={revision.draft.kind === 'required'}
                  >{KIND_WORDS[revision.draft.kind].label}</span
                >
                · {areaWords(revision.draft)}
              </span>
              <span class="c-rule-row__text">{revision.draft.text}</span>
              <span class="c-rule-row__usage">{usageWords(rule)}</span>
            </button>
          </li>
        {/each}
      </ul>
    {/if}

    {#if filter === 'active' || filter === 'proposed'}
      {#each proposals as rule (rule.id)}
        {@const revision = current(rule)}
        <div class="c-proposal" class:is-selected={selected?.id === rule.id}>
          <span class="c-proposal__eyebrow">Proposed by an agent · inactive</span>
          <button
            type="button"
            class="c-proposal__text"
            onclick={() => show(rule.id)}
            aria-label="Show {ruleLabel(rule.id)}"
          >
            “{revision.draft.text}”
          </button>
          <p class="c-proposal__why">
            {#if rule.source.kind === 'proposal'}From the {rule.source.role} role{rule.sourceSubject
                ? ` on ${rule.sourceSubject}`
                : ''}.{/if}
            Agent lessons never govern reviews until you approve them.
          </p>
          <div class="c-proposal__actions">
            <Button
              variant="accent"
              size="sm"
              disabled={busy}
              onclick={() => void change(rule, { kind: 'approve' })}>Approve</Button
            >
            <Button
              size="sm"
              disabled={busy}
              onclick={() => (dialog = { kind: 'review', rule })}>Edit first</Button
            >
            <Button
              variant="quiet"
              size="sm"
              disabled={busy}
              onclick={() => void change(rule, { kind: 'reject' })}>Reject</Button
            >
          </div>
        </div>
      {/each}
    {/if}
  </section>

  <section class="c-knowledge__detail" aria-label="Rule">
    {#if actionError}<Banner>{actionError}</Banner>{/if}

    {#if selected}
      {@const revision = current(selected)}
      {@const draft = revision.draft}
      <div class="c-knowledge__toolbar">
        <span class="c-knowledge__state">{stateWords(selected)}</span>
        {#if selected.approval === 'pending'}
          <Button
            variant="accent"
            size="sm"
            disabled={busy}
            onclick={() => void change(selected, { kind: 'approve' })}>Approve</Button
          >
          <Button
            size="sm"
            disabled={busy}
            onclick={() => (dialog = { kind: 'review', rule: selected })}>Edit first</Button
          >
          <Button
            variant="quiet"
            size="sm"
            disabled={busy}
            onclick={() => void change(selected, { kind: 'reject' })}>Reject</Button
          >
        {:else if selected.status === 'superseded' && selected.supersededBy !== null}
          {@const by = selected.supersededBy}
          <Button size="sm" onclick={() => show(by)}>Show {ruleLabel(by)}</Button>
        {:else if selected.approval === 'approved'}
          <Button
            size="sm"
            disabled={busy}
            onclick={() => (dialog = { kind: 'edit', rule: selected })}>Edit</Button
          >
          <Button
            size="sm"
            disabled={busy}
            onclick={() =>
              void change(selected, {
                kind: selected.status === 'disabled' ? 'enable' : 'disable',
              })}
          >
            {selected.status === 'disabled' ? 'Turn on' : 'Disable'}
          </Button>
          <Button
            size="sm"
            disabled={busy}
            onclick={() => (dialog = { kind: 'replace', rule: selected })}>Replace…</Button
          >
        {/if}
      </div>

      <h4 class="c-knowledge__rule">{draft.text}</h4>
      {#if draft.rationale}<p class="c-knowledge__why">{draft.rationale}</p>{/if}

      <dl class="o-facts c-knowledge__facts">
        <dt>Kind</dt>
        <dd>
          <span
            class="c-rule-row__kind"
            class:c-rule-row__kind--required={draft.kind === 'required'}
            >{KIND_WORDS[draft.kind].label}</span
          >
          · {KIND_WORDS[draft.kind].help}
        </dd>
        <dt>Scope</dt>
        <dd>
          {#each scopeParts(draft, projectName) as part, i (i)}{i > 0
              ? ' · '
              : ''}{#if part.code}<code>{part.text}</code>{:else}{part.text}{/if}{/each}
        </dd>
        {#if draft.examples.some((e) => e.stance === 'prefer')}
          <dt>Prefer</dt>
          <dd class="c-knowledge__examples">
            {#each draft.examples.filter((e) => e.stance === 'prefer') as example, i (i)}
              <code>{example.text}</code>
            {/each}
          </dd>
        {/if}
        {#if draft.examples.some((e) => e.stance === 'avoid')}
          <dt>Avoid</dt>
          <dd class="c-knowledge__examples">
            {#each draft.examples.filter((e) => e.stance === 'avoid') as example, i (i)}
              <code>{example.text}</code>
            {/each}
          </dd>
        {/if}
        {#if draft.references.length > 0}
          <dt>References</dt>
          <dd class="c-knowledge__examples">
            {#each draft.references as reference, i (i)}<span>{reference}</span>{/each}
          </dd>
        {/if}
        <dt>Origin</dt>
        <dd>
          {originWords(selected)}{selected.origin === 'agent'
            ? ' · waits for your approval'
            : ' · active on save'}
        </dd>
        <dt>Applied</dt>
        <dd>{appliedWords(selected)}</dd>
      </dl>

      <section class="c-knowledge__history" aria-labelledby="knowledge-history">
        <h5 id="knowledge-history" class="c-knowledge__subhead">History</h5>
        <ol class="o-plain-list c-knowledge__revisions">
          {#each [...selected.revisions].reverse() as entry (entry.number)}
            <li class="c-revision" class:is-current={entry.number === revision.number}>
              <span class="c-revision__meta"
                >v{entry.number} · {dayWords(entry.at)} · {revisionAuthor(
                  selected,
                  entry,
                )}</span
              >
              <span class="c-revision__text">{entry.draft.text}</span>
            </li>
          {/each}
        </ol>
      </section>
    {:else if rules !== null && rules.length > 0}
      <p class="c-knowledge__empty">Choose a rule to see its scope and history.</p>
    {/if}

    <section class="c-knowledge__sources" aria-labelledby="knowledge-sources">
      <h5 id="knowledge-sources" class="c-knowledge__subhead">
        Repository instructions outrank private rules
      </h5>
      {#if sourcesError}
        <p class="c-knowledge__prose">
          Could not read the instruction sources: {sourcesError}
        </p>
      {:else if sources}
        <p class="c-knowledge__prose">
          Reviews also read
          {#each sources.patterns as pattern, i (pattern)}<code>{pattern}</code>{i <
            sources.patterns.length - 2
              ? ', '
              : i === sources.patterns.length - 2
                ? ' and '
                : ''}{/each}{sources.patterns.length === 0 ? 'nothing' : ''}. Those belong
          to the team; wtm reads them and never edits them. When a rule disagrees with them,
          the review reports a conflict instead of choosing.
        </p>
        <p class="c-knowledge__found">
          {#if sources.documents.length > 0}
            Found {#each sources.documents as document, i (document.path)}<code
                >{document.path}</code
              >{document.truncated ? ' (long, so cut)' : ''}{i <
              sources.documents.length - 1
                ? ', '
                : ''}{/each}.
          {:else}
            No instruction files found in this repository.
          {/if}
          {#if sources.unmatched.length > 0}
            Nothing matches {sources.unmatched.join(', ')}.
          {/if}
          {#if sources.incomplete}
            The repository's file list was cut short, so some may be missing here.
          {/if}
        </p>
        {#each sources.skipped as skipped (skipped.path)}
          <p class="c-knowledge__found c-status--warn">
            <code>{skipped.path}</code> is not read: {skipped.reason}.
          </p>
        {/each}
      {:else}
        <p class="c-knowledge__prose">Reading the instruction sources…</p>
      {/if}
      {#if showSourcesHelp}
        <p class="c-field__help">
          Set <code>instruction_sources</code> under <code>[workflow]</code>, either in this
          repository's <code>wtm.toml</code> or for this project only in
          <code>~/.config/wtm/config.toml</code> under
          <code>[projects."{projectId}".config.workflow]</code>.
          {sources?.configured
            ? 'This project sets its own list.'
            : 'This project uses the default list.'}
        </p>
      {/if}
      <div class="c-knowledge__source-actions">
        <Button
          size="sm"
          ariaExpanded={showSourcesHelp}
          onclick={() => (showSourcesHelp = !showSourcesHelp)}
          >Edit instruction sources</Button
        >
        <Button size="sm" disabled>Propose this rule to the repository · later</Button>
      </div>
    </section>
  </section>
</div>

{#if dialog}
  <RuleDialog
    {projectId}
    {projectName}
    mode={dialog}
    onclose={() => (dialog = null)}
    onsaved={(rule) => void saved(rule)}
  />
{/if}
