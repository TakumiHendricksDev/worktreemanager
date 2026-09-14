<script lang="ts">
  /**
   * A browser pane: its toolbar, its empty state, its comments, and the rectangle the page is laid
   * over.
   *
   * # The one thing this component is really for
   *
   * The page is not here. It is a *native* child webview that Rust owns and positions, and it paints
   * above every element in this document — nothing in the DOM can sit on top of it, clip it, or hide
   * it with `display: none`. So this component's load-bearing job is to keep telling Rust where
   * `.c-browser__view` is, and to tell it *nothing is there* whenever something would need to paint
   * over the page: an inactive worktree, a dialog, a pane being dragged. Everything else in the file
   * is an ordinary toolbar and a list.
   *
   * # Why the measurement runs in a frame and compares before sending
   *
   * Tile geometry changes in bursts — a splitter drag is a `pointermove` per pixel — and every send
   * is an IPC round trip that ends in a native `setFrame`. One `requestAnimationFrame` coalesces a
   * burst into one measurement per paint, and `lastSent` makes the duplicate triggers free: the
   * layout effect and the `ResizeObserver` both fire for a ratio drag, and only one of them should
   * cost anything. Same guard `Terminal.refit` keeps for the same reason.
   *
   * # Why the tracking effect writes nothing reactive
   *
   * `frame` and `lastSent` are plain variables on purpose. An effect that read `visible` and wrote a
   * `$state` it also read would be the read-write loop `layout.svelte.ts` documents; this one reads
   * its inputs, schedules a frame, and the frame does the writing — to Rust, not to state. The one
   * `$state` the frame writes, `frozen`, is written after an `await` and read by nothing the effect
   * tracks.
   *
   * # Why the comments live beside the page and the pins live in it
   *
   * A list of comments can be Svelte, because it sits next to the native view rather than over it.
   * The pins and the popover cannot: they have to be drawn *on the page*, and the only thing that
   * can draw there is the runtime Rust installs into the page. So the runtime owns the pins and this
   * component owns the list, and Rust owns the comments both of them show.
   */
  import { untrack } from 'svelte';

  import { commands } from '../ipc/commands';
  import {
    errorMessage,
    type BrowserComment,
    type BrowserHistoryAction,
    type BrowserView,
  } from '../ipc/types';
  import { browsers, type BrowserShortcut } from '../state/browsers.svelte';
  import { overlay } from '../state/overlay.svelte';
  import { sessions, type Pane } from '../state/sessions.svelte';
  import { theme } from '../state/theme.svelte';
  import { workspace } from '../state/workspace.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';
  import TextInput from './ui/TextInput.svelte';

  const { pane, visible }: { pane: Pane; visible: boolean } = $props();

  /** The browser id, which is the pane's session. Null for an empty pane, which has no webview yet. */
  const id = $derived(pane.session);
  const view = $derived(browsers.viewOf(pane.session));
  const blank = $derived(id === null);
  const loading = $derived(view?.loading ?? false);
  const commenting = $derived(view?.commentMode ?? false);
  const comments = $derived(browsers.commentsOf(pane.session));
  const openComments = $derived(comments.filter((comment) => comment.status === 'open'));
  /** Why comments are off on this build, or null. The pane itself works without the runtime. */
  const runtimeOff = $derived(browsers.runtimeUnavailable);
  /** For the empty state's offers: the worktree's links and its `[browser] home`. */
  const worktree = $derived(
    workspace.worktrees.find((candidate) => candidate.id === pane.worktreeId) ?? null,
  );
  /** The agent pane Send drafts into. Derived from focus and layout, so it follows the user. */
  const target = $derived(sessions.draftTargetIn(pane.worktreeId));
  const targetLabel = $derived(target ? sessions.labelOf(target) : null);

  let address = $state<HTMLInputElement | null>(null);
  let typed = $state('');
  /** True while the address is being edited, so a page navigating underneath does not overwrite it. */
  let editing = $state(false);
  /** What failed in the toolbar. Kept beside the controls that caused it. */
  let problem = $state<string | null>(null);
  let agentAccessPending = $state(false);
  /** The rectangle the native view is laid over. */
  let box = $state<HTMLElement | null>(null);
  /** The last picture of the page, shown while the native view is hidden. */
  let frozen = $state<string | null>(null);
  let commentsOpen = $state(false);
  let editingComment = $state<number | null>(null);
  let editText = $state('');
  /** The comment a pin click pointed at, so the list can show which one. */
  let picked = $state<number | null>(null);

  /** What the address bar shows for a page: its URL, or nothing for the blank page. */
  function shownAddress(url: string | null): string {
    return url === null || url === 'about:blank' ? '' : url;
  }

  $effect(() => {
    if (!editing) typed = shownAddress(view?.url ?? pane.url);
  });

  /**
   * Hosts that are almost certainly not serving TLS: a dev server. Everything else gets `https`,
   * which is what a person typing a bare host into a browser expects to happen.
   */
  const LOCAL_HOST =
    /^(localhost|127(\.\d{1,3}){3}|0\.0\.0\.0|\[::1\]|[^/:\s]+\.local)(:\d+)?([/?#]|$)/i;
  /** A real scheme, as opposed to `localhost:5173`, whose colon a URL parser would take for one. */
  const SCHEME = /^([a-z][a-z0-9+.-]*):\/\//i;

  function normalise(raw: string): string | null {
    const text = raw.trim();
    if (text === '') return null;
    if (SCHEME.test(text) || text === 'about:blank') return text;
    return `${LOCAL_HOST.test(text) ? 'http' : 'https'}://${text}`;
  }

  async function go(): Promise<void> {
    const url = normalise(typed);
    editing = false;
    if (url === null) return;
    problem = await sessions.navigateBrowser(pane.id, url);
  }

  function open(url: string): void {
    typed = url;
    void go();
  }

  function history(action: BrowserHistoryAction): void {
    if (id === null) return;
    void commands.browserHistory(id, action).catch(() => {
      /* The pane is gone; its `browser:closed` is on the way. */
    });
  }

  async function toggleAgents(): Promise<void> {
    if (id === null || !view || agentAccessPending || runtimeOff !== null) return;
    agentAccessPending = true;
    problem = null;
    try {
      // The reply confirms the change even if the matching state event has not arrived yet.
      browsers.apply(await commands.browserSetAgentAccess(id, !view.agentAccess));
    } catch (error) {
      problem = errorMessage(error);
    } finally {
      agentAccessPending = false;
    }
  }

  function openExternally(): void {
    if (!view || shownAddress(view.url) === '') return;
    void commands.openUrl(view.url).catch(() => {});
  }

  // ── comments ──

  function setCommenting(on: boolean): void {
    if (id === null) return;
    void commands.browserSetCommentMode(id, on).catch(() => {});
    if (on) commentsOpen = true;
  }

  function toggleResolved(comment: BrowserComment): void {
    if (id === null) return;
    void commands
      .browserResolveComment(id, comment.id, comment.status !== 'resolved')
      .catch(() => {});
  }

  function remove(comment: BrowserComment): void {
    if (id === null) return;
    void commands.browserRemoveComment(id, comment.id).catch(() => {});
  }

  function startEdit(comment: BrowserComment): void {
    editingComment = comment.id;
    editText = comment.text;
  }

  async function saveEdit(comment: BrowserComment): Promise<void> {
    const text = editText.trim();
    editingComment = null;
    if (id === null || text === '' || text === comment.text) return;
    await commands.browserUpdateComment(id, comment.id, text).catch(() => {});
  }

  /**
   * The message Send drafts. Each comment names its element three ways — what it is, what it
   * says, and a selector — because an agent reading this has the page in a tool and not in front
   * of it, and the selector is what turns "this button" into something it can find.
   */
  function feedbackMessage(page: BrowserView | null, list: BrowserComment[]): string {
    const title = page?.title.trim() || 'the page';
    const lines = [`Feedback on ${title} (${page?.url ?? ''}):`, ''];
    list.forEach((comment, index) => {
      const anchor = comment.anchor;
      const what = [
        anchor.tag,
        anchor.role && anchor.role !== 'generic' ? anchor.role : null,
      ]
        .filter(Boolean)
        .join(' ');
      const says = anchor.name || anchor.text;
      const label = says ? ` “${says}”` : '';
      const heading = anchor.nearestHeading ? `, under “${anchor.nearestHeading}”` : '';
      lines.push(
        `${index + 1}. ${comment.text}`,
        `   — on the ${what}${label} (selector: \`${anchor.selector}\`${heading})`,
      );
    });
    lines.push(
      '',
      'The selectors refer to the rendered page in the wtm browser pane; use the browser tools to inspect it if needed.',
    );
    return lines.join('\n');
  }

  function send(): void {
    if (!target || openComments.length === 0) return;
    sessions.insertDraft(target.id, feedbackMessage(view, openComments));
  }

  let seenPick = 0;
  $effect(() => {
    const epoch = browsers.pickEpoch;
    const pick = browsers.pickTarget;
    if (epoch === seenPick || pick === null || pick.id !== id) return;
    seenPick = epoch;
    commentsOpen = true;
    picked = pick.commentId;
  });

  // ── the theme, for the pins and popover the runtime draws inside the page ──

  $effect(() => {
    void theme.resolved;
    // Re-sent after every load as well: the runtime starts afresh with each page.
    void loading;
    const browser = id;
    if (browser === null || browsers.runtimeUnavailable !== null) return;
    const style = getComputedStyle(document.documentElement);
    const read = (name: string) => style.getPropertyValue(name).trim();
    void commands
      .browserSetTheme(browser, {
        accent: read('--accent'),
        bg: read('--bg-elevated'),
        fg: read('--fg'),
        border: read('--border'),
        font: read('--font-ui'),
      })
      .catch(() => {});
  });

  // ── chords ──

  /** One handler for both routes a chord can take — the DOM, and Rust relaying the native view. */
  function run(action: BrowserShortcut): void {
    switch (action) {
      case 'focus-address':
        address?.focus();
        address?.select();
        break;
      case 'back':
      case 'forward':
      case 'reload':
        history(action);
        break;
      case 'escape':
        if (commenting) {
          setCommenting(false);
        } else if (editing) {
          editing = false;
          typed = shownAddress(view?.url ?? pane.url);
          problem = null;
          address?.blur();
        } else if (loading) {
          history('stop');
        }
        break;
      case 'focus':
        sessions.noteFocus(pane.worktreeId, pane.id);
        break;
      case 'shell':
        void sessions.focusOrOpenShell(pane.projectId, pane.worktreeId);
        break;
      case 'toggle-comments':
        if (runtimeOff === null) setCommenting(!commenting);
        break;
    }
  }

  function onKey(event: KeyboardEvent): void {
    if (event.key === 'Escape') {
      if (!editing && !loading && !commenting) return;
      event.preventDefault();
      run('escape');
      return;
    }
    if (!(event.metaKey || event.ctrlKey)) return;
    const action: BrowserShortcut | null =
      event.key === 'l'
        ? 'focus-address'
        : event.key === '['
          ? 'back'
          : event.key === ']'
            ? 'forward'
            : event.key === 'r'
              ? 'reload'
              : event.shiftKey && event.key.toLowerCase() === 'c'
                ? 'toggle-comments'
                : null;
    if (action === null) return;
    // Stopped, not just defaulted: `App.svelte` refreshes the worktree list on a bubble-phase ⌘R,
    // and a reload of this page must not also be that.
    event.preventDefault();
    event.stopPropagation();
    run(action);
  }

  /*
   * Chords pressed *inside* the page never reach this document. Rust relays them as
   * `browser:shortcut`, the store bumps an epoch, and this is the pane that owns the id.
   */
  let seenShortcut = 0;
  $effect(() => {
    const epoch = browsers.shortcutEpoch;
    const shortcut = browsers.shortcutTarget;
    if (epoch === seenShortcut || shortcut === null || shortcut.id !== id) return;
    seenShortcut = epoch;
    untrack(() => run(shortcut.action));
  });

  // ── where the page is ──

  let frame: number | null = null;
  let lastSent: string | null = null;

  function schedule(): void {
    if (frame === null) frame = requestAnimationFrame(flush);
  }

  async function flush(): Promise<void> {
    frame = null;
    const browser = id;
    if (browser === null) return;
    const rect = box?.getBoundingClientRect();
    const show =
      visible &&
      !overlay.covering &&
      !pane.detached &&
      rect !== undefined &&
      rect.width >= 1 &&
      rect.height >= 1;
    const bounds =
      show && rect ? { x: rect.x, y: rect.y, w: rect.width, h: rect.height } : null;
    const key = `${browser}:${JSON.stringify(bounds)}`;
    if (key === lastSent) return;
    const wasShown = lastSent !== null && !lastSent.endsWith(':null');
    lastSent = key;
    if (!show && wasShown && browsers.runtimeUnavailable === null) {
      // The picture has to be taken while the view is still painting, so the hide waits on it —
      // one round trip, during which a dialog's scrim sits under the page. Then the intent is
      // re-checked: a show that landed while this waited must not be followed by a stale hide.
      try {
        const png = await commands.browserSnapshotPng(browser);
        frozen = `data:image/png;base64,${png}`;
      } catch {
        frozen = null;
      }
      if (lastSent !== key) return;
    }
    void commands.browserSetBounds(browser, bounds).catch(() => {
      /* The browser is gone; the pane will hear so. */
    });
    if (show) {
      // Cleared one frame later, once the native view is painting where the picture was.
      requestAnimationFrame(() => {
        if (lastSent === key) frozen = null;
      });
    }
  }

  $effect(() => {
    // Everything that can move or cover the tile without resizing this element: a swap changes the
    // position and not the size, which a `ResizeObserver` cannot see.
    void id;
    void visible;
    void overlay.covering;
    void pane.detached;
    void sessions.layouts[pane.worktreeId];
    schedule();
  });

  $effect(() => {
    const node = box;
    if (!node) return;
    const observer = new ResizeObserver(() => schedule());
    observer.observe(node);
    const onResize = () => schedule();
    window.addEventListener('resize', onResize);
    return () => {
      observer.disconnect();
      window.removeEventListener('resize', onResize);
    };
  });

  $effect(() => {
    return () => {
      // A restart remounts this component under a new key, and the instance going away must not
      // leave its view showing over whatever replaces it.
      if (frame !== null) cancelAnimationFrame(frame);
      const browser = untrack(() => id);
      if (browser !== null) void commands.browserSetBounds(browser, null).catch(() => {});
    };
  });

  /** Called by `SessionPane`'s focus effect: the address for an empty pane, the page otherwise. */
  export function focus(): void {
    if (id === null) {
      address?.focus();
      return;
    }
    void commands.browserFocus(id).catch(() => {});
  }

  function hostOf(url: string): string {
    try {
      return new URL(url).host || url;
    } catch {
      return url;
    }
  }

  function describe(comment: BrowserComment): string {
    const anchor = comment.anchor;
    const says = anchor.name || anchor.text;
    return `${anchor.tag}${says ? ` “${says}”` : ''}`;
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="c-browser" class:is-commenting={commenting} onkeydown={onKey}>
  <div class="c-browser__main">
    <div
      class="c-browser__toolbar"
      class:is-loading={loading}
      role="toolbar"
      aria-label="Browser controls"
    >
      <div class="c-browser__nav">
        <Button
          variant="quiet"
          size="sm"
          icon="sm"
          title="Back (⌘[)"
          ariaLabel="Back"
          disabled={blank || !(view?.canGoBack ?? false)}
          onclick={() => history('back')}
        >
          <Icon name="chevron-left" size={13} />
        </Button>
        <Button
          variant="quiet"
          size="sm"
          icon="sm"
          title="Forward (⌘])"
          ariaLabel="Forward"
          disabled={blank || !(view?.canGoForward ?? false)}
          onclick={() => history('forward')}
        >
          <Icon name="chevron-right" size={13} />
        </Button>
        {#if loading}
          <Button
            variant="quiet"
            size="sm"
            icon="sm"
            title="Stop loading (Esc)"
            ariaLabel="Stop loading"
            onclick={() => history('stop')}
          >
            <Icon name="close" size={12} />
          </Button>
        {:else}
          <Button
            variant="quiet"
            size="sm"
            icon="sm"
            title="Reload (⌘R)"
            ariaLabel="Reload"
            disabled={blank}
            onclick={() => history('reload')}
          >
            <Icon name="restart" size={12} />
          </Button>
        {/if}
      </div>

      <form
        class="c-browser__address"
        class:is-editing={editing}
        onsubmit={(event) => {
          event.preventDefault();
          void go();
        }}
      >
        <TextInput
          mono
          bind:value={typed}
          bind:element={address}
          placeholder="Enter an address (⌘L)"
          ariaLabel="Address"
          oninput={() => {
            editing = true;
            problem = null;
          }}
          onblur={() => (editing = false)}
        />
      </form>
      {#if problem}
        <span class="c-browser__problem" role="alert">{problem}</span>
      {/if}

      <!-- Comment mode and the list are one control: turning the mode on opens the list, and the
           count on the button is how you find comments you left on an earlier visit. -->
      <span class="c-browser__comment-toggle">
        <Button
          variant="quiet"
          size="sm"
          ariaPressed={commenting}
          disabled={blank || runtimeOff !== null}
          title={runtimeOff ??
            (commenting
              ? 'Comment mode is on: click an element in the page to comment on it. Click again to stop.'
              : 'Comment on elements of the page (⌘⇧C)')}
          onclick={() => setCommenting(!commenting)}
        >
          <Icon name="comment" size={13} /> Comment
        </Button>
        {#if comments.length > 0}
          <button
            type="button"
            class="c-badge c-badge--accent"
            title={commentsOpen ? 'Hide the comments' : 'Show the comments'}
            aria-expanded={commentsOpen}
            onclick={() => (commentsOpen = !commentsOpen)}
          >
            {openComments.length}
          </button>
        {/if}
      </span>

      <span
        class="c-browser__agent"
        class:is-driving={(view?.agentDriving ?? null) !== null}
      >
        <Button
          variant={view?.agentAccess && runtimeOff === null ? 'neutral' : 'quiet'}
          size="sm"
          ariaLabel="Allow agents to use this page"
          ariaPressed={runtimeOff === null && (view?.agentAccess ?? false)}
          disabled={blank || runtimeOff !== null || agentAccessPending}
          title={runtimeOff ??
            (blank
              ? 'Open a page to control whether agents may use it.'
              : view?.agentAccess
                ? 'Agents in this worktree may read and interact with this page. Click to turn off access.'
                : 'Agent access is off for this page. Click to allow agents to read and interact with it.')}
          onclick={() => void toggleAgents()}
        >
          Agent access{runtimeOff !== null
            ? ': unavailable'
            : view
              ? view.agentAccess
                ? ': on'
                : ': off'
              : ''}
        </Button>
        {#if view?.agentDriving}
          {view.agentDriving} is browsing
        {/if}
      </span>

      {#if import.meta.env.DEV}
        <Button
          variant="quiet"
          size="sm"
          title="Open the Web Inspector for this page (development builds only)"
          disabled={blank}
          onclick={() => id && void commands.browserOpenDevtools(id).catch(() => {})}
        >
          Inspect
        </Button>
      {/if}

      <Button
        variant="quiet"
        size="sm"
        icon="sm"
        title="Open this page in your browser"
        ariaLabel="Open in your browser"
        disabled={blank}
        onclick={openExternally}
      >
        <Icon name="external" size={13} />
      </Button>
    </div>

    {#if blank}
      <div class="c-browser__empty">
        <p>
          Type an address above, or open one of this worktree's pages. The page runs in a
          real browser inside this pane, and the agents here can read and click through it.
        </p>
        {#if worktree && (worktree.browserHome || worktree.links.length > 0)}
          <div class="c-browser__links">
            {#if worktree.browserHome}
              <Button
                variant="accent"
                size="sm"
                title={worktree.browserHome}
                onclick={() => worktree?.browserHome && open(worktree.browserHome)}
              >
                <Icon name="globe" size={13} />
                {hostOf(worktree.browserHome)}
              </Button>
            {/if}
            {#each worktree.links as link, i (`${link.label}:${i}`)}
              <Button
                variant="neutral"
                size="sm"
                title={link.url}
                onclick={() => open(link.url)}
              >
                {link.label}
              </Button>
            {/each}
          </div>
        {/if}
      </div>
    {:else}
      <div class="c-browser__view" bind:this={box}>
        {#if frozen}
          <img class="c-browser__frozen" src={frozen} alt="" />
        {/if}
      </div>
    {/if}
  </div>

  {#if commentsOpen && !blank}
    <aside class="c-browser__comments" aria-label="Comments on this page">
      <header class="c-browser__comments-head">
        <h3 class="c-browser__comments-title">Comments</h3>
        <Button
          variant="quiet"
          size="sm"
          icon="sm"
          title="Hide the comments"
          ariaLabel="Hide comments"
          onclick={() => (commentsOpen = false)}
        >
          <Icon name="close" size={12} />
        </Button>
      </header>
      {#if comments.length === 0}
        <p class="c-browser__comments-empty">
          {commenting
            ? 'Click an element in the page and say what should change.'
            : 'Turn on comment mode, then click an element in the page.'}
        </p>
      {:else}
        <ol class="o-plain-list c-browser__comment-list">
          {#each comments as comment, index (comment.id)}
            <li
              class="c-browser__comment"
              class:is-resolved={comment.status === 'resolved'}
              class:is-picked={picked === comment.id}
            >
              <div class="c-browser__comment-meta">
                <span class="c-browser__comment-n">{index + 1}</span>
                <span class="c-browser__comment-what" title={comment.anchor.selector}>
                  {describe(comment)}
                </span>
              </div>
              {#if editingComment === comment.id}
                <!-- svelte-ignore a11y_autofocus -->
                <textarea
                  class="c-input c-browser__comment-edit"
                  rows="3"
                  autofocus
                  bind:value={editText}
                  onblur={() => void saveEdit(comment)}
                  onkeydown={(event) => {
                    if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
                      event.preventDefault();
                      void saveEdit(comment);
                    } else if (event.key === 'Escape') {
                      event.stopPropagation();
                      editingComment = null;
                    }
                  }}></textarea>
              {:else}
                <p class="c-browser__comment-text">{comment.text}</p>
              {/if}
              <div class="c-browser__comment-actions">
                <Button variant="quiet" size="sm" onclick={() => startEdit(comment)}
                  >Edit</Button
                >
                <Button variant="quiet" size="sm" onclick={() => toggleResolved(comment)}>
                  {comment.status === 'resolved' ? 'Reopen' : 'Resolve'}
                </Button>
                <Button variant="quiet" size="sm" onclick={() => remove(comment)}
                  >Remove</Button
                >
              </div>
            </li>
          {/each}
        </ol>
      {/if}
      <footer class="c-browser__comments-foot">
        <!-- Drafted, not sent: the message lands in the agent's composer for the user to read,
             add to, and send themselves. -->
        <Button
          variant="accent"
          size="sm"
          disabled={openComments.length === 0 || target === null}
          title={target === null
            ? 'Open an agent session in this worktree first'
            : `Draft these comments into ${targetLabel}'s composer`}
          onclick={send}
        >
          {target
            ? `Send ${openComments.length} to ${targetLabel}`
            : `Send ${openComments.length}`}
        </Button>
      </footer>
    </aside>
  {/if}
</div>
