<script lang="ts">
  /**
   * Home: a place to work from that is not any one worktree.
   *
   * The main column holds Home's own conversation; beside it, everything Home is *about* — the
   * approvals waiting on you anywhere, the session you are peeking at, and what agents have lately
   * said to each other. The tree of every session is in the left column, in the sidebar's place.
   *
   * # Mounted once, then hidden
   *
   * Like every other surface (`SessionSurface`, `DatabaseSurface`), this is hidden rather than
   * unmounted when Home is not showing, because what it holds — a composer's draft, a scroll
   * position — lives in components. It is mounted the first time Home is shown, so somebody who
   * never opens Home never pays for it.
   *
   * # Two shapes
   *
   * Side by side when there is room, and one region with tabs when there is not — at the 860px
   * minimum window with the tree open, the main column is about 580px, and splitting that again
   * would leave two columns too narrow to read a command in. Measured on this element rather than
   * on the window, for the reason the pane header's fold is: what matters is the room this has.
   *
   * # The side column's width
   *
   * Side by side, the line between the two columns drags, and the width is saved beside the
   * sidebar's in `config.toml`. What is saved is the width chosen; what is drawn is that width
   * clamped to the room Home has now. So a window made narrower squeezes the side column only as far
   * as it must, and gets it back when it is widened again. Until it is first dragged it has no
   * width of its own and takes the share it always had.
   */
  import { onMount, type Snippet } from 'svelte';

  import { HOME_AGENT } from '../home';
  import { commands } from '../ipc/commands';
  import { attention } from '../state/attention.svelte';
  import { fleet } from '../state/fleet.svelte';
  import { sessions, type Pane } from '../state/sessions.svelte';
  import { view } from '../state/view.svelte';
  import ActivityList from './ActivityList.svelte';
  import InFlight from './InFlight.svelte';
  import NeedsYou from './NeedsYou.svelte';
  import CreationPanel from './CreationPanel.svelte';
  import PeekPanel from './PeekPanel.svelte';
  import Splitter from './ui/Splitter.svelte';

  const {
    visible,
    onreveal,
    onopenworktree,
    main,
  }: {
    visible: boolean;
    /** Go to a pane in its own worktree. */
    onreveal: (pane: Pane) => void;
    /** Go to a worktree, with nothing in particular to focus. */
    onopenworktree: (projectId: string, worktreeId: string) => void;
    /** Home's own conversation, the main column. */
    main: Snippet;
  } = $props();

  /** Below this content width the side column folds into tabs. */
  const SPLIT = 880;
  /*
   * The side column's limits. 320 is the floor it always had: Peek's header still fits Stop and Open
   * in worktree beside a few words of title. The conversation keeps 440, enough for a line of prose
   * and a composer whose Send button is not wedged against its model picker. At 880, the narrowest
   * Home that is side by side at all, that still leaves the column 320 to 439.
   */
  const SIDE_MIN = 320;
  const MAIN_MIN = 440;
  const SIDE_WIDTH_PREF = 'ui.home_side_width';

  let root = $state<HTMLElement | null>(null);
  let narrow = $state(false);
  /** Home's own width, as last measured while it was showing. */
  let width = $state(0);
  /** The side column's width as the user left it, or `null` until they first move it. */
  let chosen = $state<number | null>(null);
  let dragging = $state(false);

  $effect(() => {
    const el = root;
    if (!el) return;
    const observer = new ResizeObserver(([entry]) => {
      const measured = entry?.contentRect.width ?? 0;
      // A hidden surface measures 0×0, which says nothing about how wide it will be.
      if (measured > 0) {
        width = measured;
        narrow = measured < SPLIT;
      }
    });
    observer.observe(el);
    return () => observer.disconnect();
  });

  /**
   * The `clamp(320px, 36%, 440px)` the column had before it could be dragged, so that nothing moves
   * for somebody who never drags it.
   */
  const fallback = $derived(Math.min(Math.max(Math.round(width * 0.36), SIDE_MIN), 440));
  const sideMax = $derived(Math.max(SIDE_MIN, Math.floor(width) - 1 - MAIN_MIN));
  const sideWidth = $derived(Math.min(Math.max(chosen ?? fallback, SIDE_MIN), sideMax));

  onMount(() => {
    let gone = false;
    void commands
      .getPref(SIDE_WIDTH_PREF)
      .then((stored) => {
        const parsed = stored ? Number.parseInt(stored, 10) : NaN;
        // A drag that beat the read to it is newer than what the read found.
        if (!gone && chosen === null && Number.isFinite(parsed)) chosen = parsed;
      })
      .catch(() => {});
    return () => {
      gone = true;
    };
  });

  function saveSide(next: number) {
    chosen = next;
    void commands.setPref(SIDE_WIDTH_PREF, String(next)).catch(() => {});
  }

  type Tab = 'home' | 'inbox' | 'peek' | 'activity';
  /** The tab on screen when narrow. Wide, only the side column's two compete. */
  let tab = $state<Tab>('home');
  let side = $state<'peek' | 'activity'>('activity');

  const peeked = $derived(sessions.paneById(view.peeked));

  // Choosing a session to peek at is asking to see it, so it comes forward in either shape. Only
  // when the choice changes: a new approval never switches tabs by itself.
  $effect(() => {
    if (view.peeked === null) return;
    // A session chosen over a job: the slot shows one thing at a time.
    fleet.jobShown = null;
    side = 'peek';
    if (narrow) tab = 'peek';
  });
  $effect(() => {
    if (fleet.jobShown === null) return;
    side = 'peek';
    if (narrow) tab = 'peek';
  });

  const job = $derived(fleet.jobs.find((j) => j.id === fleet.jobShown) ?? null);

  // A pane that goes away stops being peeked at, rather than leaving an empty panel.
  $effect(() => {
    if (view.peeked !== null && peeked === null) view.peek(null);
  });

  /*
   * Showing Home is looking at what it shows: the peeked pane is on screen, and every project's
   * listing is worth fetching again. Both on the way in and on coming back to the window, which is
   * the refresh policy the rest of the app follows.
   */
  $effect(() => {
    if (!visible) return;
    void fleet.refresh();
    const onFocus = () => {
      fleet.forgetAgents();
      void fleet.refresh();
      const id = view.peeked;
      if (id) {
        sessions.markPaneSeen(id);
        attention.clearPane(id);
      }
    };
    window.addEventListener('focus', onFocus);
    return () => window.removeEventListener('focus', onFocus);
  });

  function peek(pane: Pane) {
    view.peek(pane.id);
    sessions.markPaneSeen(pane.id);
    attention.clearPane(pane.id);
  }

  const tabs = $derived<{ id: Tab; label: string; count?: number }[]>([
    { id: 'home', label: HOME_AGENT },
    { id: 'inbox', label: 'Needs you', count: fleet.needsYou.length },
    { id: 'peek', label: 'Peek' },
    { id: 'activity', label: 'Activity' },
  ]);
</script>

{#snippet peekOrHint()}
  {#if job}
    <CreationPanel {job} {onopenworktree} />
  {:else if peeked}
    <PeekPanel pane={peeked} {onreveal} />
  {:else}
    <p class="c-home__hint">
      Pick a session in the tree to read what it is doing and write to it, without going to
      its worktree.
    </p>
  {/if}
{/snippet}

<section
  class="c-home"
  class:is-hidden={!visible}
  class:is-narrow={narrow}
  class:is-dragging={dragging}
  bind:this={root}
  aria-label="Home"
>
  {#if narrow}
    <div class="c-tabs c-tabs--inset" role="tablist" aria-label="Home">
      {#each tabs as entry (entry.id)}
        <button
          class="c-tabs__tab"
          class:is-active={tab === entry.id}
          role="tab"
          aria-selected={tab === entry.id}
          aria-controls="home-{entry.id}"
          onclick={() => (tab = entry.id)}
        >
          {entry.label}{#if entry.count}{' '}<span class="c-tabs__count">{entry.count}</span
            >{/if}
        </button>
      {/each}
    </div>
  {/if}

  <div
    class="c-home__main"
    id="home-home"
    role={narrow ? 'tabpanel' : undefined}
    hidden={narrow && tab !== 'home'}
  >
    {@render main()}
  </div>

  {#if narrow}
    <div class="c-home__panel" id="home-inbox" role="tabpanel" hidden={tab !== 'inbox'}>
      {#if fleet.needsYou.length === 0}
        <p class="c-home__hint">Nothing is waiting on you.</p>
      {/if}
      <NeedsYou onpeek={peek} {onreveal} />
    </div>
    <div class="c-home__panel" id="home-peek" role="tabpanel" hidden={tab !== 'peek'}>
      {@render peekOrHint()}
    </div>
    <div
      class="c-home__panel"
      id="home-activity"
      role="tabpanel"
      hidden={tab !== 'activity'}
    >
      <div class="c-home__inbox">
        <InFlight onpeek={peek} />
      </div>
      <ActivityList />
    </div>
  {:else}
    <Splitter
      value={sideWidth}
      min={SIDE_MIN}
      max={sideMax}
      label="Resize the side panel"
      grows="left"
      controls="home-side"
      onresize={(next) => (chosen = next)}
      oncommit={saveSide}
      ondrag={(active) => (dragging = active)}
    />
    <aside
      class="c-home__side"
      id="home-side"
      style:--home-side-w="{sideWidth}px"
      aria-label="What Home is watching"
    >
      <div class="c-home__inbox">
        <NeedsYou onpeek={peek} {onreveal} />
        <InFlight onpeek={peek} />
      </div>
      <div class="c-tabs c-tabs--inset" role="tablist" aria-label="Side panel">
        <button
          class="c-tabs__tab"
          class:is-active={side === 'peek'}
          role="tab"
          aria-selected={side === 'peek'}
          aria-controls="home-side-panel"
          onclick={() => (side = 'peek')}
        >
          Peek
        </button>
        <button
          class="c-tabs__tab"
          class:is-active={side === 'activity'}
          role="tab"
          aria-selected={side === 'activity'}
          aria-controls="home-side-panel"
          onclick={() => (side = 'activity')}
        >
          Activity
        </button>
      </div>
      <div class="c-home__panel" id="home-side-panel" role="tabpanel">
        {#if side === 'peek'}
          {@render peekOrHint()}
        {:else}
          <ActivityList />
        {/if}
      </div>
    </aside>
  {/if}
</section>
