<script lang="ts">
  /**
   * The rows offered while `@` or `/` is open, inside the composer card.
   *
   * # Why this is in the flow and not floating
   *
   * Every other app puts this menu *over* the conversation. Doing that here would need a third
   * stacking level, and `settings/_config.scss` says the app has two — a scrim and a dialog — and
   * that nothing else may add one. Following the rule instead of arguing with it turned out better
   * than the thing it ruled out:
   *
   *   * no `z-index`, no portal, no outside-click handler, no focus trap, no repositioning on
   *     scroll or resize — a floating menu needs all six, and each is its own bug;
   *   * the composer card already lights as one unit on `:focus-within`, so a strip inside it reads
   *     as part of the field being typed in rather than as a separate surface;
   *   * it pushes the transcript up instead of covering it. The transcript is the content. Hiding
   *     it to help someone write a message about it is backwards.
   *
   * # Why there is no keyboard handling here
   *
   * The keys arrive at the textarea, which is where focus is and stays — this element is never
   * focused at all. `SessionPane` owns the arrow keys and Enter and passes down the resulting
   * index. Splitting it the other way would mean moving focus into the list, which loses the caret
   * and is the reason so many of these menus feel broken.
   */
  import type { Suggestion } from '../suggest';

  const {
    id,
    items,
    active,
    onpick,
  }: {
    /** Ties the textarea's `aria-controls` and `aria-activedescendant` to these rows. */
    id: string;
    items: Suggestion[];
    /** Which row the arrow keys are on. Always a valid index — the caller clamps. */
    active: number;
    onpick: (value: string) => void;
  } = $props();

  let list = $state<HTMLElement | null>(null);

  /**
   * Keep the arrow-key selection visible.
   *
   * `nearest` rather than the default `center`: the list is short and centring makes it jump half
   * its own height on every keypress, which reads as the rows moving rather than the selection.
   */
  $effect(() => {
    const row = list?.children[active];
    if (row instanceof HTMLElement) row.scrollIntoView({ block: 'nearest' });
  });
</script>

<div class="c-suggest" {id} role="listbox" aria-label="Suggestions" bind:this={list}>
  {#each items as item, i (item.value)}
    <!--
      `onmousedown` with `preventDefault`, not `onclick`. A click blurs the textarea first, which
      moves the caret out of the draft this is about to edit — and on some engines closes the strip
      before the click lands at all. This is the same trick `OpenInButton` uses on its menu.

      Not a `<button>`: a listbox option is not a button, and making it one would put every row in
      the tab order in front of the Send button. `tabindex="-1"` for the same reason — focus never
      comes here at all. The textarea keeps it and names the active row with
      `aria-activedescendant`, which is what keeps the caret alive while the list is walked.
    -->
    <div
      class="c-suggest__row"
      class:is-active={i === active}
      id="{id}-{i}"
      role="option"
      aria-selected={i === active}
      tabindex="-1"
      onmousedown={(event) => {
        event.preventDefault();
        onpick(item.value);
      }}
    >
      <span class="c-suggest__label">{item.label}</span>
      {#if item.detail !== ''}
        <span class="c-suggest__detail">{item.detail}</span>
      {/if}
    </div>
  {/each}
</div>
