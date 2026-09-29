<script lang="ts">
  /**
   * One group in the sidebar: a header you can fold, rename and drag, over its rows.
   *
   * # Why the whole section is the tree item
   *
   * The ARIA tree pattern nests: a parent `treeitem` owns a `group` of child items, and
   * `aria-expanded` on the parent is what a screen reader reads as open or closed. The header
   * therefore cannot be the tree item on its own without `aria-owns` to reach the rows beside it,
   * which WebKit does not reliably honour. So the wrapper is the item and takes focus, and its
   * accessible name is set explicitly — otherwise it would be the text of every row inside it.
   *
   * The cost is that a focus ring on the wrapper would circle the whole group. The stylesheet moves
   * the ring onto the header instead; see `_sidebar-group.scss`.
   *
   * # Why the header has no keyboard handler of its own
   *
   * Keyboard focus lands on the tree item, not the header, and the tree's own handler in
   * `Sidebar.svelte` is what answers Enter, Space and the arrows for every item. The header's pointer
   * handlers are the mouse half of the same actions, which is why the a11y rules that expect a key
   * handler beside a click one are silenced on it rather than satisfied twice.
   */
  import type { Snippet } from 'svelte';

  import type { Section } from '../sidebar';
  import { STATUS_NAME, type PaneStatus } from '../status';
  import Icon from './ui/Icon.svelte';
  import SessionDot from './ui/SessionDot.svelte';

  const {
    section,
    status,
    renaming,
    droptarget,
    dragging,
    children,
    ontoggle,
    onrename,
    oncancelrename,
    onpress,
    onmenu,
  }: {
    section: Section;
    /** The most urgent status among the rows the fold hides, or null. */
    status: PaneStatus | null;
    renaming: boolean;
    /** A dragged row would land in this group. */
    droptarget: boolean;
    /** This group's header is the one being dragged. */
    dragging: boolean;
    children: Snippet;
    ontoggle: () => void;
    onrename: (name: string) => void;
    oncancelrename: () => void;
    /** A press on the header that may become a drag. Only custom groups move. */
    onpress: (event: PointerEvent) => void;
    onmenu: (event: MouseEvent) => void;
  } = $props();

  const count = $derived(section.members.length);
  /**
   * The tree item's accessible name, spelled out because the default — its text content — would
   * be every row inside it. The count and the status are in it for the same reason they are on
   * screen: a folded group has to say what it is hiding.
   */
  const name = $derived(
    [
      section.label,
      section.open ? null : `${count} ${count === 1 ? 'worktree' : 'worktrees'}`,
      status ? STATUS_NAME[status] : null,
    ]
      .filter(Boolean)
      .join(', '),
  );

  let input = $state<HTMLInputElement | null>(null);
  /** Set by Escape so the blur that follows it does not commit what was just cancelled. */
  let cancelled = false;

  $effect(() => {
    if (renaming && input) {
      cancelled = false;
      input.focus();
      input.select();
    }
  });

  function onRenameKeydown(event: KeyboardEvent) {
    // Nothing typed here is the tree's business: a Space or an arrow is text.
    event.stopPropagation();
    if (event.key === 'Enter') {
      event.preventDefault();
      input?.blur();
    } else if (event.key === 'Escape') {
      event.preventDefault();
      cancelled = true;
      oncancelrename();
    }
  }

  function onRenameBlur() {
    if (cancelled || !input) return;
    onrename(input.value);
  }
</script>

<div
  class="c-sidebar-group"
  class:is-drop-target={droptarget}
  class:is-dragging={dragging}
  role="treeitem"
  aria-level={1}
  aria-expanded={section.open}
  aria-selected={false}
  aria-label={name}
  tabindex="-1"
  data-nav={`group:${section.id}`}
  data-group-slot={section.kind === 'custom' ? section.id : undefined}
>
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div
    class="c-sidebar-group__head"
    data-slot="header"
    data-group={section.id}
    data-first={section.members[0]?.id ?? ''}
    onclick={renaming ? undefined : ontoggle}
    onpointerdown={renaming ? undefined : onpress}
    oncontextmenu={onmenu}
  >
    <span class="c-sidebar-group__chevron">
      <Icon name={section.open ? 'chevron-down' : 'chevron-right'} size={12} />
    </span>
    {#if renaming}
      <input
        bind:this={input}
        class="c-sidebar-group__rename"
        value={section.kind === 'custom' ? section.label : ''}
        aria-label="Group name"
        maxlength={80}
        autocomplete="off"
        spellcheck="false"
        onkeydown={onRenameKeydown}
        onblur={onRenameBlur}
        onclick={(event) => event.stopPropagation()}
      />
    {:else}
      <span class="c-sidebar-group__name" title={section.label}>{section.label}</span>
    {/if}
    {#if !section.open && count > 0}
      <span class="c-sidebar-group__count" aria-hidden="true">{count}</span>
    {/if}
    {#if status}
      <!-- Unlabelled: the tree item's name already says it, and would hide this one's anyway. -->
      <SessionDot {status} />
    {/if}
  </div>

  <div class="c-sidebar-group__rows" role="group">
    {@render children()}
  </div>
</div>
