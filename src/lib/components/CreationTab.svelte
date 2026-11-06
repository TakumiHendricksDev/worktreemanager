<script lang="ts">
  import ExtensionPoint from './ExtensionPoint.svelte';
  import { extensionPart, extensionContext } from '../state/extensions.svelte';
  const publicContext = extensionContext();
  /**
   * A worktree being made, standing in the list where it will land.
   *
   * The real tab's markup — key, name, mono branch, a flags line of a dot and a word — so the ghost
   * reads as "this worktree, not yet" rather than as some other kind of row, and is replaced in
   * place by the tab it was standing in for. Hollow (a dashed outline) rather than dimmed: dimmed
   * text says "you cannot click this", and clicking it is the way back to the run's progress.
   *
   * A treeitem in the sidebar's tree, keyed `ghost:<id>` so the arrow keys reach it; it has no
   * context menu and cannot be dragged, because there is no worktree yet to act on.
   */
  import { worktreeLabel } from '../worktree-label';
  import { currentStep, identityOf, type Creation } from '../state/creations.svelte';
  import Icon from './ui/Icon.svelte';
  import SessionDot from './ui/SessionDot.svelte';

  const {
    creation,
    selected,
    onselect,
    ondiscard,
  }: {
    creation: Creation;
    selected: boolean;
    onselect: () => void;
    ondiscard: () => void;
  } = $props();

  const identity = $derived(worktreeLabel(identityOf(creation)));
  const step = $derived(currentStep(creation));
  const running = $derived(creation.phase === 'running');
  /** Partway through the step it is on, so the bar moves when a stage starts, not only after. */
  const percent = $derived(
    step && step.total > 0 ? Math.round(((step.index - 0.5) / step.total) * 100) : 4,
  );
  const word = $derived(
    running
      ? step
        ? `creating · ${step.index}/${step.total} ${step.label}`
        : 'creating · starting'
      : creation.phase === 'refused'
        ? 'couldn’t create'
        : 'cancelled',
  );
</script>

<ExtensionPoint target="component.creation-tab" context={publicContext()}>
  <div
    use:extensionPart={{ target: 'component.creation-tab', context: publicContext }}
    class="c-worktree-tab c-worktree-tab--ghost"
    class:c-worktree-tab--failed={!running}
    role="presentation"
  >
    <button
      role="treeitem"
      aria-selected={selected}
      aria-level={1}
      tabindex={selected ? 0 : -1}
      class="c-worktree-tab__button"
      class:is-selected={selected}
      data-nav="ghost:{creation.id}"
      title={running ? 'Being created — click for its progress' : (creation.error ?? word)}
      onclick={onselect}
    >
      <span class="c-worktree-tab__line">
        {#if identity.key}<span class="c-worktree-tab__key">{identity.key}</span>{/if}
        <span class="c-worktree-tab__name">{identity.name}</span>
      </span>
      <span class="c-worktree-tab__line c-worktree-tab__meta">
        <span class="c-worktree-tab__branch">{creation.branch ?? creation.dirname}</span>
      </span>
      <span class="c-worktree-tab__line c-worktree-tab__flags">
        {#if running}
          <span class="c-status--muted c-worktree-tab__flag"
            ><SessionDot status="working" />&nbsp;{word}</span
          >
        {:else}
          <span class="c-status--danger c-worktree-tab__flag">✕&nbsp;{word}</span>
        {/if}
      </span>
      {#if running}
        <span class="c-worktree-tab__progress" aria-hidden="true">
          <span class="c-worktree-tab__progress-fill" style:width="{percent}%"></span>
        </span>
      {/if}
    </button>
    {#if !running}
      <button
        class="c-worktree-tab__close"
        type="button"
        title="Discard this attempt"
        aria-label="Discard this attempt"
        onclick={ondiscard}
      >
        <Icon name="close" size={12} />
      </button>
    {/if}
  </div>
</ExtensionPoint>
