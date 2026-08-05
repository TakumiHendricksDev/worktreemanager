<script lang="ts">
  /**
   * The right pane: everything known about the selected worktree.
   *
   * The path is the first fact in Overview and the first thing copyable, because "where is
   * this on disk" is the question the app exists to answer quickly. It used to sit in the
   * header beside the actions, which made a *fact* look like a control and left the header
   * doing two jobs; the header is now the title and the things you can do to it, nothing
   * else.
   */
  import { commands } from '../ipc/commands';
  import { errorMessage, type Worktree } from '../ipc/types';
  import { agents } from '../state/agents.svelte';
  import { SHORTCUT_LABEL, terminals } from '../state/terminals.svelte';
  import AgentPane from './AgentPane.svelte';
  import OpenInButton from './OpenInButton.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    worktree,
    projectId,
    onremove,
    onfavorite,
  }: {
    worktree: Worktree;
    projectId: string;
    onremove: () => void;
    onfavorite: () => void;
  } = $props();

  /**
   * `chat` is deliberately first in the union and the default.
   *
   * The walking skeleton lives in a tab because that is the smallest place to put it while the
   * protocol is being settled; the reorganization that makes chat the pane itself is its own
   * change. Defaulting to it anyway means the round trip is what you see on selecting a worktree,
   * which is the point of the increment.
   */
  type Tab = 'chat' | 'overview' | 'env';
  let tab = $state<Tab>('chat');

  const panes = $derived(agents.panesIn(worktree.id));
  let copied = $state(false);

  async function copyPath() {
    try {
      await navigator.clipboard.writeText(worktree.path);
      copied = true;
      setTimeout(() => (copied = false), 1400);
    } catch {
      // Clipboard access can be denied; the path is selectable as a fallback.
    }
  }

  async function open(url: string) {
    try {
      await commands.openUrl(url);
    } catch {
      /* Nothing useful to do if the OS declines or the scheme is rejected. */
    }
  }

  const envEntries = $derived(worktree.env);

  /**
   * Values the user has explicitly revealed, for this worktree only.
   *
   * Keyed by env key and reset whenever the selected worktree changes, so switching tabs
   * and coming back re-masks. A revealed secret should not linger on screen because you
   * looked at something else for a minute.
   */
  let revealed = $state<Record<string, string>>({});
  let revealError = $state<string | null>(null);

  $effect(() => {
    // Depend on the id so this re-runs on selection change.
    void worktree.id;
    revealed = {};
    revealError = null;
  });

  async function reveal(key: string) {
    revealError = null;
    try {
      revealed[key] = await commands.revealEnvValue(projectId, worktree.id, key);
    } catch (e) {
      revealError = errorMessage(e);
    }
  }

  function hide(key: string) {
    delete revealed[key];
  }

  async function copyValue(key: string) {
    try {
      const value =
        revealed[key] ?? (await commands.revealEnvValue(projectId, worktree.id, key));
      await navigator.clipboard.writeText(value);
      copiedKey = key;
      setTimeout(() => (copiedKey = null), 1400);
    } catch (e) {
      revealError = errorMessage(e);
    }
  }

  let copiedKey = $state<string | null>(null);
</script>

<div
  class="c-detail"
  id="worktree-detail"
  role="tabpanel"
  aria-labelledby={`tab-${worktree.id}`}
  tabindex="-1"
>
  <header class="c-detail__head">
    <div class="c-detail__titles">
      <!--
        Duplicated from the sidebar on purpose: the one there only appears on hover, which
        is not somewhere a control can be *found*. This is where you learn it exists.
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
      <h1 class="c-pane-title">{worktree.title}</h1>
      <div class="c-detail__badges">
        {#if worktree.isMain}<span class="c-badge c-badge--accent">main worktree</span>{/if}
        {#if worktree.isBare}<span class="c-badge">bare</span>{/if}
        {#if worktree.issueKey}<span class="c-badge">{worktree.issueKey}</span>{/if}
        {#each worktree.badges as badge (badge.label)}
          <span class="c-badge" title={badge.label}>{badge.label}: {badge.value}</span>
        {/each}
      </div>
    </div>

    <!-- Actions, pinned right. The path moved into Overview, where it reads as one of the
         facts rather than as something to click.

         Neutral things first — the terminal toggle, then Open-in — then a divider, then Remove.
         The gap is not cosmetic: a destructive control sitting flush against a neutral one is how
         it gets clicked by accident, and Remove is now the only thing on the far side of the
         line. -->
    <div class="c-detail__actions">
      <!--
        A disclosure, not a link to somewhere: `aria-expanded` says the region it names is on
        screen and `aria-controls` says which region. The dock is mounted by the shell rather than
        by this component precisely so it can outlive it, so this button and the thing it toggles
        only ever meet through the store and that id.
      -->
      <Button
        variant="quiet"
        size="sm"
        onclick={() => void terminals.toggle(projectId, worktree.id)}
        title="Terminal ({SHORTCUT_LABEL})"
        ariaExpanded={terminals.open}
        ariaControls="terminal-dock"
      >
        <Icon name="terminal" size={13} /> Terminal
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
        Remove worktree
      </button>
    </div>
  </header>

  <nav class="c-tabs c-tabs--inset" aria-label="Worktree details">
    <button
      class="c-tabs__tab"
      class:is-active={tab === 'chat'}
      onclick={() => (tab = 'chat')}
    >
      Chat
      {#if panes.length > 0}<span class="c-tabs__count">{panes.length}</span>{/if}
    </button>
    <button
      class="c-tabs__tab"
      class:is-active={tab === 'overview'}
      onclick={() => (tab = 'overview')}
    >
      Overview
    </button>
    {#if envEntries.length > 0}
      <button
        class="c-tabs__tab"
        class:is-active={tab === 'env'}
        onclick={() => (tab = 'env')}
      >
        Environment <span class="c-tabs__count">{envEntries.length}</span>
      </button>
    {/if}
  </nav>

  <div class="c-detail__body" class:c-detail__body--flush={tab === 'chat'}>
    {#if tab === 'chat'}
      {#if panes.length === 0}
        <div class="c-agent__start">
          <p>
            Start an agent session in this worktree. It runs in
            <code>{worktree.dirname}</code> and can read and change the files there.
          </p>
          <!--
            Unavailable agents are listed and disabled, with the reason, rather than omitted.
            The same call `OpenInButton` makes and for the same reason: a greyed row saying "no
            `claude` on wtm's PATH" doubles as the diagnosis of this app's most likely production
            failure, where a silently shorter list is a mystery. Filtering them out was the first
            version of this, and it hid exactly the case worth explaining.
          -->
          <div class="o-row">
            {#each agents.options as option (option.id)}
              <Button
                variant={option.available ? 'accent' : 'neutral'}
                size="sm"
                disabled={!option.available}
                title={option.detail ?? option.blurb}
                onclick={() => void agents.open(projectId, worktree.id, option.id)}
              >
                {option.label}
              </Button>
            {/each}
          </div>
          {#if agents.options.every((o) => !o.available)}
            <p class="c-status--warn">
              No agent CLI is on wtm's PATH. Settings → Advanced shows the PATH wtm
              resolved.
            </p>
          {/if}
        </div>
      {:else}
        <!--
          Keyed by session *and* generation so a restart remounts rather than continuing a dead
          session's transcript under a live one — the same key the terminal dock uses, for the same
          reason. A pane with no session id yet is keyed by its index, which is stable for the one
          tick before the id arrives.
        -->
        {#each panes as pane, index (`${pane.session ?? `pending-${index}`}:${pane.generation}`)}
          <AgentPane {pane} />
        {/each}
      {/if}
    {:else if tab === 'overview'}
      <dl class="o-facts">
        <!--
          First, because "where is this on disk" is the question the app exists to answer
          quickly. It is a fact, so it is rendered like the other values rather than as the
          button it used to be; the copy action sits beside it and stays visible, matching
          the Environment tab's row actions.

          There is no separate `Directory` row any more — `dirname` is by definition the last
          segment of this path, so it was the same fact written twice, one line apart.
        -->
        <dt>Path</dt>
        <dd class="c-detail__path-row">
          <code>{worktree.path}</code>
          <button class="c-row-action" onclick={copyPath}>
            {copied ? 'copied' : 'copy'}
          </button>
        </dd>

        <dt>Branch</dt>
        <dd>
          {#if worktree.branch}
            <code>{worktree.branch}</code>
          {:else}
            <!-- Never substitute the directory name here: they legitimately disagree. -->
            <span class="c-status--muted">detached HEAD</span>
          {/if}
        </dd>

        {#if worktree.head}
          <dt>HEAD</dt>
          <dd><code>{worktree.head}</code></dd>
        {/if}

        <dt>Status</dt>
        <dd>
          {#if worktree.dirty || worktree.untracked > 0 || worktree.staged > 0}
            <span class="c-detail__statuses">
              {#if worktree.staged > 0}<span class="c-status--ok"
                  >{worktree.staged} staged</span
                >{/if}
              {#if worktree.dirty}<span class="c-status--warn">modified</span>{/if}
              {#if worktree.untracked > 0}
                <span class="c-status--muted">{worktree.untracked} untracked</span>
              {/if}
            </span>
          {:else}
            <span class="c-status--ok">clean</span>
          {/if}
        </dd>

        {#if worktree.ahead > 0 || worktree.behind > 0}
          <dt>Divergence</dt>
          <dd>
            <span class="c-status--info">
              {worktree.ahead} ahead, {worktree.behind} behind
            </span>
          </dd>
        {/if}

        {#if worktree.locked}
          <dt>Locked</dt>
          <dd>
            <span class="c-status--warn">{worktree.locked || 'no reason given'}</span>
          </dd>
        {/if}

        {#if worktree.prunable}
          <dt>Stale</dt>
          <dd>
            <span class="c-status--danger">{worktree.prunable}</span>
          </dd>
        {/if}
      </dl>

      {#if worktree.links.length > 0}
        <h2 class="c-section-heading">Links</h2>
        <div class="c-detail__links">
          {#each worktree.links as link (link.label)}
            <button class="c-link-row" onclick={() => open(link.url)}>
              <span class="c-link-row__label">{link.label}</span>
              <code>{link.url}</code>
            </button>
          {/each}
        </div>
      {/if}

      {#if worktree.table.length > 0}
        <h2 class="c-section-heading">Ports</h2>
        <table class="c-table">
          <tbody>
            {#each worktree.table as row (row.label)}
              <tr>
                <th>{row.label}</th>
                <td>
                  {#if row.url}
                    <button
                      class="c-button c-button--link c-button--sm c-detail__port"
                      onclick={() => open(row.url!)}>{row.value}</button
                    >
                  {:else}
                    <code>{row.value}</code>
                  {/if}
                  {#if row.inherited}
                    <!-- An absent variable means the base value is in effect. Invisible
                         unless we say so, and then confusing. -->
                    <span
                      class="c-detail__inherited"
                      title="Not set for this worktree — the project default applies"
                    >
                      default
                    </span>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    {:else}
      {#if revealError}
        <p class="c-status--danger">{revealError}</p>
      {/if}
      <p class="c-detail__env-note">
        No values are sent to this window. Reveal one at a time; each is read from disk when
        you ask for it and never kept.
      </p>
      <table class="c-table c-table--env">
        <tbody>
          {#each envEntries as key (key)}
            <tr>
              <th><code>{key}</code></th>
              <td>
                {#if revealed[key] !== undefined}
                  <code class="c-detail__revealed">{revealed[key]}</code>
                  <button class="c-row-action" onclick={() => hide(key)}>hide</button>
                {:else}
                  <code class="c-detail__masked" aria-label="hidden value">••••••••</code>
                  <button class="c-row-action" onclick={() => reveal(key)}>reveal</button>
                {/if}
                <button class="c-row-action" onclick={() => copyValue(key)}>
                  {copiedKey === key ? 'copied' : 'copy'}
                </button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </div>
</div>
