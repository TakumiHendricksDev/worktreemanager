<script lang="ts">
  /**
   * One row above the sessions: what this worktree is, and what you can do to it.
   *
   * Everything the user asked to keep accessible is here, and the placements are deliberate:
   *
   *   * **Links** is a native `<select>` in the existing `o-overlay-select` idiom — the same pattern
   *     `OpenInButton` uses. A native popup renders outside the stacking context, so a links menu
   *     costs **no `z-index`**, which is what keeps `settings/_config.scss`'s rule intact.
   *   * **Details** is a text button rather than an icon. An "info" glyph is a circle, a bar and a
   *     dot, and at 1.5 stroke on a 16 grid the dot is a smudge — the judgement `icons.ts` already
   *     records for the cog and the terminal box.
   *   * **Open in** and **Remove** keep their position, order and variants, with Remove alone past
   *     the hairline. The gap is not cosmetic: a destructive control flush against a neutral one is
   *     how it gets clicked by accident.
   *
   * Branch and dirty state stay inline because they are the two facts you glance at; the rest moved
   * into the Details dialog.
   */
  import { sessions } from '../state/sessions.svelte';
  import { commands } from '../ipc/commands';
  import { INSPECTOR_SHORTCUT, SHELL_SHORTCUT } from '../state/sessions.svelte';
  import type { Worktree } from '../ipc/types';
  import OpenInButton from './OpenInButton.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    worktree,
    projectId,
    onremove,
    onfavorite,
    oninspect,
  }: {
    worktree: Worktree;
    projectId: string;
    onremove: () => void;
    onfavorite: () => void;
    oninspect: () => void;
  } = $props();

  /**
   * The empty-string sentinel, the same trick `OpenInButton` uses.
   *
   * A `<select>` fires no `change` when you re-pick the option that is already selected, so the value
   * is reset to `''` after every pick and the placeholder is what is always "chosen".
   */
  let linkChoice = $state('');
  let agentChoice = $state('');
  const startable = $derived(
    sessions.options.filter((option) => option.available && option.offered),
  );

  // The Compose project name is an implementation detail and, in the ordinary case, just a
  // slugged copy of the worktree title beside it. Keep genuinely useful configured badges (issue
  // status, environment, owner) without spending top-bar space repeating the selected worktree.
  const visibleBadges = $derived(
    worktree.badges.filter((badge) => badge.label.trim().toLowerCase() !== 'compose'),
  );

  async function pickLink(event: Event) {
    const select = event.currentTarget as HTMLSelectElement;
    const url = select.value;
    select.value = '';
    linkChoice = '';
    if (!url) return;
    try {
      await commands.openUrl(url);
    } catch {
      /* The scheme is validated in Rust; nothing useful to do if the OS declines. */
    }
  }

  function pickAgent(event: Event) {
    const select = event.currentTarget as HTMLSelectElement;
    const provider = select.value;
    select.value = '';
    agentChoice = '';
    if (provider) void sessions.openAgent(projectId, worktree.id, provider);
  }
</script>

<header class="c-worktree-bar">
  <!--
    Duplicated from the sidebar on purpose: the one there only appears on hover, which is not
    somewhere a control can be *found*. This is where you learn it exists.
  -->
  <button
    class="c-detail__star"
    class:is-on={worktree.favorite}
    aria-pressed={worktree.favorite}
    title={worktree.favorite ? 'Remove from favorites' : 'Add to favorites'}
    onclick={onfavorite}
  >
    <Icon name={worktree.favorite ? 'star' : 'star-outline'} size={18} />
    <span class="u-visually-hidden">Favorite</span>
  </button>

  <h1 class="c-worktree-bar__title">{worktree.title}</h1>

  <div class="c-worktree-bar__facts">
    {#if worktree.branch}
      <code class="c-worktree-bar__branch">{worktree.branch}</code>
    {:else}
      <span class="c-status--muted">detached</span>
    {/if}
    {#if worktree.dirty || worktree.untracked > 0 || worktree.staged > 0}
      <span class="c-status--warn">modified</span>
    {/if}
    {#if worktree.ahead > 0 || worktree.behind > 0}
      <span class="c-status--info c-worktree-bar__diverge">
        ↑{worktree.ahead}↓{worktree.behind}
      </span>
    {/if}
    {#if worktree.isMain}<span class="c-badge c-badge--accent">main worktree</span>{/if}
    {#each visibleBadges as badge (badge.label)}
      <span class="c-badge" title={badge.label}>{badge.label}: {badge.value}</span>
    {/each}
  </div>

  <div class="c-worktree-bar__actions">
    {#if startable.length > 0}
      <!-- Available even when the only pane is a shell; starting an agent never has to replace it. -->
      <span class="o-overlay-select">
        <span class="c-button c-button--quiet c-button--sm" aria-hidden="true">
          New agent <Icon name="chevron-down" size={11} />
        </span>
        <select
          class="o-overlay-select__native"
          aria-label="Open an agent in this worktree"
          bind:value={agentChoice}
          onchange={pickAgent}
        >
          <option value="">New agent</option>
          {#each startable as option (option.id)}
            <option value={option.id}>{option.label}</option>
          {/each}
        </select>
      </span>
    {/if}

    {#if worktree.links.length > 0}
      <!-- A native select, so the menu needs no stacking level of its own. -->
      <span class="o-overlay-select">
        <span class="c-button c-button--quiet c-button--sm" aria-hidden="true">
          Links <Icon name="chevron-down" size={11} />
        </span>
        <select
          class="o-overlay-select__native"
          aria-label="Open a link for this worktree"
          bind:value={linkChoice}
          onchange={pickLink}
        >
          <option value="">Links</option>
          {#each worktree.links as link (link.label)}
            <option value={link.url}>{link.label} — {link.url}</option>
          {/each}
        </select>
      </span>
    {/if}

    <Button
      variant="quiet"
      size="sm"
      title="Path, ports and environment ({INSPECTOR_SHORTCUT})"
      onclick={oninspect}
    >
      Details
    </Button>

    <Button
      variant="quiet"
      size="sm"
      title="Open a shell in this worktree ({SHELL_SHORTCUT})"
      onclick={() => void sessions.openShell(projectId, worktree.id)}
    >
      <Icon name="terminal" size={13} /> Shell
    </Button>

    <OpenInButton {projectId} worktreeId={worktree.id} />

    <span class="c-detail__divider" aria-hidden="true"></span>

    <!-- The main worktree cannot be removed; git refuses, and so does the pipeline. -->
    <button
      class="c-button c-button--danger-outline c-button--sm"
      onclick={onremove}
      disabled={worktree.isMain}
      title={worktree.isMain
        ? "git will not remove a repository's main worktree"
        : 'Remove this worktree'}
    >
      Remove
    </button>
  </div>
</header>
