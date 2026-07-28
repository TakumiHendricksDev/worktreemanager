<script lang="ts">
  /**
   * The right pane: everything known about the selected worktree.
   *
   * The path is the first thing shown and the first thing copyable, because "where is
   * this on disk" is the question the app exists to answer quickly.
   */
  import { commands } from '../ipc/commands';
  import { errorMessage, type Worktree } from '../ipc/types';

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

  type Tab = 'overview' | 'env';
  let tab = $state<Tab>('overview');
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
  class="detail"
  id="worktree-detail"
  role="tabpanel"
  aria-labelledby={`tab-${worktree.id}`}
  tabindex="-1"
>
  <header class="head">
    <div class="titles">
      <!--
        Duplicated from the sidebar on purpose: the one there only appears on hover, which
        is not somewhere a control can be *found*. This is where you learn it exists.
      -->
      <button
        class="star"
        class:on={worktree.favorite}
        aria-pressed={worktree.favorite}
        title={worktree.favorite ? 'Remove from favorites' : 'Add to favorites'}
        onclick={onfavorite}
      >
        <span aria-hidden="true">{worktree.favorite ? '★' : '☆'}</span>
        <span class="visually-hidden">Favorite</span>
      </button>
      <h1>{worktree.title}</h1>
      <div class="badges">
        {#if worktree.isMain}<span class="badge accent">main worktree</span>{/if}
        {#if worktree.isBare}<span class="badge">bare</span>{/if}
        {#if worktree.issueKey}<span class="badge">{worktree.issueKey}</span>{/if}
        {#each worktree.badges as badge (badge.label)}
          <span class="badge" title={badge.label}>{badge.label}: {badge.value}</span>
        {/each}
      </div>
    </div>

    <div class="row">
      <button class="path" onclick={copyPath} title="Copy the full path">
        <code>{worktree.path}</code>
        <span class="copy">{copied ? 'copied' : 'copy'}</span>
      </button>

      <!-- The main worktree cannot be removed; git refuses, and so does the pipeline. -->
      <button
        class="remove"
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

  <nav class="tabs" aria-label="Worktree details">
    <button class:active={tab === 'overview'} onclick={() => (tab = 'overview')}>
      Overview
    </button>
    {#if envEntries.length > 0}
      <button class:active={tab === 'env'} onclick={() => (tab = 'env')}>
        Environment <span class="count">{envEntries.length}</span>
      </button>
    {/if}
  </nav>

  <div class="body">
    {#if tab === 'overview'}
      <dl class="facts">
        <dt>Branch</dt>
        <dd>
          {#if worktree.branch}
            <code>{worktree.branch}</code>
          {:else}
            <!-- Never substitute the directory name here: they legitimately disagree. -->
            <span class="muted">detached HEAD</span>
          {/if}
        </dd>

        <dt>Directory</dt>
        <dd><code>{worktree.dirname}</code></dd>

        {#if worktree.head}
          <dt>HEAD</dt>
          <dd><code>{worktree.head}</code></dd>
        {/if}

        <dt>Status</dt>
        <dd>
          {#if worktree.dirty || worktree.untracked > 0 || worktree.staged > 0}
            <span class="statuses">
              {#if worktree.staged > 0}<span class="ok">{worktree.staged} staged</span>{/if}
              {#if worktree.dirty}<span class="warn">modified</span>{/if}
              {#if worktree.untracked > 0}
                <span class="muted">{worktree.untracked} untracked</span>
              {/if}
            </span>
          {:else}
            <span class="ok">clean</span>
          {/if}
        </dd>

        {#if worktree.ahead > 0 || worktree.behind > 0}
          <dt>Divergence</dt>
          <dd>
            <span class="info">
              {worktree.ahead} ahead, {worktree.behind} behind
            </span>
          </dd>
        {/if}

        {#if worktree.locked}
          <dt>Locked</dt>
          <dd><span class="warn">{worktree.locked || 'no reason given'}</span></dd>
        {/if}

        {#if worktree.prunable}
          <dt>Stale</dt>
          <dd>
            <span class="danger">{worktree.prunable}</span>
          </dd>
        {/if}
      </dl>

      {#if worktree.links.length > 0}
        <h2>Links</h2>
        <div class="links">
          {#each worktree.links as link (link.label)}
            <button class="link" onclick={() => open(link.url)}>
              <span class="label">{link.label}</span>
              <code>{link.url}</code>
            </button>
          {/each}
        </div>
      {/if}

      {#if worktree.table.length > 0}
        <h2>Ports</h2>
        <table>
          <tbody>
            {#each worktree.table as row (row.label)}
              <tr>
                <th>{row.label}</th>
                <td>
                  {#if row.url}
                    <button class="inline" onclick={() => open(row.url!)}
                      >{row.value}</button
                    >
                  {:else}
                    <code>{row.value}</code>
                  {/if}
                  {#if row.inherited}
                    <!-- An absent variable means the base value is in effect. Invisible
                         unless we say so, and then confusing. -->
                    <span
                      class="inherited"
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
        <p class="revealerror">{revealError}</p>
      {/if}
      <p class="envnote">
        No values are sent to this window. Reveal one at a time; each is read from disk when
        you ask for it and never kept.
      </p>
      <table class="env">
        <tbody>
          {#each envEntries as key (key)}
            <tr>
              <th><code>{key}</code></th>
              <td>
                {#if revealed[key] !== undefined}
                  <code class="revealed">{revealed[key]}</code>
                  <button class="envaction" onclick={() => hide(key)}>hide</button>
                {:else}
                  <code class="masked" aria-label="hidden value">••••••••</code>
                  <button class="envaction" onclick={() => reveal(key)}>reveal</button>
                {/if}
                <button class="envaction" onclick={() => copyValue(key)}>
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

<style>
  .detail {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    background: var(--bg);
  }

  .head {
    flex: 0 0 auto;
    padding: var(--sp-4) var(--sp-5) var(--sp-3);
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }

  .titles {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    flex-wrap: wrap;
  }

  h1 {
    font-size: var(--step-2);
    font-weight: 600;
    letter-spacing: -0.01em;
  }

  /* Always visible here, unlike the sidebar's hover-revealed twin — this is the copy that
     has to be findable. */
  .star {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    /* Pulls the star in from the row's gap so it reads as attached to the title. */
    margin-right: calc(var(--sp-2) - var(--sp-3));
    border-radius: var(--r-sm);
    font-size: var(--step-1);
    line-height: 1;
    color: var(--fg-subtle);
    transition:
      background var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
  }

  .star.on {
    color: var(--star);
  }

  .star:hover {
    background: var(--bg-hover);
    color: var(--star);
  }

  h2 {
    font-size: var(--step--1);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-muted);
    margin-top: var(--sp-5);
    margin-bottom: var(--sp-2);
  }

  .badges {
    display: flex;
    gap: var(--sp-2);
    flex-wrap: wrap;
  }

  .badge {
    font-size: var(--step--2);
    padding: 2px 8px;
    border-radius: 999px;
    background: var(--bg-hover);
    color: var(--fg-muted);
    white-space: nowrap;
  }

  .badge.accent {
    background: var(--accent-soft);
    color: var(--accent);
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }

  /*
    Never shrinks and never wraps. The label used to be "Remove…", following the macOS
    convention that a trailing ellipsis means "this opens a dialog" — but next to a path that
    *does* get ellipsised, it reads as a truncated word rather than an affordance. Spelling it
    out costs a few pixels the path can give up.
  */
  .remove {
    flex: 0 0 auto;
    white-space: nowrap;
    padding: 4px 10px;
    border-radius: var(--r-md);
    border: 1px solid var(--border);
    font-size: var(--step--2);
    color: var(--fg-muted);
  }

  .remove:hover:not(:disabled) {
    border-color: color-mix(in oklab, var(--danger) 45%, transparent);
    color: var(--danger);
  }

  .remove:disabled {
    opacity: 0.35;
    cursor: default;
  }

  .path {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    padding: 4px 8px;
    border-radius: var(--r-md);
    background: var(--bg-code);
    color: var(--fg-muted);
    transition: background var(--dur-fast) var(--ease);
  }

  .path:hover {
    background: var(--bg-hover);
  }

  .path code {
    font-size: var(--step--1);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .copy {
    flex: 0 0 auto;
    font-size: var(--step--2);
    color: var(--fg-subtle);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .tabs {
    flex: 0 0 auto;
    display: flex;
    gap: var(--sp-1);
    padding: 0 var(--sp-5);
    border-bottom: 1px solid var(--border);
  }

  .tabs button {
    padding: var(--sp-2) var(--sp-3);
    font-size: var(--step--1);
    color: var(--fg-muted);
    border-bottom: 2px solid transparent;
    margin-bottom: -1px;
    transition: color var(--dur-fast) var(--ease);
  }

  .tabs button:hover {
    color: var(--fg);
  }

  .tabs button.active {
    color: var(--fg);
    border-bottom-color: var(--accent);
    font-weight: 500;
  }

  .count {
    font-size: var(--step--2);
    color: var(--fg-subtle);
  }

  .body {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    padding: var(--sp-4) var(--sp-5) var(--sp-6);
  }

  .facts {
    display: grid;
    grid-template-columns: minmax(110px, max-content) 1fr;
    gap: var(--sp-2) var(--sp-4);
    align-items: baseline;
  }

  dt {
    color: var(--fg-muted);
    font-size: var(--step--1);
  }

  dd {
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .statuses {
    display: flex;
    gap: var(--sp-3);
    flex-wrap: wrap;
  }

  .muted {
    color: var(--fg-muted);
  }
  .ok {
    color: var(--ok);
  }
  .warn {
    color: var(--warn);
  }
  .danger {
    color: var(--danger);
  }
  .info {
    color: var(--info);
  }

  .links {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    align-items: flex-start;
  }

  .link {
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
    padding: 4px 8px;
    border-radius: var(--r-md);
    max-width: 100%;
  }

  .link:hover {
    background: var(--bg-hover);
  }

  .link .label {
    font-size: var(--step--1);
    font-weight: 500;
    flex: 0 0 auto;
  }

  .link code {
    color: var(--accent);
    font-size: var(--step--1);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  table {
    border-collapse: collapse;
    width: 100%;
    max-width: 560px;
  }

  table.env {
    max-width: none;
  }

  th {
    text-align: left;
    font-weight: 400;
    color: var(--fg-muted);
    font-size: var(--step--1);
    padding: 3px var(--sp-4) 3px 0;
    vertical-align: baseline;
    white-space: nowrap;
  }

  td {
    padding: 3px 0;
    vertical-align: baseline;
    overflow-wrap: anywhere;
  }

  .inline {
    color: var(--accent);
    font-family: var(--font-mono);
    font-size: 0.925em;
  }

  .inline:hover {
    text-decoration: underline;
  }

  .envnote {
    color: var(--fg-muted);
    font-size: var(--step--2);
    line-height: 1.55;
    max-width: 74ch;
    margin-bottom: var(--sp-3);
  }

  .revealerror {
    color: var(--danger);
    font-size: var(--step--1);
    margin-bottom: var(--sp-2);
  }

  .masked {
    color: var(--fg-subtle);
    letter-spacing: 1px;
  }

  .revealed {
    /* A revealed secret should look different from ordinary data, so it is obvious at a
       glance that something is exposed on screen. */
    background: color-mix(in oklab, var(--warn) 16%, transparent);
    padding: 0 4px;
    border-radius: var(--r-sm);
    overflow-wrap: anywhere;
  }

  .envaction {
    margin-left: var(--sp-2);
    font-size: var(--step--2);
    color: var(--fg-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .envaction:hover {
    color: var(--accent);
    text-decoration: underline;
  }

  .inherited {
    margin-left: var(--sp-2);
    font-size: var(--step--2);
    color: var(--fg-subtle);
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 0 6px;
  }
</style>
