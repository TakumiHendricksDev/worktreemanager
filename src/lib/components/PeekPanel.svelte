<script lang="ts">
  /**
   * A look at one session from Home, without going to its worktree: its transcript, its oldest
   * waiting approval, and a box to write to it.
   *
   * # Not a second `SessionPane`
   *
   * The session's own pane is already mounted, hidden, in its worktree — every pane stays mounted —
   * and a `SessionPane` holds state of its own: the draft, the scroll position, a focus effect, a
   * drop listener. Mounting a second one for the same pane would give it two composers that
   * disagree and two focus effects that fight. So this draws the parts that live on the *pane*
   * rather than in the component — the event log, the approvals, the queue — and leaves the rest to
   * the pane itself, one click away under "Open in worktree".
   *
   * What is written here goes through the same store calls the pane's own composer uses, so a
   * message queued from Home is in that pane's queue when you get there.
   */
  import { tick } from 'svelte';

  import { fileIndex, resolveRef, type CodeRef } from '../code-links';
  import { codeRequests } from '../state/code-request.svelte';
  import { composerPrefs } from '../state/composer.svelte';
  import { shellRuns } from '../state/shell-runs.svelte';
  import { databaseConsole } from '../state/database-console.svelte';
  import { fleet } from '../state/fleet.svelte';
  import { sessions, type Pane } from '../state/sessions.svelte';
  import { view } from '../state/view.svelte';
  import { workspace } from '../state/workspace.svelte';
  import { STATUS_WORD } from '../status';
  import AgentTranscript from './AgentTranscript.svelte';
  import ApprovalCard from './ApprovalCard.svelte';
  import ComposerQueue from './ComposerQueue.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';
  import SessionDot from './ui/SessionDot.svelte';

  const {
    pane,
    onreveal,
  }: {
    pane: Pane;
    /** Go to the pane in its worktree. */
    onreveal: (pane: Pane) => void;
  } = $props();

  const status = $derived(sessions.statusOfPane(pane));
  const label = $derived(sessions.labelOf(pane));
  const title = $derived(pane.agentTitle ?? pane.firstPrompt ?? label);
  const out = $derived(sessions.isOut(pane.id));
  const steersMidTurn = $derived(
    pane.kind.kind === 'agent'
      ? (sessions.capabilities[pane.kind.provider]?.steersMidTurn ?? false)
      : false,
  );

  const where = $derived.by(() => {
    const project = workspace.projects.find((p) => p.id === pane.projectId);
    const worktree = fleet
      .worktreesOf(pane.projectId)
      ?.find((w) => w.id === pane.worktreeId);
    return `${project?.name ?? 'A project'} / ${worktree?.title ?? pane.worktreeId.split('/').pop()}`;
  });

  const blocking = $derived(pane.approvals[0] ?? null);

  // The same file links the pane's own transcript has, resolved against that worktree's files.
  $effect(() => {
    if (!sessions.files[pane.worktreeId]) void sessions.loadFiles(pane.worktreeId);
  });
  const codeLink = $derived.by(() => {
    const root =
      fleet.worktreesOf(pane.projectId)?.find((w) => w.id === pane.worktreeId)?.path ??
      pane.worktreeId;
    const index = fileIndex(sessions.files[pane.worktreeId] ?? [], root);
    return {
      resolve: (text: string) => resolveRef(text, index),
      open: (ref: CodeRef) =>
        codeRequests.open(pane.projectId, pane.worktreeId, ref.path, ref.line),
    };
  });

  // Follow the tail while the reader is at it, as the pane does.
  let scroller = $state<HTMLDivElement | null>(null);
  let pinned = $state(true);
  function onScroll() {
    if (!scroller) return;
    pinned = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 32;
  }
  $effect(() => {
    void pane.events.length;
    void blocking;
    if (!pinned || !scroller) return;
    void tick().then(() => {
      if (scroller) scroller.scrollTop = scroller.scrollHeight;
    });
  });
  // A different pane starts at its own tail.
  $effect(() => {
    void pane.id;
    pinned = true;
  });

  let draft = $state('');
  let sending = $state(false);

  async function submit(steer: boolean) {
    const text = draft.trim();
    if (text === '' || sending) return;
    // Busy: into the pane's own queue, which its worktree tile shows too. A steer is a queued
    // entry handed straight to the running turn — the pane's own route for ⇧⌘⏎.
    if (pane.working || pane.queue.length > 0) {
      const id = sessions.enqueue(pane.id, text, []);
      draft = '';
      if (steer && id) await sessions.steerQueued(pane.id, id);
      return;
    }
    sending = true;
    try {
      if (await sessions.send(pane.id, text)) draft = '';
    } finally {
      sending = false;
    }
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key !== 'Enter' || event.isComposing) return;
    const mod = event.metaKey || event.ctrlKey;
    if (event.shiftKey && mod) {
      event.preventDefault();
      void submit(true);
    } else if (mod || (composerPrefs.sendKey === 'enter' && !event.shiftKey)) {
      event.preventDefault();
      void submit(false);
    }
  }
</script>

<section class="c-peek" aria-label="Peek: {title}">
  <header class="c-peek__head">
    <SessionDot {status} />
    <div class="c-peek__titles">
      <h2 class="c-peek__title" {title}>{title}</h2>
      <p class="c-peek__where">
        {label}{#if pane.model}
          · {pane.model}{/if} · {where}
        {#if STATUS_WORD[status]}<span class="c-peek__status">· {STATUS_WORD[status]}</span
          >{/if}
        {#if pane.openedFromHome}
          · Opened by Home{/if}
      </p>
    </div>
    <span class="c-peek__actions">
      {#if pane.working}
        <Button
          variant="neutral"
          size="sm"
          onclick={() => void sessions.interrupt(pane.id)}
        >
          Stop
        </Button>
      {/if}
      <Button variant="neutral" size="sm" onclick={() => onreveal(pane)}
        >Open in worktree</Button
      >
      <Button
        variant="quiet"
        icon="sm"
        title="Stop peeking"
        ariaLabel="Stop peeking"
        onclick={() => view.peek(null)}
      >
        <Icon name="close" size={12} />
      </Button>
    </span>
  </header>

  <div class="c-peek__body" bind:this={scroller} onscroll={onScroll}>
    {#if pane.events.length === 0}
      <p class="c-peek__empty">
        {pane.restoring
          ? 'Picking this conversation up again…'
          : pane.detached
            ? 'This session is not running.'
            : `Nothing from ${label} yet.`}
      </p>
    {/if}
    <AgentTranscript
      events={pane.events}
      onrunshell={(snippet) => shellRuns.open(pane.projectId, pane.worktreeId, snippet)}
      onrunsql={(sql) => databaseConsole.open(pane.projectId, pane.worktreeId, sql)}
      {codeLink}
    />
    {#if blocking}
      <div class="c-peek__approval">
        <ApprovalCard
          request={blocking.request}
          focusOnMount={false}
          onanswer={(answer) =>
            void sessions.answerAndKeep(pane.id, blocking.id, answer, blocking.request)}
        />
      </div>
    {/if}
  </div>

  {#if pane.queue.length > 0}
    <ComposerQueue {pane} {label} {steersMidTurn} />
  {/if}

  {#if pane.detached && pane.restoring}
    <div class="c-peek__state">
      <p>Restored from the last run, and on its way back.</p>
    </div>
  {:else if pane.detached && pane.error !== null}
    <div class="c-peek__state">
      <p>Couldn't pick this conversation up again: {pane.error}</p>
      <div class="o-row">
        <Button
          variant="neutral"
          size="sm"
          onclick={() => void sessions.startFresh(pane.id)}
        >
          Start fresh
        </Button>
        <Button variant="quiet" size="sm" onclick={() => void sessions.reattach(pane.id)}>
          Try again
        </Button>
      </div>
    </div>
  {:else if pane.detached}
    <div class="c-peek__state">
      <p>Restored from the last run, with no process behind it.</p>
      <Button variant="neutral" size="sm" onclick={() => void sessions.reattach(pane.id)}>
        Resume
      </Button>
    </div>
  {:else if pane.ended !== null || pane.error !== null}
    <div class="c-peek__state">
      <p>{pane.error ?? `Ended: ${pane.ended}`}</p>
      <Button variant="neutral" size="sm" onclick={() => void sessions.restart(pane.id)}>
        Restart
      </Button>
    </div>
  {:else if out}
    <!-- That window drains this pane's queue, so a message queued here would never be sent. -->
    <div class="c-peek__state">
      <p>This pane is in a window of its own.</p>
      <Button
        variant="neutral"
        size="sm"
        onclick={() => sessions.focus(pane.worktreeId, pane.id)}
      >
        Bring it forward
      </Button>
    </div>
  {:else}
    <form
      class="c-peek__composer"
      onsubmit={(event) => {
        event.preventDefault();
        void submit(false);
      }}
    >
      <label class="u-visually-hidden" for="peek-input-{pane.id}">Message {label}</label>
      <textarea
        id="peek-input-{pane.id}"
        class="c-peek__input"
        rows="2"
        bind:value={draft}
        onkeydown={onKeydown}
        placeholder={pane.working ? `Queue a message for ${label}` : `Message ${label}`}
      ></textarea>
      <Button
        variant="accent"
        size="sm"
        type="submit"
        disabled={draft.trim() === '' || sending}
      >
        {pane.working ? 'Queue' : 'Send'}
      </Button>
    </form>
  {/if}
</section>
