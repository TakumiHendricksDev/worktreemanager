<script lang="ts">
  /**
   * A single-line text input.
   *
   * `element` exists because two callers need the node itself — one to focus it on mount, one
   * to return focus to it after the folder picker closes. Exposing it as a bindable prop is
   * the alternative to those callers reaching in with `querySelector`.
   */
  // `let`, not `const`: a `$bindable` prop is written by the parent, and Svelte rejects a
  // binding to a `const` destructuring.
  let {
    id,
    value = $bindable(''),
    type = 'text',
    mono = false,
    placeholder,
    disabled = false,
    element = $bindable(null),
    oninput,
  }: {
    id?: string;
    value?: string;
    /** `search` gets the platform's clear affordance; `number` gets its stepper. */
    type?: 'text' | 'search' | 'number';
    /** For a value read character by character — a path, a hash. */
    mono?: boolean;
    placeholder?: string;
    disabled?: boolean;
    element?: HTMLInputElement | null;
    oninput?: (event: Event) => void;
  } = $props();
</script>

<!--
  `type` is spread rather than bound because Svelte cannot two-way bind `value` on an input
  whose `type` is dynamic — it has no way to know whether to coerce to a number.
-->
<input
  {id}
  {...{ type }}
  {placeholder}
  {disabled}
  {oninput}
  bind:this={element}
  bind:value
  class="c-input"
  class:c-input--mono={mono}
  autocapitalize="off"
  autocorrect="off"
  spellcheck="false"
/>
