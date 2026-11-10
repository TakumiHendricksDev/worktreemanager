<script lang="ts">
  /**
   * The review screen.
   *
   * Shows the *exact* argv that will run, not a paraphrase, because "trust me" is not a
   * review. That is only honest because of the pipeline's invariant: stages 1–6 mutate
   * nothing, and the preview is produced by the same code that will execute — so what is
   * printed here is what runs.
   *
   * `setupCwd` is shown deliberately. A project's setup command often has to run from the
   * repository root rather than the new worktree, and that is surprising enough to state
   * rather than hide.
   */
  import type { BranchChoice, Preflight, Preview } from '../ipc/types';
  import Choice from './ui/Choice.svelte';
  import PreflightList from './ui/PreflightList.svelte';

  const {
    preview,
    acknowledged,
    adoptBranch,
    onadopt,
  }: {
    preview: Preview;
    acknowledged: string[];
    adoptBranch: string | null;
    onadopt: (branch: string | null) => void;
  } = $props();

  const errors = $derived(preview.preflight.filter((p) => p.severity === 'error'));
  const warns = $derived(preview.preflight.filter((p) => p.severity === 'warn'));

  function toggle(item: Preflight, on: boolean) {
    const index = acknowledged.indexOf(item.id);
    if (on && index === -1) acknowledged.push(item.id);
    if (!on && index !== -1) acknowledged.splice(index, 1);
  }

  function label(choice: BranchChoice): string {
    return choice.remoteOnly ? `${choice.branch}  (remote only)` : choice.branch;
  }
</script>

<div class="o-stack o-stack--loose">
  {#if preview.branchChoices.length > 0}
    <section>
      <h3 class="c-section-heading">An existing branch matches</h3>
      <p class="c-note">
        Adopt one instead of creating a new branch. This replaces the numbered prompt the
        shell script would have shown.
      </p>
      <Choice
        type="radio"
        name="adopt-branch"
        checked={adoptBranch === null}
        onchange={() => onadopt(null)}
      >
        Create a new branch
      </Choice>
      {#each preview.branchChoices as choice (choice.branch)}
        <Choice
          type="radio"
          name="adopt-branch"
          checked={adoptBranch === choice.branch}
          onchange={() => onadopt(choice.branch)}
        >
          <code>{label(choice)}</code>
        </Choice>
      {/each}
    </section>
  {/if}

  <section>
    <h3 class="c-section-heading">What will be created</h3>
    <dl class="o-facts">
      <!--
        Both of these come from the adopted branch rather than from the naming templates, and
        saying so here is what connects the radio button above to the dimmed fields on the
        left. Without it the values simply change and you are left to infer why.
      -->
      <dt>Branch</dt>
      <dd>
        {#if preview.branch}<code>{preview.branch}</code>{:else}<span
            class="c-status--muted">detached</span
          >{/if}
        {#if adoptBranch}<span class="c-review__from">existing</span>{/if}
      </dd>
      <dt>Directory</dt>
      <dd>
        <code>{preview.directory}</code>
        {#if adoptBranch}<span class="c-review__from">from the branch</span>{/if}
      </dd>
      <dt>Base</dt>
      <dd>
        <code>{preview.baseRef}</code>
        {#if preview.baseCommit}
          <span class="c-status--muted">at {preview.baseCommit}</span>
        {:else}
          <span class="c-status--danger">does not resolve</span>
        {/if}
        {#if preview.willFetch}<span class="c-status--muted">· will fetch first</span>{/if}
      </dd>
    </dl>
  </section>

  {#if Object.keys(preview.lookups).length > 0 || Object.keys(preview.computed).length > 0}
    <section>
      <h3 class="c-section-heading">Resolved values</h3>
      <dl class="o-facts o-facts--wide">
        {#each Object.entries(preview.lookups) as [key, value] (key)}
          <dt><code>{key}</code></dt>
          <dd>{value || '—'}</dd>
        {/each}
        {#each Object.entries(preview.computed) as [key, value] (key)}
          <dt><code>{key}</code></dt>
          <dd>{value || '—'}</dd>
        {/each}
      </dl>
    </section>
  {/if}

  <section>
    <h3 class="c-section-heading">Commands</h3>
    <pre class="c-review__argv">{preview.gitArgv.join(' ')}</pre>
    {#if preview.setupArgv}
      <pre class="c-review__argv">{preview.setupArgv.join(' ')}</pre>
      <p class="c-note">
        Setup runs in <code>{preview.setupCwd}</code>
      </p>
    {:else}
      <p class="c-note">This project declares no setup command.</p>
    {/if}
  </section>

  {#if preview.warnings.length > 0}
    <section>
      <h3 class="c-section-heading">Warnings</h3>
      <ul class="o-plain-list o-stack">
        {#each preview.warnings as warning, i (i)}
          <li class="c-status--warn">{warning}</li>
        {/each}
      </ul>
    </section>
  {/if}

  {#if errors.length > 0 || warns.length > 0}
    <section>
      <h3 class="c-section-heading">Preflight</h3>
      <PreflightList
        items={[...errors, ...warns]}
        {acknowledged}
        overrideLabel="Do it anyway"
        onacknowledge={(id, on) => {
          const item = [...errors, ...warns].find((i) => i.id === id);
          if (item) toggle(item, on);
        }}
      />
    </section>
  {/if}
</div>
