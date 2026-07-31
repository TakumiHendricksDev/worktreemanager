<script lang="ts">
  /**
   * A labelled form field and the messages beneath it.
   *
   * # The message order is the component's job
   *
   * It is not arbitrary and a rewrite would silently reorder it:
   *
   *   1. the inert note, **or** the normalized preview — never both, because showing
   *      `1234 → ACME-1234` beside a field that no longer names anything is exactly the false
   *      impression the inert note exists to dispel;
   *   2. errors, so a validation failure is never pushed below explanatory prose;
   *   3. help last, because it is the thing you read when nothing is wrong.
   *
   * # `id` is required
   *
   * Rather than generated, so the caller cannot let the `<label for>` and the control's `id`
   * drift apart — a label that points at nothing is invisible until someone tries the app with
   * a screen reader or clicks the label expecting focus.
   */
  import type { Snippet } from 'svelte';

  const {
    id,
    label,
    required = false,
    help,
    errors = [],
    note,
    inert = false,
    inertReason,
    children,
  }: {
    id: string;
    label: string;
    required?: boolean;
    help?: string | null;
    /** Falsy entries are dropped, so a caller can pass optional values positionally. */
    errors?: (string | null | undefined | false)[];
    /** The effective value after a `normalize` template ran, when it differs from the input. */
    note?: string | null;
    /** The field's value no longer affects the outcome. Dimmed, still editable. */
    inert?: boolean;
    inertReason?: string;
    children: Snippet;
  } = $props();

  const shown = $derived(
    errors.filter((e): e is string => typeof e === 'string' && e !== ''),
  );
</script>

<div class="c-field" class:is-inert={inert}>
  <label class="c-field__label" for={id}>
    {label}
    {#if required}<span class="c-field__required" aria-label="required">*</span>{/if}
  </label>

  {@render children()}

  {#if inert && inertReason}
    <p class="c-field__help">{inertReason}</p>
  {:else if note}
    <p class="c-field__note">→ <code>{note}</code></p>
  {/if}

  {#each shown as message (message)}
    <p class="c-field__help c-status--danger">{message}</p>
  {/each}

  {#if help}
    <p class="c-field__help">{help}</p>
  {/if}
</div>
