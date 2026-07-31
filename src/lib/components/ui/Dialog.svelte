<script lang="ts">
  /**
   * The modal.
   *
   * Owns the Escape handler and the scrim click, which both dialogs implemented separately and
   * only one of which guarded on a busy state — closing mid-removal was possible in the other.
   * `closeDisabled` is that guard, now in one place.
   *
   * # Not `<dialog>.showModal()`
   *
   * The native element promotes to the top layer, which sits above everything regardless of
   * `z-index`. That invalidates the two stacking levels in `settings/_config.scss` and makes
   * the hand-drawn scrim redundant in a way that would need the whole thing rebuilt rather than
   * adjusted. Worth doing one day; not as a side effect of a class rename.
   *
   * # Known gap, stated rather than hidden
   *
   * This does not trap focus, and does not restore it to the trigger on close, despite
   * `aria-modal="true"` claiming otherwise. Neither dialog did before either. It is a small
   * change now that there is one component instead of two, and it is deliberately not in the
   * same commit as a migration that must not alter behaviour.
   */
  import type { Snippet } from 'svelte';

  import Button from './Button.svelte';
  import Icon from './Icon.svelte';

  const {
    title,
    onclose,
    onsubmit,
    closeDisabled = false,
    wide = false,
    body,
    footer,
  }: {
    title: string;
    onclose: () => void;
    /**
     * Supply for a dialog that asks a question, and the body and footer are wrapped in a
     * `<form>` — which is what makes Enter submit it. Without one, a dialog with a single text
     * field would need the user to reach for the mouse to answer it.
     */
    onsubmit?: (event: Event) => void;
    /** True while an operation is in flight and dismissing would abandon it mid-way. */
    closeDisabled?: boolean;
    wide?: boolean;
    body: Snippet;
    footer: Snippet;
  } = $props();

  function onKeydown(event: KeyboardEvent) {
    if (event.key !== 'Escape' || closeDisabled) return;
    // Stopped so a dialog opened from within another Escape-handling view does not close both.
    event.stopPropagation();
    onclose();
  }

  function onScrim() {
    if (!closeDisabled) onclose();
  }
</script>

<svelte:window onkeydown={onKeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="o-scrim" onclick={onScrim}></div>

<div
  class="c-dialog"
  class:c-dialog--wide={wide}
  role="dialog"
  aria-modal="true"
  aria-label={title}
>
  <div class="c-dialog__head">
    <h2 class="c-dialog__title">{title}</h2>
    <Button
      variant="quiet"
      icon="md"
      disabled={closeDisabled}
      onclick={onclose}
      ariaLabel="Close"
    >
      <Icon name="close" />
    </Button>
  </div>

  {#if onsubmit}
    <!-- `display: contents` so the form participates in no layout: the panel's flex column
         still sees the body and footer as its own children, which is what keeps the body
         scrollable and the footer pinned. -->
    <form {onsubmit} style="display: contents">
      <div class="c-dialog__body">{@render body()}</div>
      <div class="c-dialog__foot">{@render footer()}</div>
    </form>
  {:else}
    <div class="c-dialog__body">{@render body()}</div>
    <div class="c-dialog__foot">{@render footer()}</div>
  {/if}
</div>
