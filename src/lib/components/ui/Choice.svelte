<script lang="ts">
  /**
   * A checkbox or a radio with its label.
   *
   * One component for both because they are the same box — the difference is the input's
   * `type`, and for radios that a set shares a `name`.
   *
   * Controlled rather than bindable: `checked` in, `onchange` out. Every call site already
   * worked that way, and for good reason — the state these represent lives somewhere with
   * rules attached (an array of acknowledged preflight ids, a nullable adopted branch), so a
   * two-way binding would have to be unwound at the call site anyway.
   *
   * The input itself is native. A custom control would have to reimplement the focus ring, the
   * indeterminate state, the platform's press animation and its dark-mode inversion, and the
   * payoff would be that it looks slightly more like the app and slightly less like the
   * operating system — the wrong trade in a developer tool.
   */
  import type { Snippet } from 'svelte';

  const {
    type = 'checkbox',
    checked = false,
    name,
    size = 'md',
    disabled = false,
    onchange,
    children,
  }: {
    type?: 'checkbox' | 'radio';
    checked?: boolean;
    /** Radios only. Sharing one across a set is what gives it native arrow-key navigation. */
    name?: string;
    size?: 'md' | 'sm';
    disabled?: boolean;
    /** Receives the control's new checked state. */
    onchange?: (checked: boolean) => void;
    children: Snippet;
  } = $props();
</script>

<label class="c-choice" class:c-choice--sm={size === 'sm'}>
  <input
    {...{ type }}
    {name}
    {checked}
    {disabled}
    onchange={(event) => onchange?.((event.currentTarget as HTMLInputElement).checked)}
  />
  <span>{@render children()}</span>
</label>
