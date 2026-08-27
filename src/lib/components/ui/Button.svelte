<script lang="ts">
  /**
   * The app's button.
   *
   * `variant` and `size` are union types rather than free-form class strings, deliberately.
   * With the stylesheet global there are no scoped `<style>` blocks and therefore no
   * unused-selector warnings, so a mistyped class name is invisible to every tool in the
   * repository. A typed prop is the one place that can still catch it, which is why the class
   * contract lives here and not in the markup.
   *
   * See `styles/components/_button.scss` for what each variant is for.
   */
  import type { Snippet } from 'svelte';

  const {
    variant = 'neutral',
    size = 'md',
    type = 'button',
    disabled = false,
    full = false,
    icon = false,
    title,
    ariaLabel,
    ariaExpanded,
    ariaPressed,
    ariaControls,
    onclick,
    children,
  }: {
    variant?:
      | 'accent'
      | 'neutral'
      | 'danger-outline'
      | 'danger-solid'
      | 'quiet'
      | 'link'
      | 'inline';
    size?: 'sm' | 'md' | 'lg';
    /**
     * Defaults to `button`, which is the opposite of the HTML default and is the point.
     * A bare `<button>` inside a `<form>` submits it — the Cancel button in the Add a
     * repository dialog had to say `type="button"` for exactly that reason. Defaulting the
     * safe way makes that class of bug impossible and costs the one real submit button an
     * explicit `type="submit"`.
     */
    type?: 'button' | 'submit';
    disabled?: boolean;
    full?: boolean;
    /** Square and icon-only. Requires `ariaLabel` — there is no text to name it. */
    icon?: false | 'md' | 'sm';
    title?: string;
    /** For an icon-only button, where there is no text to name it. */
    ariaLabel?: string;
    /**
     * For a disclosure: whether the region named by `ariaControls` is currently on screen.
     *
     * Here rather than at the call site because the alternative is hand-writing
     * `class="c-button c-button--quiet c-button--sm"` alongside the aria attributes, which
     * gives up the one mechanism that catches a mistyped class name — see the note above.
     */
    ariaExpanded?: boolean;
    /** For two-state view toggles such as Sessions / Database. */
    ariaPressed?: boolean;
    /** The id of the region a disclosure toggles. */
    ariaControls?: string;
    onclick?: (event: MouseEvent) => void;
    children: Snippet;
  } = $props();
</script>

<button
  {type}
  {disabled}
  {title}
  {onclick}
  aria-label={ariaLabel}
  aria-expanded={ariaExpanded}
  aria-pressed={ariaPressed}
  aria-controls={ariaControls}
  class="c-button c-button--{variant} c-button--{size}"
  class:c-button--full={full}
  class:c-button--icon={icon === 'md'}
  class:c-button--icon-sm={icon === 'sm'}
>
  {@render children()}
</button>
