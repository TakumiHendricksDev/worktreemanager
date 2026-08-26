<script lang="ts" module>
  /**
   * Mounted dialogs, oldest first. Escape and the focus trap belong only to the last entry:
   * two window listeners cannot coordinate via `stopPropagation`.
   */
  let nextDialog = 0;
  const stack: number[] = [];

  function registerDialog(): number {
    const id = nextDialog;
    nextDialog += 1;
    stack.push(id);
    return id;
  }

  function unregisterDialog(id: number): void {
    const index = stack.lastIndexOf(id);
    if (index >= 0) stack.splice(index, 1);
  }

  function topDialog(): number | undefined {
    return stack.at(-1);
  }
</script>

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
   * `z-index`. That invalidates every level in `settings/_config.scss` at once — including the
   * toast stack, which is deliberately *below* the scrim — and makes
   * the hand-drawn scrim redundant in a way that would need the whole thing rebuilt rather than
   * adjusted. Worth doing one day; not as a side effect of a class rename.
   *
   * # The focus trap
   *
   * `aria-modal="true"` is a promise that Tab cannot leave the dialog, and for a long time
   * this component made that promise and did not keep it — neither dialog it replaced had
   * kept it either. Settings is what forced the issue: it is the first dialog with a
   * keyboard route in (⌘, and a menu item), so it is the first where tabbing straight out
   * into the sidebar behind the scrim is something a person will actually do.
   *
   * Two halves, both small. `onFocusin` bounces focus back when it lands outside — which
   * catches Tab, Shift-Tab and a click-through equally, where a keydown handler would only
   * catch the first two. And the element that was focused when the dialog opened is restored
   * on teardown, so closing returns you to the button you pressed rather than to the top of
   * the document.
   */
  import { onDestroy, type Snippet } from 'svelte';

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

  let panel = $state<HTMLElement | null>(null);

  /*
   * Only the topmost dialog traps focus and handles Escape. Two mounted dialogs used to
   * bounce focus into each other forever (⌘I over Settings is the reachable case), and
   * Escape closed both because sibling `window` listeners ignore `stopPropagation`.
   */
  const depth = registerDialog();
  onDestroy(() => unregisterDialog(depth));

  function isTop(): boolean {
    return topDialog() === depth;
  }

  function onKeydown(event: KeyboardEvent) {
    if (!isTop() || event.key !== 'Escape' || closeDisabled) return;
    event.stopImmediatePropagation();
    onclose();
  }

  function onScrim() {
    if (!isTop() || closeDisabled) return;
    onclose();
  }

  /** Everything Tab can reach inside the panel, in document order. */
  function focusables(): HTMLElement[] {
    if (!panel) return [];
    return [
      ...panel.querySelectorAll<HTMLElement>(
        'a[href], button, input, select, textarea, [tabindex]:not([tabindex="-1"])',
      ),
    ].filter((el) => !el.hasAttribute('disabled') && el.offsetParent !== null);
  }

  /*
   * Move focus in on open, and put it back where it was on close.
   *
   * The first focusable rather than the panel itself: the close button is first in the
   * markup, so opening a dialog and pressing Enter dismisses it — which is the behaviour
   * someone who opened it by accident expects. Focusing the panel would instead require a
   * Tab before anything responds to the keyboard.
   *
   * A dialog that focuses something specific — the path field in Add a repository — still
   * wins, because its own `$effect` runs after this one.
   */
  $effect(() => {
    const previous = document.activeElement as HTMLElement | null;
    focusables()[0]?.focus();

    return () => {
      // `isConnected` because the trigger may have been removed by the very action the
      // dialog performed — removing a worktree unmounts the row whose button opened it.
      if (previous?.isConnected) previous.focus();
    };
  });

  /*
   * Bounce focus back when it escapes.
   *
   * A `focusin` listener rather than a Tab keydown handler, because focus leaves a modal by
   * more routes than Tab: Shift-Tab off the front, a click on the scrim, and the browser's
   * own focus restoration after an element is removed. One listener covers all of them.
   *
   * Guarded on the panel existing and on there being something to focus, so a dialog whose
   * controls are all disabled mid-operation does not spin.
   */
  function onFocusIn(event: FocusEvent) {
    if (!isTop()) return;
    const target = event.target as Node | null;
    if (!panel || !target || panel.contains(target)) return;
    const wrap = focusables();
    if (wrap.length === 0) return;

    // Shift-Tab off the first control should land on the last, not bounce to the first.
    const backwards = event.relatedTarget === wrap[0];
    (backwards ? wrap[wrap.length - 1] : wrap[0])?.focus();
  }
</script>

<svelte:window onkeydown={onKeydown} onfocusin={onFocusIn} />

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="o-scrim" onclick={onScrim}></div>

<div
  bind:this={panel}
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
    <form {onsubmit} class="c-dialog__form">
      <div class="c-dialog__body">{@render body()}</div>
      <div class="c-dialog__foot">{@render footer()}</div>
    </form>
  {:else}
    <div class="c-dialog__body">{@render body()}</div>
    <div class="c-dialog__foot">{@render footer()}</div>
  {/if}
</div>
