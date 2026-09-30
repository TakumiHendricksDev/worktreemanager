<script lang="ts">
  /**
   * The worktree's configured links, as a split button: the one last opened on the left, the others
   * in a menu on the right.
   *
   * # Why it left the ⋯ menu
   *
   * Links were a submenu there, so opening one was ⋯, then Links, then the row — three steps for
   * what is usually the same link every time, the ticket or the pull request. Now that one is a
   * single click and any other is two. It borrows Open in's shape because it is the same idea: a
   * remembered choice beside the list it came from.
   *
   * # A native menu, not the `<select>` Open in uses
   *
   * The menu lists only the *other* links, so the current one is never re-picked and there is no
   * sentinel to reset, which is the reason `OpenInButton` needs one. It is also the menu these rows
   * already lived in under ⋯, so they read exactly as they did before they moved.
   *
   * With one link there is nothing to put in a menu, so the button has no menu half; with none the
   * worktree bar leaves it out. In a narrow bar the label goes and the icon stays, which is why the
   * name is also in `aria-label` and the tooltip — see `$bar-links-fold`.
   */
  import { commands } from '../ipc/commands';
  import type { Link } from '../ipc/types';
  import { item, popUp, under } from '../native-menu';
  import { workspace } from '../state/workspace.svelte';
  import Icon from './ui/Icon.svelte';

  const { projectId, links }: { projectId: string; links: Link[] } = $props();

  /**
   * Matched by label, falling back to the first. A remembered label the config has since dropped
   * is simply not found, and the button shows the first link rather than nothing.
   */
  const primary = $derived.by(() => {
    const label = workspace.lastLink(projectId);
    return links.find((link) => link.label === label) ?? links[0] ?? null;
  });

  // By identity rather than by label, so two links a config happens to give the same name both
  // stay reachable.
  const rest = $derived(links.filter((link) => link !== primary));

  function open(link: Link) {
    workspace.rememberLink(projectId, link.label);
    // The scheme is validated in Rust, and there is nothing useful to do if the OS declines.
    void commands.openUrl(link.url).catch(() => {});
  }

  function menu(event: MouseEvent) {
    const button = event.currentTarget as HTMLElement;
    const entries = rest.map((link) =>
      item(`${link.label} — ${link.url}`, () => open(link)),
    );
    // Under the whole control rather than the chevron, so the menu lines up with the label it will
    // replace — the way a macOS pull-down opens under its button.
    void popUp(entries, under(button.parentElement ?? button));
  }
</script>

{#if primary}
  <div class="c-split-button">
    <button
      class="c-split-button__action c-split-button__action--link"
      onclick={() => open(primary)}
      title="{primary.label} — {primary.url}"
      aria-label="Open {primary.label}"
    >
      <Icon name="external" size={11} />
      <span class="c-split-button__label">{primary.label}</span>
    </button>
    {#if rest.length > 0}
      <button
        class="c-split-button__menu"
        onclick={menu}
        title="Other links"
        aria-label="Other links for this worktree"
        aria-haspopup="menu"
      >
        <Icon name="chevron-down" size={12} />
      </button>
    {/if}
  </div>
{/if}
