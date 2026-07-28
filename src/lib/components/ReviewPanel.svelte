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

<div class="review">
  {#if preview.branchChoices.length > 0}
    <section>
      <h3>An existing branch matches</h3>
      <p class="note">
        Adopt one instead of creating a new branch. This replaces the numbered prompt the
        shell script would have shown.
      </p>
      <label class="radio">
        <input type="radio" checked={adoptBranch === null} onchange={() => onadopt(null)} />
        <span>Create a new branch</span>
      </label>
      {#each preview.branchChoices as choice (choice.branch)}
        <label class="radio">
          <input
            type="radio"
            checked={adoptBranch === choice.branch}
            onchange={() => onadopt(choice.branch)}
          />
          <span><code>{label(choice)}</code></span>
        </label>
      {/each}
    </section>
  {/if}

  <section>
    <h3>What will be created</h3>
    <dl>
      <dt>Branch</dt>
      <dd>
        {#if preview.branch}<code>{preview.branch}</code>{:else}<span class="muted"
            >detached</span
          >{/if}
      </dd>
      <dt>Directory</dt>
      <dd><code>{preview.directory}</code></dd>
      <dt>Base</dt>
      <dd>
        <code>{preview.baseRef}</code>
        {#if preview.baseCommit}
          <span class="muted">at {preview.baseCommit}</span>
        {:else}
          <span class="danger">does not resolve</span>
        {/if}
        {#if preview.willFetch}<span class="muted">· will fetch first</span>{/if}
      </dd>
    </dl>
  </section>

  {#if Object.keys(preview.lookups).length > 0 || Object.keys(preview.computed).length > 0}
    <section>
      <h3>Resolved values</h3>
      <dl class="tokens">
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
    <h3>Commands</h3>
    <pre class="argv">{preview.gitArgv.join(' ')}</pre>
    {#if preview.setupArgv}
      <pre class="argv">{preview.setupArgv.join(' ')}</pre>
      <p class="note">
        Setup runs in <code>{preview.setupCwd}</code>
      </p>
    {:else}
      <p class="note">This project declares no setup command.</p>
    {/if}
  </section>

  {#if preview.warnings.length > 0}
    <section>
      <h3>Warnings</h3>
      <ul class="plain">
        {#each preview.warnings as warning, i (i)}
          <li class="warn">{warning}</li>
        {/each}
      </ul>
    </section>
  {/if}

  {#if errors.length > 0 || warns.length > 0}
    <section>
      <h3>Preflight</h3>
      <ul class="plain">
        {#each errors as item (item.id)}
          <li class="check">
            <span class="danger">✗ {item.message}</span>
            {#if item.hint}<span class="hint">{item.hint}</span>{/if}
            {#if item.overridable}
              <label class="ack">
                <input
                  type="checkbox"
                  checked={acknowledged.includes(item.id)}
                  onchange={(e) =>
                    toggle(item, (e.currentTarget as HTMLInputElement).checked)}
                />
                <span>Do it anyway</span>
              </label>
            {/if}
          </li>
        {/each}
        {#each warns as item (item.id)}
          <li class="check">
            <span class="warn">! {item.message}</span>
            {#if item.hint}<span class="hint">{item.hint}</span>{/if}
          </li>
        {/each}
      </ul>
    </section>
  {/if}
</div>

<style>
  .review {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }

  h3 {
    font-size: var(--step--1);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-muted);
    margin-bottom: var(--sp-2);
  }

  dl {
    display: grid;
    grid-template-columns: minmax(80px, max-content) 1fr;
    gap: var(--sp-1) var(--sp-4);
    align-items: baseline;
  }

  dl.tokens {
    grid-template-columns: minmax(150px, max-content) 1fr;
  }

  dt {
    color: var(--fg-muted);
    font-size: var(--step--1);
  }

  dd {
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .argv {
    background: var(--bg-code);
    border-radius: var(--r-md);
    padding: var(--sp-2) var(--sp-3);
    font-size: var(--step--2);
    /* An argv must scroll, never wrap: a wrapped command is easy to misread. */
    overflow-x: auto;
    white-space: pre;
    margin-bottom: var(--sp-2);
  }

  .note {
    font-size: var(--step--2);
    color: var(--fg-muted);
    line-height: 1.55;
  }

  .plain {
    list-style: none;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }

  .check {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: var(--step--1);
  }

  .hint {
    color: var(--fg-muted);
    font-size: var(--step--2);
    line-height: 1.5;
  }

  .radio,
  .ack {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--step--1);
    padding: 2px 0;
  }

  .ack {
    color: var(--fg-muted);
    font-size: var(--step--2);
  }

  .muted {
    color: var(--fg-muted);
  }
  .warn {
    color: var(--warn);
  }
  .danger {
    color: var(--danger);
  }
</style>
