<script lang="ts">
  /**
   * Every worktree's split tree, all mounted, all but the active one hidden.
   *
   * # Why this is mounted by the shell and not by the worktree pane
   *
   * A transcript lives in its component — an xterm instance for a shell, a rendered list for a chat —
   * so unmounting one throws away what is on screen. `Detail` is destroyed whenever the main pane
   * switches views, and momentarily whenever a project switch lands on an empty cached list. Living
   * above it, this is only ever *hidden*.
   *
   * That is the reasoning the terminal dock carried before it became one kind of session, and it
   * applies unchanged: hiding is `display: none`, because a hidden pane must cost nothing to lay out
   * — xterm's DOM renderer writes real rows on every chunk — and because a 0×0 `ResizeObserver` fire
   * doubles as the signal that a pane came back. `_surface.scss` keeps the rejected alternatives.
   *
   * # What it does not do
   *
   * It does not open a session for a worktree because you selected one. Browsing six worktrees would
   * otherwise start six CLIs. A session starts when someone asks.
   */
  import { onMount, untrack } from 'svelte';

  import type { Brief } from '../ipc/types';
  import { AT_CAPACITY, sessions } from '../state/sessions.svelte';
  import { workspace } from '../state/workspace.svelte';
  import AgentsDialog from './AgentsDialog.svelte';
  import AgentTree from './AgentTree.svelte';
  import PlanViewer from './PlanViewer.svelte';
  import SessionTree from './SessionTree.svelte';
  import Button from './ui/Button.svelte';

  const {
    visible,
  }: {
    /** False while the create pane owns the screen. Hidden, never unmounted. */
    visible: boolean;
  } = $props();

  const activeId = $derived(workspace.selectedWorktreeId);
  /** Whether the full delegated-agent list is open. The rail only summarises. */
  let browsingAgents = $state(false);
  /** Worktrees that have any pane at all, so nothing renders an empty tree. */
  const occupied = $derived([...new Set(sessions.panes.map((p) => p.worktreeId))]);
  const activeLayout = $derived(sessions.layoutFor(activeId));
  const resumable = $derived(activeId ? (sessions.resumable[activeId] ?? []) : []);
  const briefs = $derived(activeId ? (sessions.briefs[activeId] ?? []) : []);
  const background = $derived(activeId ? (sessions.background[activeId] ?? []) : []);
  const stillRunning = $derived(
    background.filter((t) => t.state !== 'done' && t.state !== 'failed'),
  );
  /**
   * The agents that could actually start here: installed **and** offered by this repository.
   *
   * `available` alone offered agents whose spawn `open_agent_session` refuses on `offers_agent`, so
   * a repo that turned Codex off still showed a Codex button that failed with a config error once
   * clicked. Two flags rather than one because the two refusals have different fixes.
   */
  const startable = $derived(sessions.options.filter((o) => o.available && o.offered));

  /**
   * The stored plan being read, if any.
   *
   * The `Brief` itself rather than its id, so the dialog survives the list refreshing underneath
   * it — `sessions.briefs` is replaced wholesale by `refreshBriefs`, and an id would resolve to
   * `undefined` mid-read and close the dialog for no visible reason.
   */
  let reading = $state<Brief | null>(null);

  function resumeLabel(record: (typeof resumable)[number]): string {
    if (record.title?.trim()) return record.title;
    const provider =
      sessions.options.find((option) => option.id === record.provider)?.label ??
      record.provider;
    const updated = record.updated ? new Date(record.updated) : null;
    const when =
      updated && !Number.isNaN(updated.valueOf())
        ? updated.toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
        : 'saved conversation';
    return `${provider} · ${when}`;
  }

  /*
   * Read what can be resumed, what plans are stored, and what is running, when the selection lands
   * somewhere new.
   *
   * On selection rather than on a timer: polling is banned, and the list only changes when this
   * window opens or closes a session — which is exactly when the store refreshes it itself.
   *
   * # This effect must depend on the selection and nothing else
   *
   * All three of these write a map. Each used to *read* that map before its first `await`, which
   * put the read inside this effect's tracking window and made the effect depend on its own writes
   * — so it re-ran forever. `refreshBackground` shells out to `claude agents` every call, and the
   * Claude Code binary is a few hundred megabytes, so the loop was a fork bomb: hundreds of
   * resident copies, the machine in swap, and `kernel_task` pegged.
   *
   * **The fix is in the store, not here.** The three methods now await before they touch the map,
   * and skip the assignment when nothing moved. `untrack` around these calls was tried and does
   * *not* work — measured against this project's Svelte, a pre-`await` read stays tracked whether
   * it is wrapped or not, so wrapping it would only have looked like a guard. If you add a fourth
   * call, the rule it has to follow is the one in `sessions.svelte.ts`, not one available here.
   */
  $effect(() => {
    const worktree = workspace.selectedWorktreeId;
    const project = workspace.activeProjectId;
    if (!worktree || !project) return;
    void sessions.refreshResumable(worktree);
    void sessions.refreshBriefs(project, worktree);
    void sessions.refreshBackground(worktree);
    // The fourth call, and it follows the same rule: `refreshOptions` awaits before it touches
    // `options` and skips an assignment that would change nothing. `offered` is the repository's
    // answer, so it has to be re-asked when the project moves.
    void sessions.refreshOptions(project);
  });

  /*
   * Selecting a worktree is looking at it.
   *
   * A separate effect rather than a line in the one above, because that effect's comment insists it
   * depend on the selection and nothing else, and it deserves to keep saying so — the fork bomb it
   * describes is what happens when something is added to it carelessly.
   *
   * One effect covers *every* route to a selection: `select`, `selectRelative`, `selectProject`, the
   * sidebar's arrow keys, a toast click, and the cache-restore path. Patching the three call sites
   * instead would have missed the last two.
   *
   * `untrack` because the body writes `pane.unseen` on panes it finds by scanning `panes`, and an
   * effect that both reads and writes that array is exactly the loop the other comment describes. It
   * genuinely works here, unlike at the call sites that comment warns about: what defeats `untrack`
   * there is a read that happens *before an await* inside an async function, which no wrapper can
   * contain. This body is synchronous.
   */
  $effect(() => {
    const worktree = workspace.selectedWorktreeId;
    if (!visible) return;
    untrack(() => sessions.markSeen(worktree));
  });

  /*
   * Fill in a restored worktree's shells the first time it is looked at.
   *
   * Here rather than in `sessions.init` so a launch with six remembered worktrees spawns nothing
   * until one of them is opened — the same judgement `sessions.rs` makes about not forking a CLI
   * per remembered conversation, applied to the one session kind that has nothing to resume.
   *
   * `untrack` for the reason the effect above gives: the body reads and writes `panes`, and it is
   * synchronous, which is what makes the wrapper work here and not at the async call sites.
   */
  $effect(() => {
    const worktree = workspace.selectedWorktreeId;
    const project = workspace.activeProjectId;
    if (!worktree || !project || !visible) return;
    untrack(() => void sessions.materialise(project, worktree));
  });

  /*
   * Re-read the background roster on window focus.
   *
   * There is no event when a background agent finishes, so this is the same trigger `workspace` uses
   * for the worktree list, and for the same reason: polling a state directory is how these tools end
   * up spinning a fan. The cost is a count that can be a few seconds stale, which the copy says.
   */
  $effect(() => {
    const onFocus = () => {
      const worktree = workspace.selectedWorktreeId;
      if (worktree) void sessions.refreshBackground(worktree);
    };
    window.addEventListener('focus', onFocus);
    return () => window.removeEventListener('focus', onFocus);
  });

  /*
   * Drop panes whose worktree has gone.
   *
   * Guarded on the list being authoritative: a cached or still-loading list is missing worktrees that
   * do exist, and ending a session because a refresh has not landed is the worst false positive
   * available. Same guard, same reason, as the dock's.
   */
  $effect(() => {
    const projectId = workspace.activeProjectId;
    if (!projectId || workspace.stale || workspace.loadingWorktrees) return;
    sessions.reconcile(
      projectId,
      workspace.worktrees.map((w) => w.id),
    );
  });

  onMount(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey)) return;
      if (!visible) return;
      // Not behind a modal: a dialog's focus trap would bounce focus straight back out. `[aria-modal]`
      // is the hook because it is the promise the dialog makes, and the rule is to select on ARIA.
      if (document.querySelector('[aria-modal="true"]')) return;

      const project = workspace.activeProjectId;
      const worktree = workspace.selectedWorktreeId;
      if (!project || !worktree) return;

      if (event.key === 'j') {
        // "Get me to a terminal here", which is what it has always meant. With more than one shell
        // open, repeating it walks between them — see `focusOrOpenShell`. Opening a *second* shell
        // is the pane's own Split control, deliberately: a shortcut that spawned a login shell on
        // every press would be a way to hit the pane cap by holding a key down.
        event.preventDefault();
        event.stopPropagation();
        void sessions.focusOrOpenShell(project, worktree);
      }
    };

    /*
     * Capture, and propagation stopped.
     *
     * xterm listens on the textarea it keeps focused, so a bubble-phase handler runs *after* the byte
     * has gone to the shell — `preventDefault` alone would open a pane and type a line feed. Scoped
     * to this one chord so every other key keeps its ordinary route.
     */
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  });
</script>

<div class="c-surface" class:is-hidden={!visible} aria-label="Sessions">
  {#if activeId}
    <AgentTree worktreeId={activeId} onbrowse={() => (browsingAgents = true)} />
  {/if}
  {#each occupied as worktreeId (worktreeId)}
    {@const layout = sessions.layoutFor(worktreeId)}
    {#if layout}
      <!--
        Every worktree's tree stays mounted; all but the active one are `display: none`. See the
        header — this is the property the whole component exists for.
      -->
      <div class="c-surface__tree" class:is-inactive={worktreeId !== activeId}>
        <SessionTree {layout} {worktreeId} visible={visible && worktreeId === activeId} />
      </div>
    {/if}
  {/each}

  {#if !activeLayout}
    <div class="c-surface__empty">
      {#if sessions.atCapacity}
        <!-- The same sentence the banner carries. This branch is reachable only when the worktree
             has no panes of its own while some *other* worktree holds the global cap; the banner
             covers the far commoner case of being refused while looking at a full surface. -->
        <p>{AT_CAPACITY}</p>
      {:else if workspace.selected && workspace.activeProjectId}
        <p>
          Start a session in <code>{workspace.selected.dirname}</code>. It runs in that
          directory and can read and change the files there.
        </p>
        <div class="o-row">
          <!-- Every agent, including the ones that cannot start, each with the reason in its
               tooltip — the contract `list_agents` keeps and the diagnosis for this app's most
               likely failure. `offered` as well as `available`, because a repository declining an
               agent used to leave an enabled button whose spawn was refused. -->
          {#each sessions.options as option (option.id)}
            {@const startable = option.available && option.offered}
            <Button
              variant={startable ? 'accent' : 'neutral'}
              size="sm"
              disabled={!startable}
              title={option.detail ?? option.blurb}
              onclick={() =>
                void sessions.openAgent(
                  workspace.activeProjectId ?? '',
                  workspace.selected?.id ?? '',
                  option.id,
                )}
            >
              {option.label}
            </Button>
          {/each}
          <Button
            variant="neutral"
            size="sm"
            title="Open a shell in this worktree"
            onclick={() =>
              void sessions.openShell(
                workspace.activeProjectId ?? '',
                workspace.selected?.id ?? '',
              )}
          >
            Shell
          </Button>
        </div>
        {#if resumable.length > 0}
          <!--
            Offered rather than resumed automatically. Re-establishing every conversation on launch
            would fork a CLI per pane for sessions the user may be finished with — the same judgement
            the terminal dock made in deciding its open state must not persist.
          -->
          <h2 class="c-section-heading">Pick up where you left off</h2>
          <ul class="o-plain-list c-surface__resume">
            {#each resumable as record (record.provider + record.providerSession)}
              <li class="o-row">
                <Button
                  variant="neutral"
                  size="sm"
                  title="Resume this conversation on {record.model ?? 'its own model'}"
                  onclick={() =>
                    void sessions.resume(
                      workspace.activeProjectId ?? '',
                      workspace.selected?.id ?? '',
                      record,
                    )}
                >
                  {resumeLabel(record)}
                </Button>
                <span class="c-status--subtle">{record.provider}</span>
                <button
                  class="c-row-action"
                  title="Stop offering this conversation"
                  onclick={() => void sessions.forget(workspace.selected?.id ?? '', record)}
                >
                  forget
                </button>
              </li>
            {/each}
          </ul>
        {/if}

        {#if briefs.length > 0}
          <!--
            Plans kept from a session that has ended. Handing one to another agent is the whole reason
            they are stored: the alternative is scrolling back through a transcript that may be gone.
          -->
          <h2 class="c-section-heading">Plans</h2>
          <ul class="o-plain-list c-surface__resume">
            {#each briefs as brief (brief.id)}
              <li class="o-row">
                <!-- A button, because the whole plan used to be reachable only as a native
                     `title=` tooltip truncated to 400 characters — which is not reading a plan, it
                     is guessing at one. -->
                <button
                  class="c-surface__brief"
                  title="Read this plan"
                  onclick={() => (reading = brief)}
                >
                  {brief.title}
                </button>
                <span class="c-status--subtle">{brief.provider}</span>
                {#each startable as option (option.id)}
                  <button
                    class="c-row-action"
                    title="Open a {option.label} session and hand it this plan"
                    onclick={() =>
                      void sessions.handOff(
                        workspace.activeProjectId ?? '',
                        workspace.selected?.id ?? '',
                        option.id,
                        brief,
                      )}
                  >
                    → {option.label}
                  </button>
                {/each}
                <button
                  class="c-row-action"
                  onclick={() =>
                    void sessions.forgetBrief(
                      workspace.activeProjectId ?? '',
                      workspace.selected?.id ?? '',
                      brief.id,
                    )}
                >
                  forget
                </button>
              </li>
            {/each}
          </ul>
        {/if}

        {#if background.length > 0}
          <!--
            Claude Code only: Codex has no equivalent roster, and its long-running work is another live
            thread that already shows as a pane. The "as of the last check" is not hedging — there is no
            event when one of these finishes, so the count genuinely can be a few seconds stale, and
            saying so is better than a timer.
          -->
          <h2 class="c-section-heading">
            Background agents
            <span class="c-status--subtle">
              {stillRunning.length} running, as of the last check
            </span>
          </h2>
          <ul class="o-plain-list c-surface__resume">
            {#each background as task (task.id)}
              <li class="o-row">
                <span class="c-surface__brief">{task.name}</span>
                <span
                  class={task.state === 'failed'
                    ? 'c-status--danger'
                    : task.state === 'done'
                      ? 'c-status--ok'
                      : 'c-status--info'}
                >
                  {task.state}
                </span>
              </li>
            {/each}
          </ul>
        {/if}

        <!-- Two messages rather than one, because the two ways of having nothing to start have
             different fixes and only one of them is about this machine. Keyed on `available` alone
             first, so the PATH diagnosis keeps saying exactly what it always said. -->
        {#if sessions.options.every((o) => !o.available)}
          <p class="c-status--warn">
            No agent CLI is on wtm's PATH. Settings → Advanced shows the PATH wtm resolved.
          </p>
        {:else if startable.length === 0}
          <p class="c-status--warn">
            This repository's <code>wtm.toml</code> does not offer any of the agents installed
            here. Hover a button above for which.
          </p>
        {/if}
      {:else}
        <p>Select a worktree to start a session in it.</p>
      {/if}
    </div>
  {/if}
</div>

<!-- Outside `.c-surface`, beside the plan viewer, because a dialog is not part of the pane
     geometry — and unmounted when shut, which is what makes it free. -->
{#if browsingAgents && activeId}
  <AgentsDialog worktreeId={activeId} onclose={() => (browsingAgents = false)} />
{/if}

{#if reading}
  <!-- Outside the `visible` gate on purpose: this surface is hidden with `display: none` while the
       create pane owns the screen, and a modal inside a hidden subtree is a scrim over nothing. -->
  <PlanViewer
    title={reading.title}
    markdown={reading.markdown}
    provider={reading.provider}
    created={reading.created}
    onclose={() => (reading = null)}
  >
    {#snippet actions()}
      <!-- The same hand-off the row offers, repeated here because having just read the plan is
           exactly when you decide who should act on it. -->
      {#each startable as option (option.id)}
        <Button
          variant="neutral"
          size="sm"
          title="Open a {option.label} session and hand it this plan"
          onclick={() => {
            const brief = reading;
            reading = null;
            if (brief) {
              void sessions.handOff(
                workspace.activeProjectId ?? '',
                workspace.selected?.id ?? '',
                option.id,
                brief,
              );
            }
          }}
        >
          → {option.label}
        </Button>
      {/each}
    {/snippet}
  </PlanViewer>
{/if}
