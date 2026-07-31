<script lang="ts">
  /**
   * The reasons an operation might not be a good idea, and the acknowledgements that let it
   * proceed anyway.
   *
   * Rendered by both the create review and the remove dialog, which had ~25 near-identical
   * lines each. The only real difference between them was the override's wording.
   *
   * `acknowledged` is controlled rather than bound: it belongs to whoever owns the operation,
   * which also has to decide whether the Create/Remove button is enabled, and splitting that
   * across two owners is how the list and the button end up disagreeing.
   */
  import type { Preflight } from '../../ipc/types';
  import Choice from './Choice.svelte';
  import Icon from './Icon.svelte';

  const {
    items,
    acknowledged = [],
    overrideLabel,
    disabled = false,
    onacknowledge,
  }: {
    items: Preflight[];
    /** Ids of the errors the user has ticked. */
    acknowledged?: string[];
    /** "Do it anyway" when creating, "Remove anyway" when removing. */
    overrideLabel: string;
    disabled?: boolean;
    onacknowledge?: (id: string, acknowledged: boolean) => void;
  } = $props();

  // Errors first, then warnings: a warning below an error reads as less urgent, which is
  // correct, and the reverse buries the thing that actually blocks you.
  const errors = $derived(items.filter((i) => i.severity === 'error'));
  const warns = $derived(items.filter((i) => i.severity === 'warn'));
</script>

<ul class="c-preflight o-plain-list">
  {#each errors as item (item.id)}
    <li class="c-preflight__item">
      <span class="c-preflight__message c-status--danger">
        <Icon name="close" size={14} />
        {item.message}
      </span>
      {#if item.hint}<span class="c-preflight__hint">{item.hint}</span>{/if}
      {#if item.overridable}
        <Choice
          size="sm"
          {disabled}
          checked={acknowledged.includes(item.id)}
          onchange={(checked) => onacknowledge?.(item.id, checked)}
        >
          {overrideLabel}
        </Choice>
      {/if}
    </li>
  {/each}

  {#each warns as item (item.id)}
    <li class="c-preflight__item">
      <span class="c-preflight__message c-status--warn">! {item.message}</span>
      {#if item.hint}<span class="c-preflight__hint">{item.hint}</span>{/if}
    </li>
  {/each}
</ul>
