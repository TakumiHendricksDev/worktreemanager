<script lang="ts">
  /**
   * One session, whatever kind it is: a header, a body, and for an agent a composer.
   *
   * # Why one component for a shell and a chat
   *
   * Everything around the body is identical — the title, the status line, Restart and Close, the
   * focus handling, the "this session ended" state. Two components would be two copies of that, and
   * the copy would drift. The body is the only part that differs, so the body is the only part that
   * switches.
   *
   * The alternative — a `SessionPane` that only wraps and delegates — was tried and abandoned: it
   * meant every prop passed twice and every state class defined twice.
   */
  import { getCurrentWebview } from '@tauri-apps/api/webview';
  import { open } from '@tauri-apps/plugin-dialog';
  import { onDestroy } from 'svelte';

  import { commands } from '../ipc/commands';
  import { errorMessage, type AgentAttachment } from '../ipc/types';
  import { commandsFor } from '../agent-commands';
  import { composerPrefs } from '../state/composer.svelte';
  import { DESTINATION, dictation } from '../state/dictate.svelte';
  import { sessions, type Pane } from '../state/sessions.svelte';
  import { STATUS_WORD, type PaneStatus } from '../status';
  import { accept, matchFiles, matchSkills, queryAt, relativise } from '../suggest';
  import AgentTranscript from './AgentTranscript.svelte';
  import ApprovalCard from './ApprovalCard.svelte';
  import Markdown from './Markdown.svelte';
  import ModelPicker from './ModelPicker.svelte';
  import SideQuestion from './SideQuestion.svelte';
  import Suggest from './Suggest.svelte';
  import Terminal from './Terminal.svelte';
  import Banner from './ui/Banner.svelte';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Icon from './ui/Icon.svelte';
  import SessionDot from './ui/SessionDot.svelte';

  const {
    pane,
    visible,
    onmovestart,
    onmovekey,
  }: {
    pane: Pane;
    visible: boolean;
    /**
     * The grip was pressed. Raw event, so the tree can decide what it means.
     *
     * Required rather than optional, deliberately: `svelte-check` is the only guard this codebase has
     * on a component's call sites, so a required prop is what turns "someone rendered a pane without
     * wiring the grip" into a build error instead of a handle that silently does nothing.
     *
     * The geometry stays out of this file. `SessionTree` owns the pane rectangles, so it is the only
     * thing that can say what a drag or an arrow key resolves to.
     */
    onmovestart: (event: PointerEvent) => void;
    onmovekey: (event: KeyboardEvent) => void;
  } = $props();

  let draft = $state('');
  let attachments = $state<AgentAttachment[]>([]);
  let contextOpen = $state(false);
  let skillsOpen = $state(false);
  let confirmRestart = $state(false);
  let confirmClose = $state(false);
  let closingSession = $state(false);
  let copiedReply = $state(false);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;
  onDestroy(() => clearTimeout(copyTimer));
  /**
   * True from submit until the turn is accepted or refused.
   *
   * A turn can now wait several seconds for a session that is still starting, so the control has to
   * say it is busy — and a second click during that wait would send the same text twice.
   */
  let sending = $state(false);
  let scroller = $state<HTMLElement | null>(null);
  /** What is inside the scroller. Observed for height, which the scroller itself cannot report. */
  let content = $state<HTMLElement | null>(null);
  let terminal = $state<ReturnType<typeof Terminal> | null>(null);
  /**
   * Whether the transcript is following its tail.
   *
   * Recorded from a scroll listener rather than measured in the anchoring observer: by the time
   * that runs the DOM has grown, so measuring then answers "is it pinned *after* the append" —
   * which on first content is `scrollTop === 0` against a tall scroller and reads as "scrolled
   * away". Not `$state`: nothing renders from it.
   */
  let pinned = true;

  /**
   * The provider id, or null for a shell.
   *
   * Narrowed once into a local rather than re-narrowed at each use: Svelte's template reads
   * `pane.kind` through a `$props()` getter, so TypeScript will not carry a narrowing across one.
   */
  const provider = $derived(pane.kind.kind === 'agent' ? pane.kind.provider : null);

  const label = $derived.by(() => {
    if (provider === null) return 'Shell';
    return sessions.options.find((o) => o.id === provider)?.label ?? provider;
  });

  const isFocused = $derived(sessions.focused[pane.worktreeId] === pane.id);

  /**
   * What a restored pane says about the conversation it is holding a place for.
   *
   * The title comes from the resume list rather than from the pane, so there is one copy of it and
   * it is the backend's. A pane whose conversation the user has since forgotten falls back to
   * offering a fresh session, which is the honest thing to say when there is nothing to resume.
   */
  const offer = $derived.by(() => {
    if (!pane.providerSession) return `This pane held a ${label} session.`;
    const record = (sessions.resumable[pane.worktreeId] ?? []).find(
      (r) => r.providerSession === pane.providerSession,
    );
    return record?.title?.trim() ? record.title : `A ${label} conversation was open here.`;
  });

  /**
   * This pane's state, as one word.
   *
   * Replaces a backward scan over `pane.events` for the nearest `turn_started`/`turn_finished`, which
   * was O(events) against a log bounded at 20 000 — affordable for one reader and not for the sidebar,
   * which needs the same answer per row. `sessions.record` now maintains `pane.working` in O(1) and
   * `status.ts` owns the vocabulary. See both for why.
   */
  const status = $derived(sessions.statusOfPane(pane));

  /**
   * Which `c-status--*` tone the header note takes.
   *
   * A lookup rather than interpolation, for the reason every UI primitive here states its contract as
   * a union: the stylesheet is global, so a tone that does not exist fails silently, and a typed
   * record is the only thing that catches it.
   */
  const TONE: Record<PaneStatus, string> = {
    failed: 'c-status--danger',
    ended: 'c-status--warn',
    attention: 'c-status--warn',
    working: 'c-status--info',
    done: 'c-status--info',
    detached: 'c-status--subtle',
    starting: 'c-status--subtle',
    idle: 'c-status--subtle',
  };

  /**
   * What the header note says.
   *
   * The two states that have a message of their own use it; the rest use the status word. An error or
   * an exit summary says *what* went wrong, which "failed" does not.
   */
  const note = $derived(
    status === 'failed'
      ? pane.error
      : status === 'ended'
        ? pane.ended
        : STATUS_WORD[status],
  );

  /** The oldest unanswered approval. One at a time, in arrival order. */
  const blocking = $derived(pane.approvals[0] ?? null);

  /** The session that delegated to this one, when it is still around to go back to. */
  const origin = $derived.by(() => {
    const parent = pane.parentSession ? sessions.paneBySession(pane.parentSession) : null;
    if (!parent) return null;
    const kind = parent.kind;
    const label =
      kind.kind === 'agent'
        ? (sessions.options.find((o) => o.id === kind.provider)?.label ?? kind.provider)
        : 'Shell';
    return { id: parent.id, label };
  });

  /**
   * A child's question, asked here.
   *
   * # Why the orchestrator answers for its children
   *
   * Because a six-way fan-out otherwise costs six pane visits to clear six `Bash` prompts — and the
   * panes are not on screen to be visited: a delegated child holds no tile until it is selected, so
   * "go and answer it" first means finding it. Nothing about an approval needed the pane to be
   * visible; it lives on `pane.approvals` and `answer` takes a pane id. The only thing missing was
   * somewhere to render it.
   *
   * # Why one at a time, and after this pane's own
   *
   * `ApprovalCard`'s header explains that an unanswered card is a stalled session, which is why it
   * sits above the composer and cannot be scrolled past. Six of them stacked would bury the
   * composer — the same failure in a new place. So this is a queue with a count, exactly as a
   * single pane already treats its own approvals, and this pane's own question always outranks a
   * child's: it is the one whose transcript is on screen.
   */
  const delegated = $derived(blocking ? [] : sessions.delegatedApprovals(pane.session));
  const relayed = $derived(delegated[0] ?? null);

  /**
   * The plan a blocking approval is asking about, if that is what it is asking about.
   *
   * Derived from the approval rather than copied when the panel opens, so a plan that is superseded
   * — the session interrupted, the approval withdrawn — takes its panel with it instead of leaving
   * a document on screen with nothing left to decide about it.
   */
  const plan = $derived(blocking?.request.kind === 'plan_review' ? blocking.request : null);

  /**
   * Whether the plan panel is open, beside the transcript.
   *
   * Lifted out of `ApprovalCard`, which used to own it and render a modal. The panel is a sibling of
   * the whole pane column rather than something inside the card, so the card cannot own it: a
   * document you read to make a decision should not cover the decision, or the transcript that is
   * the context for it.
   */
  let reading = $state(false);

  // Nothing left to read. Without this the panel outlives the approval and its close button becomes
  // the only way back to a full-width transcript.
  $effect(() => {
    if (!plan) reading = false;
  });
  const side = $derived(sessions.sideFor(pane.id));
  /*
   * The provider's own figure, with no arithmetic on this side.
   *
   * There used to be a `tokensIn + cached` fallback for a zero `contextUsed`, and it was wrong for
   * whichever provider it was not written for: Claude reports those two disjointly, Codex's
   * `inputTokens` already contains `cachedInputTokens`, so the sum double counted there. A provider
   * that has not reported a footprint yet is a thing to say, not a thing to estimate.
   */
  const contextUsed = $derived(pane.usage?.contextUsed ?? 0);

  /**
   * How full the window is, or `null` when that cannot be said truthfully.
   *
   * `null` rather than a clamp, and the distinction is the whole reason this reads correctly now.
   * A ratio over 100% does not mean "completely full"; it means the numerator and the denominator
   * are not measuring the same thing — which is exactly what was happening, and clamping turned an
   * obviously-broken 340% into a believable "100%" that sat there for the life of the session. An
   * em dash is falsifiable; a plausible wrong number is not.
   */
  const contextPercent = $derived.by(() => {
    const window = pane.usage?.contextWindow ?? 0;
    if (window <= 0 || contextUsed <= 0) return null;
    const percent = Math.round((contextUsed / window) * 100);
    return percent > 100 ? null : percent;
  });

  /**
   * Whether this provider's allow can carry a rewritten payload.
   *
   * Claude Code's `control_response` takes an `updatedInput`; Codex refuses the answer rather than
   * running the original unedited. A property of the protocol, not of the machine, which is why it
   * is keyed off the provider here — it belongs on the capability query.
   */
  /**
   * Every agent that could run a turn in this pane, with what each can do.
   *
   * All of them, not just this pane's, because the model menu is grouped by provider — see
   * `ModelPicker`'s header. Filtered on `offered` as well as `available`: a repository that declines
   * an agent must not have it appear in a menu whose selection would be refused at spawn.
   *
   * The capabilities are already warm. `sessions.init` asks for every available provider's at
   * startup and `loadCapability` caches for the window's life, so reading more of that map than this
   * pane's own costs nothing — which is the whole reason the grouped menu is cheap.
   */
  const groups = $derived(
    sessions.options
      .filter((o) => o.available && o.offered)
      .map((o) => ({
        id: o.id,
        label: o.label,
        capability: sessions.capabilities[o.id] ?? null,
        failed: sessions.capabilityFailed[o.id] === true,
      })),
  );

  /**
   * Where a limited conversation could carry on: another agent, installed and offered here.
   *
   * Gated on both flags for the same reason the model menu is — `available` alone would offer a
   * hand-off whose spawn `open_agent_session` refuses on `offers_agent`, turning "continue on Codex"
   * into a config error. Null leaves the banner explaining the limit with nothing to click, which is
   * the honest outcome on a machine with one agent installed.
   */
  const continuation = $derived(
    sessions.options.find((o) => o.id !== provider && o.available && o.offered) ?? null,
  );

  /**
   * When the limit lifts, as a clock time, or null if the provider did not say.
   *
   * A fixed string, not a countdown: timers are banned on this side, and a wall-clock time is the
   * more useful form anyway — "resets around 3:05 PM" survives the window losing focus, where
   * "in 43 minutes" is wrong as soon as you look away.
   */
  const resetsAt = $derived.by(() => {
    const seconds = pane.limit?.resetsAt ?? null;
    if (seconds === null) return null;
    // Seconds on the wire, milliseconds in `Date`.
    return new Intl.DateTimeFormat(undefined, { timeStyle: 'short' }).format(
      seconds * 1000,
    );
  });

  /**
   * What a restart would apply, as a sentence — or null when it would apply nothing.
   *
   * Doubles as the flag for whether Restart is the thing to press: the two settings a running
   * session cannot be told about are the provider and the effort, so this is the only reason to
   * offer the control prominently rather than as a quiet icon. Spelled out rather than left as
   * "Restart to apply changes", because the two are worth distinguishing — switching agent throws
   * away a conversation that switching effort keeps.
   */
  const pending = $derived.by(() => {
    const target = pane.pendingProvider;
    const label =
      target === null ? null : (groups.find((g) => g.id === target)?.label ?? target);
    if (label !== null && pane.effortPending) {
      return `Restart to switch to ${label} at effort ${pane.effort}`;
    }
    if (label !== null) return `Restart to switch to ${label}`;
    if (pane.effortPending) return `Restart to apply effort ${pane.effort}`;
    return null;
  });

  async function restartConfirmed() {
    confirmRestart = false;
    await sessions.restart(pane.id);
  }

  async function closeConfirmed() {
    if (closingSession) return;
    closingSession = true;
    try {
      await sessions.close(pane.id);
      confirmClose = false;
    } finally {
      closingSession = false;
    }
  }

  /*
   * Follow the tail whenever the transcript changes height, for any reason.
   *
   * This watched `pane.events.length` for a long time, which meant it followed appends and nothing
   * else. Every `<details>` in a transcript — thinking, a tool's output, a folded run of work —
   * changes the height without adding an event, and none of them re-anchored. The browser does not
   * cover for it either: scroll anchoring is a `overflow-anchor` feature and WebKit does not
   * implement it, so on the platform this app actually ships to there was no fallback at all.
   *
   * A `ResizeObserver` on the content is the honest version of the same intent — "the document got
   * taller, keep up" — and it costs one observer instead of a dependency list that has to be kept
   * in step with every future thing that can change the layout. No feedback loop: setting
   * `scrollTop` does not change `scrollHeight`.
   */
  /*
   * Appends re-anchor even when nothing is being painted.
   *
   * Kept alongside the observer below rather than replaced by it, because a `ResizeObserver` is
   * delivered by the rendering lifecycle and a document that is not rendering has no lifecycle:
   * measured in a hidden tab, the observer fired **zero** times across a 250px growth. Panes here
   * are hidden with `display: none` whenever their worktree is not selected, and a window can be
   * minimised, so that is an ordinary state rather than an exotic one. Two lines to keep the
   * guarantee the observer cannot make on its own.
   */
  $effect(() => {
    void pane.events.length;
    if (scroller && pinned) scroller.scrollTop = scroller.scrollHeight;
  });

  $effect(() => {
    const el = content;
    const box = scroller;
    if (!el || !box) return;

    const observer = new ResizeObserver(() => {
      if (pinned) box.scrollTop = box.scrollHeight;
    });
    observer.observe(el);

    /*
     * Opening a disclosure stops the pane following the tail.
     *
     * Without this the observer above fights the user: you open a folded run to read it, the
     * content grows, and you are thrown to the bottom of the transcript — away from the thing you
     * just asked to see. Opening one is the clearest statement there is that you are reading rather
     * than watching. Closing one says nothing, so it leaves `pinned` alone.
     *
     * **`beforetoggle`, and that is the load-bearing part.** `toggle` is queued as a task and lands
     * *after* the resize observer has already run for the same layout change, so unpinning there is
     * one frame too late: measured, the view still jumped 550px to the tail before the flag was
     * cleared. `beforetoggle` fires synchronously ahead of the state change. `toggle` is kept as
     * well, so an engine that only has the older event still stops following — it just cannot undo
     * that first jump.
     *
     * Capture on both, because neither bubbles. They still pass every ancestor on the way down.
     */
    const unpinOnOpen = (event: Event) => {
      const target = event.target;
      if (!(target instanceof HTMLDetailsElement)) return;
      // `beforetoggle` reports the state it is moving to; by `toggle` the element already holds it.
      const state = (event as Event & { newState?: string }).newState;
      if (state === undefined ? target.open : state === 'open') pinned = false;
    };
    box.addEventListener('beforetoggle', unpinOnOpen, true);
    box.addEventListener('toggle', unpinOnOpen, true);

    return () => {
      observer.disconnect();
      box.removeEventListener('beforetoggle', unpinOnOpen, true);
      box.removeEventListener('toggle', unpinOnOpen, true);
    };
  });

  /*
   * Move focus in when someone asks, and at no other time.
   *
   * Tracking `focusEpoch` alone is the design. An effect that also tracked the selection would fire
   * on every arrow key in the sidebar, and focus would land in a session the user was navigating
   * past. Same mechanism the terminal dock used, and the same reason.
   */
  $effect(() => {
    if (sessions.focusEpoch === 0) return;
    if (sessions.focusTarget !== pane.id) return;
    if (provider === null) terminal?.focus();
    else composer?.focus();
  });

  let composer = $state<HTMLTextAreaElement | null>(null);
  /** The composer card. The drop target's bounds — see the drag-drop effect for why it is needed. */
  let form = $state<HTMLElement | null>(null);

  function onScroll() {
    if (!scroller) return;
    pinned = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 32;
  }

  /*
   * Grow the composer to fit what is in it.
   *
   * `_pane.scss` claimed this for a while without anything implementing it — `rows="2"` fixed the
   * height and the `max-height: 33%` beside the claim resolved against a content-height form, so it
   * did nothing. The bounds live in CSS; this only sets the height between them, and past the
   * maximum the textarea's own `overflow-y` takes over.
   *
   * `height: auto` first, because `scrollHeight` never shrinks below the height already set — without
   * the collapse the box would grow with a long paste and never come back down when it was deleted.
   */
  $effect(() => {
    void draft;
    const el = composer;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${el.scrollHeight}px`;
  });

  /**
   * What the microphone button says it will do.
   *
   * Names the destination in the tooltip rather than only in Settings. The consent step is where
   * agreement happens, but a control that sends audio to a third party should keep saying so where
   * it is pressed, not only where it was switched on.
   */
  const micTitle = $derived(
    dictation.transcribing
      ? 'Transcribing…'
      : dictation.recording
        ? 'Recording — release to transcribe'
        : dictation.mode === 'hold'
          ? `Hold to dictate — audio goes to ${DESTINATION}`
          : `Tap to dictate, tap again to stop — audio goes to ${DESTINATION}`,
  );

  let stopRequested = false;

  async function beginDictation() {
    stopRequested = false;
    await dictation.start();
    if (stopRequested) await endDictation();
  }

  /**
   * End a recording and put what was said in the draft.
   *
   * Appended rather than replacing, and separated by a space when there is already text: dictation
   * is routinely used to finish a sentence somebody started typing, and clobbering that would make
   * the button dangerous to press by accident.
   *
   * A press shorter than the start round trip used to no-op here (`recording` was still false)
   * and leave the microphone live once start resolved. `stopRequested` closes that window.
   */
  async function endDictation() {
    if (!dictation.recording) {
      stopRequested = true;
      return;
    }
    stopRequested = false;
    const said = await dictation.stop();
    if (said === null) {
      if (dictation.error !== null) sessions.error = dictation.error;
      return;
    }
    draft = draft.trim() === '' ? said : `${draft.replace(/\s+$/, '')} ${said}`;
    composer?.focus();
  }

  async function toggleDictation() {
    if (dictation.recording) await endDictation();
    else await beginDictation();
  }

  async function submit(event: Event) {
    event.preventDefault();
    const text = draft.trim();
    if ((!text && attachments.length === 0) || sending) return;

    if (attachments.length === 0 && text.startsWith('/')) {
      const [rawCommand] = text.split(/\s+/, 1);
      const command = rawCommand?.toLowerCase();
      if (command === '/clear' || command === '/new') {
        if (pane.working) {
          sessions.error = 'Stop the current turn before clearing this conversation.';
          return;
        }
        sending = true;
        await sessions.restart(pane.id);
        sending = false;
        draft = '';
        contextOpen = false;
        return;
      }
      if (command === '/context' || command === '/status') {
        contextOpen = !contextOpen;
        draft = '';
        return;
      }
      /*
       * Answered here rather than sent, because sending it does nothing.
       *
       * Anthropic's own SDK documentation says commands that need an interactive terminal are
       * excluded from a headless session's `slash_commands`, and `/skills` opens a picker — so it
       * reached the CLI and was treated as prose. wtm already holds the list this command exists to
       * show, from the init line, `skills/list` and the on-disk seed, so the honest thing is to
       * render it rather than to forward a request that cannot be answered.
       */
      if (command === '/skills') {
        skillsOpen = !skillsOpen;
        draft = '';
        return;
      }
      if (command === '/copy') {
        const copied = await copyLatestResponse();
        if (copied) {
          draft = '';
          copiedReply = true;
          clearTimeout(copyTimer);
          copyTimer = setTimeout(() => (copiedReply = false), 1400);
        }
        return;
      }
      /*
       * Answered here rather than sent, and this one used to fail loudly.
       *
       * `/fast` reaching the CLI came back "Fast mode is not available in the Agent SDK", because
       * the pill's own mechanism — the `fastMode` flag setting — *is* the opt-in that message asks
       * for, and the command that reports it is a terminal-UI command a headless session never
       * runs. So the honest thing is to drive the control the pane already has.
       *
       * The CLI's own argument grammar, so muscle memory transfers: bare toggles, `on` and `off`
       * are explicit. An unsupported provider says so instead of silently doing nothing, since the
       * pill is absent there and a no-op would look like a bug.
       */
      if (command === '/fast') {
        const argument = text
          .slice(rawCommand?.length ?? 0)
          .trim()
          .toLowerCase();
        if (argument !== '' && argument !== 'on' && argument !== 'off') {
          sessions.error = 'Use /fast, /fast on, or /fast off.';
          return;
        }
        const wanted = argument === '' ? !pane.fast : argument === 'on';
        // `setFast` rather than `configure`, because this caller knows about exactly one setting
        // and inventing the other three would mark the pane "on restart" for an effort change
        // nobody made. It reports the unsupported case rather than quietly doing nothing.
        if (!sessions.setFast(pane.id, wanted)) {
          sessions.error = 'This agent has no high-speed mode.';
          return;
        }
        draft = '';
        return;
      }
    }

    if (text.startsWith('/')) {
      const [rawCommand] = text.split(/\s+/, 1);
      const command = rawCommand?.toLowerCase();
      if (command === '/btw' || command === '/side') {
        const question = text.slice(rawCommand?.length ?? 0).trim();
        sending = true;
        const outgoing = attachments;
        const opened = await sessions.openSide(pane.id, question, outgoing);
        sending = false;
        if (opened && draft.trim() === text) draft = '';
        if (opened && attachments === outgoing) attachments = [];
        return;
      }
    }

    /*
     * The draft is held until the turn is accepted, not cleared on the way out.
     *
     * Clearing first destroyed the message whenever `send` could not deliver it, which was every
     * turn composed before the session id landed — and nothing said so, because the composer looked
     * exactly like one that had just sent successfully.
     */
    sending = true;
    const outgoing = attachments;
    const sent = await sessions.send(pane.id, text, outgoing);
    sending = false;
    // Only clear what actually went out. The wait can be seconds long on a pane that is still
    // starting, and anything typed during it is the next message rather than part of this one.
    if (sent && draft.trim() === text) draft = '';
    if (sent && attachments === outgoing) attachments = [];
  }

  /*
   * ⌘⏎ always sends. Whether a *bare* Enter also sends is `composerPrefs.sendKey`.
   *
   * The default is still ⌘⏎-only, because an agent prompt is routinely several lines — a stack
   * trace, a diff, a list of files — and a composer where Enter submits makes pasting one an
   * accident. But that is an argument about what a particular person pastes, not a universal one,
   * so it is a setting; `state/composer.svelte.ts` carries the full reasoning.
   *
   * Three orderings matter here, and all three are load-bearing:
   *
   * 1. ⌘⏎ is checked first and works in both modes, so the habit never breaks.
   * 2. The typeahead's claim on Enter is checked *before* Enter-to-send. It borrows Enter, Tab, the
   *    arrows and Escape, but **only while it is open** — which is why the guard is `query !== null`
   *    rather than a mode flag. Sending on the Enter that was meant to accept `@src/main.ts` would
   *    fire a half-written prompt.
   * 3. Shift+Enter is the newline in Enter mode, and it must be excluded *before* the send, not
   *    after — otherwise there is no way to write a second line at all.
   */
  function onKeydown(event: KeyboardEvent) {
    if ((event.metaKey || event.ctrlKey) && event.key === 'Enter') {
      void submit(event);
      return;
    }

    const typeaheadOpen = query !== null && suggestions.length > 0;
    if (
      composerPrefs.sendKey === 'enter' &&
      event.key === 'Enter' &&
      !event.shiftKey &&
      !event.altKey &&
      !event.isComposing &&
      !typeaheadOpen
    ) {
      // `isComposing` above is not decoration: an IME candidate is accepted with Enter, and
      // sending on it would submit a half-typed Japanese or Chinese prompt mid-word.
      void submit(event);
      return;
    }

    if (!typeaheadOpen) return;

    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      // Wraps, because a list this short is faster to reach from either end than to walk back
      // along. `+ length` keeps the modulo positive when going up from zero.
      const step = event.key === 'ArrowDown' ? 1 : -1;
      active = (active + step + suggestions.length) % suggestions.length;
    } else if (event.key === 'Enter' || event.key === 'Tab') {
      event.preventDefault();
      const chosen = suggestions[active];
      if (chosen) pick(chosen.value);
    } else if (event.key === 'Escape') {
      // Dismissed without touching the draft, so a `/` typed as literal text survives. The trigger
      // is still there, so the strip would reopen — `dismissed` is what remembers the refusal.
      event.preventDefault();
      dismissed = draft.slice(0, caret);
    }
  }

  // ── The `@` and `/` typeahead ───────────────────────────────────────────────────────────────
  //
  // The logic lives in `../suggest`, deliberately: there is no JS test runner here, so anything
  // reachable only by typing into a live textarea cannot be checked. What is left in this file is
  // the parts that genuinely need the DOM — where the caret is, and putting text back.

  /**
   * The caret, mirrored into state.
   *
   * `selectionStart` is not reactive, so the strip has to be recomputed from an explicit signal
   * rather than read during render. Updated on input, click and keyup — the three ways a caret
   * moves — because an arrow key that only moves the caret fires none of the other two.
   */
  let caret = $state(0);
  let active = $state(0);
  /**
   * The draft-prefix an Escape was pressed against.
   *
   * Escape has to mean "not this one" rather than "not ever", and the trigger character is still in
   * the text, so something has to distinguish the dismissed state from a fresh one. Comparing the
   * text before the caret does it: type one more character and this no longer matches, so the strip
   * comes back — which is right, because the query changed.
   */
  let dismissed = $state<string | null>(null);

  const query = $derived.by(() => {
    if (provider === null) return null;
    const found = queryAt(draft, caret);
    if (found === null || draft.slice(0, caret) === dismissed) return null;
    return found;
  });

  /**
   * Everything this session can be asked to do by name, discovered entries first.
   *
   * Derived once and shared by the `/` typeahead and the `/skills` panel, so the two cannot
   * disagree about what exists — which they would the first time one of them was given a filter the
   * other did not have.
   */
  const skills = $derived(commandsFor(provider ?? '', pane.skills));

  const suggestions = $derived.by(() => {
    if (query === null) return [];
    return query.kind === 'file'
      ? matchFiles(sessions.files[pane.worktreeId] ?? [], query.text)
      : matchSkills(skills, query.text);
  });

  // Back to the top whenever the offered set changes. Without this, narrowing a query from ten
  // matches to two leaves the selection on an index that no longer exists — or worse, on a
  // different row than the one that was highlighted a keystroke ago.
  $effect(() => {
    void suggestions;
    active = 0;
  });

  /**
   * Fill the `@` list the first time a pane needs it.
   *
   * On first `@` rather than on mount: this shells out to `git ls-files`, and a window with eight
   * panes open would otherwise run eight of them at startup for a list most sessions never use.
   */
  $effect(() => {
    if (query?.kind !== 'file') return;
    if (sessions.files[pane.worktreeId] !== undefined) return;
    void sessions.loadFiles(pane.worktreeId);
  });

  function noteCaret(event: Event) {
    caret = (event.currentTarget as HTMLTextAreaElement).selectionStart;
    // Any movement invalidates a previous Escape — see `dismissed`.
    if (dismissed !== null && draft.slice(0, caret) !== dismissed) dismissed = null;
  }

  function pick(value: string) {
    if (query === null) return;
    const next = accept(draft, query, value);
    draft = next.draft;
    caret = next.caret;
    dismissed = null;
    // The caret has to be moved on the element too, not only in our mirror of it: Svelte writes
    // `value`, which puts the real caret at the end of the whole draft rather than after what was
    // just inserted. Deferred one tick so it runs after that write.
    const el = composer;
    if (el)
      queueMicrotask(() => {
        el.focus();
        el.setSelectionRange(next.caret, next.caret);
      });
  }

  /**
   * Put paths into the draft, from the picker or from a drop.
   *
   * Appended at the end rather than at the caret, unlike an accepted suggestion. Both of these
   * arrive from *outside* the keyboard — a dialog that took focus, a drop that never had it — so
   * there is no meaningful caret to insert at, and guessing at the last one is how a path lands in
   * the middle of a word.
   */
  function addPaths(paths: string[]) {
    const mentions = paths.map((p) => `@${relativise(p, pane.worktreeId)}`).join(' ');
    if (mentions === '') return;
    const gap = draft === '' || draft.endsWith(' ') || draft.endsWith('\n') ? '' : ' ';
    draft = `${draft}${gap}${mentions} `;
    caret = draft.length;
    composer?.focus();
  }

  function addAttachment(attachment: AgentAttachment) {
    if (attachments.some((item) => item.path === attachment.path)) return;
    attachments = [...attachments, attachment];
    composer?.focus();
  }

  async function attachPaths(paths: string[]) {
    const mentions: string[] = [];
    for (const path of paths) {
      try {
        addAttachment(await commands.prepareAgentAttachment(path));
      } catch (error) {
        // A dropped directory cannot be attached as bytes, but it is still useful as an @ mention.
        // Other read failures remain visible globally while the path stays in the draft for the
        // user to remove or send deliberately.
        mentions.push(path);
        const message = errorMessage(error);
        if (!message.includes('is not a file')) sessions.error = message;
      }
    }
    addPaths(mentions);
  }

  function bytesToBase64(bytes: Uint8Array): string {
    let binary = '';
    const chunk = 0x8000;
    for (let at = 0; at < bytes.length; at += chunk) {
      binary += String.fromCharCode(...bytes.subarray(at, at + chunk));
    }
    return btoa(binary);
  }

  function formatBytes(size: number): string {
    if (size < 1024) return `${size} B`;
    if (size < 1024 * 1024) return `${Math.round(size / 1024)} KB`;
    return `${(size / (1024 * 1024)).toFixed(1)} MB`;
  }

  function formatTokens(tokens: number): string {
    if (tokens < 1000) return tokens.toLocaleString();
    if (tokens < 1_000_000) return `${(tokens / 1000).toFixed(tokens < 10_000 ? 1 : 0)}k`;
    return `${(tokens / 1_000_000).toFixed(1)}m`;
  }

  async function copyLatestResponse(): Promise<boolean> {
    let text = '';
    for (let index = pane.events.length - 1; index >= 0; index -= 1) {
      const event = pane.events[index];
      if (event?.kind === 'message') {
        text = event.text;
        break;
      }
      if (event?.kind === 'message_delta') {
        text = event.text + text;
        continue;
      }
      if (text !== '') break;
    }
    if (text === '') {
      sessions.error = 'There is no completed response to copy yet.';
      return false;
    }
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      sessions.error = 'The system clipboard was unavailable.';
      return false;
    }
  }

  async function onPaste(event: ClipboardEvent) {
    const files = Array.from(event.clipboardData?.files ?? []);
    if (files.length === 0) return;
    event.preventDefault();
    for (const file of files) {
      try {
        const bytes = new Uint8Array(await file.arrayBuffer());
        addAttachment(
          await commands.stageAgentAttachment(
            file.name || `pasted-${attachments.length + 1}`,
            file.type,
            bytesToBase64(bytes),
          ),
        );
      } catch (error) {
        sessions.error = errorMessage(error);
      }
    }
  }

  /**
   * The `+` button: the OS picker, rooted at the worktree.
   *
   * Covers what the typeahead cannot — directories, and anything outside the repository or inside
   * `.gitignore`. `dialog:allow-open` is already granted for the Browse… button in Add a
   * repository, so this needs no new permission and no new dependency.
   */
  function onAdd(event: Event) {
    const select = event.currentTarget as HTMLSelectElement;
    const choice = select.value;
    // Back to the sentinel immediately, so the control reads "Add…" again rather than keeping the
    // last action as if it were a setting. Reset before the await, because the dialog is modal and
    // the stale label would be on screen underneath it for as long as it is open.
    select.value = '';
    if (choice === '') return;
    void browse(choice === 'folders');
  }

  async function browse(directory: boolean) {
    const picked = await open({
      multiple: true,
      directory,
      defaultPath: pane.worktreeId,
      title: directory ? 'Add folders' : 'Add files',
    });
    if (picked === null) return;
    const paths = Array.isArray(picked) ? picked : [picked];
    if (directory) addPaths(paths);
    else await attachPaths(paths);
  }

  /**
   * Files dropped from Finder onto this pane's composer.
   *
   * Registered per pane and filtered by position, because Tauri's drag-drop is a **window** event —
   * there is one stream for every pane in the window, and each listener sees every drop. The
   * position is physical, so it is divided by the device pixel ratio before being handed to
   * `elementFromPoint`, which works in CSS pixels; skipping that puts the hit test at roughly twice
   * the intended coordinates on any Retina display, which is every Mac this ships to.
   *
   * The window-level handler is also why `dragDropEnabled` had to be turned on in `tauri.conf.json`,
   * and that has a real cost: it disables HTML5 drag events across the whole webview. The trade is
   * worth it — a webview drop event cannot see a real path, which is the only thing an agent can
   * use — but it is a whole-window change made for one control.
   */
  $effect(() => {
    if (provider === null) return;
    let stop: (() => void) | null = null;
    let gone = false;

    void getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type !== 'drop') return;
        const ratio = window.devicePixelRatio || 1;
        const at = document.elementFromPoint(
          event.payload.position.x / ratio,
          event.payload.position.y / ratio,
        );
        if (at === null || form === null || !form.contains(at)) return;
        void attachPaths(event.payload.paths);
      })
      .then((unlisten) => {
        // The subscription resolves asynchronously, so a pane closed in between would otherwise
        // leak a listener that outlives it — and every drop after that would append to a draft
        // nobody can see.
        if (gone) unlisten();
        else stop = unlisten;
      });

    return () => {
      gone = true;
      stop?.();
    };
  });
</script>

<!--
  Clicking anywhere in a pane makes it the split target for the next session, which is what an
  editor does. `noteFocus` rather than `focus`, so the click does not also re-trigger the focus
  effect and fight the caret the user just placed.
-->
<!--
  A click anywhere makes this the split target for the next session, which is what an editor does.
  There is no keyboard equivalent to add because `focusin` already covers it: tabbing into the pane
  is the keyboard way of doing the same thing, which is why the two handlers sit together.
-->
<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<section
  class="c-pane"
  class:is-focused={isFocused}
  aria-label="{label} session"
  onfocusin={() => sessions.noteFocus(pane.worktreeId, pane.id)}
  onclick={() => sessions.noteFocus(pane.worktreeId, pane.id)}
>
  <div class="c-pane__main">
    <header class="c-pane__head">
      <!--
      First, before the title, because it is what the pane is held by — and because a handle after the
      name reads as an action on the name rather than on the pane.

      A real `<button>`, so it is a tab stop and the arrow keys can move a pane without a pointer.
      Pointer events rather than HTML5 drag: `dragDropEnabled` in tauri.conf.json disables `dragstart`
      across the whole webview, in exchange for Finder drops carrying real paths. See the drag-drop
      effect above.
    -->
      <button
        class="c-pane__grip"
        type="button"
        aria-label="Move this session"
        aria-keyshortcuts="ArrowLeft ArrowRight ArrowUp ArrowDown Shift+ArrowLeft Shift+ArrowRight Shift+ArrowUp Shift+ArrowDown"
        title="Drag to move this pane, or use the arrow keys"
        onpointerdown={onmovestart}
        onkeydown={onmovekey}
      >
        <Icon name="grip" size={12} />
      </button>

      <h2 class="c-pane__title">{label}</h2>

      <!--
      One branch instead of four, and a dot beside the word.

      The word is what carries the state — `_semantic.scss` forbids colour alone, and here that rule
      is satisfied outright rather than worked around, which is why the dot is `aria-hidden`: it is
      the same fact drawn a second way, so naming it too would have a screen reader read the state
      twice. `idle` draws nothing at all, exactly as before.

      `attention` is new, and it is a correction: a pane blocked on an approval is still between
      `turn_started` and `turn_finished`, so this used to report it as "working…" while it was in fact
      doing nothing and waiting for an answer. See `statusOf`.
    -->
      {#if status !== 'idle' && note}
        <p class="c-pane__note {TONE[status]}">
          <SessionDot {status} />
          {note}
        </p>
      {/if}

      <!--
      Restart · Split · Close, in that order, and the order does not change with the pane's state.

      Restart used to appear *only* on a pane that had ended or failed, which left both "on restart"
      markers — effort, and now the provider — pointing at a control that did not exist. It also meant
      the header swapped Split for Restart when a session died, sliding Close left under whatever the
      pointer was already over.

      Prominence tracks whether anything is waiting on it: a quiet icon while it is merely available,
      a labelled button when it is the thing to press. Same register the row already used for Split
      against Restart-on-ended.
    -->
      <div class="c-pane__actions">
        <!--
        The way out of a delegated child, and it is here rather than only in the rail because this
        is where you are looking when you want it.

        `showRelated` reaches a child by *replacing* the tile it was clicked from, which is what
        keeps a twenty-agent run from tiling twenty panes — but it also means the session you came
        from is no longer on screen anywhere. The rail did offer a way back, as a chip carrying the
        provider's bare name, and that reads as a label rather than as a destination. It went
        unfound.

        First in the row, ahead of Restart, because it is the only one of these verbs that is
        navigation rather than an action on the session in front of you.
      -->
        {#if origin}
          <Button
            variant="quiet"
            size="sm"
            title="Back to {origin.label}, the session that started this one"
            onclick={() => sessions.showRelated(origin.id)}
          >
            ← {origin.label}
          </Button>
        {/if}

        {#if pane.ended || pane.error}
          <Button variant="neutral" size="sm" onclick={() => (confirmRestart = true)}>
            Restart
          </Button>
        {:else if pending !== null}
          <Button
            variant="neutral"
            size="sm"
            title={pending}
            onclick={() => (confirmRestart = true)}
          >
            Restart
          </Button>
        {:else}
          <Button
            variant="quiet"
            size="sm"
            icon="sm"
            title="Restart this session — the transcript is cleared and the conversation stays resumable"
            ariaLabel="Restart session"
            onclick={() => (confirmRestart = true)}
          >
            <Icon name="restart" size={12} />
          </Button>
        {/if}

        <!-- Shells get this too, and it is how a second terminal is opened. The guard used to require
           a provider, which was the last thing keeping a worktree to one shell once the backend
           allowed several. Both arms pass `pane.id` as the neighbour: the pane whose button was
           pressed, not whichever one the focus map thinks. See `Sessions.place` for why those
           differ on macOS. -->
        {#if !pane.ended && !pane.error}
          <Button
            variant="quiet"
            size="sm"
            icon="sm"
            title={provider === null
              ? 'Open another shell to the right'
              : 'Split this session to the right'}
            ariaLabel={provider === null ? 'New shell to the right' : 'Split right'}
            onclick={() =>
              void (provider === null
                ? sessions.openShell(pane.projectId, pane.worktreeId, 'right', pane.id)
                : sessions.openAgent(
                    pane.projectId,
                    pane.worktreeId,
                    provider,
                    'right',
                    pane.id,
                  ))}
          >
            <Icon name="split-right" size={13} />
          </Button>
        {/if}

        <Button
          variant="quiet"
          size="sm"
          icon="sm"
          title="End this session and close the pane"
          ariaLabel="Close session"
          onclick={() => (confirmClose = true)}
        >
          <Icon name="close" size={12} />
        </Button>
      </div>
    </header>

    {#if pane.detached && provider !== null}
      <!--
      Restored from the last run with nothing behind it yet.

      A card in the pane rather than a row in the surface's resume list, because the pane is the
      point: the split you built is back, and this is the one that used to hold this conversation.
      Resuming from here keeps its position; resuming from the list would open beside whatever had
      focus. See `sessions.reattach`.
    -->
      <div class="c-pane__body c-pane__body--offer">
        <div class="o-stack">
          <p class="c-pane__empty">{offer}</p>
          <div class="o-row">
            <Button
              variant="accent"
              size="sm"
              title="Resume this conversation in this pane"
              onclick={() => void sessions.reattach(pane.id)}
            >
              {pane.providerSession ? 'Resume' : `New ${label} session`}
            </Button>
            <Button
              variant="quiet"
              size="sm"
              title="Close this pane without resuming"
              onclick={() => void sessions.close(pane.id)}
            >
              Close
            </Button>
          </div>
          {#if pane.error}
            <Banner variant="error">{pane.error}</Banner>
          {/if}
        </div>
      </div>
    {:else if provider === null}
      <div class="c-pane__body c-pane__body--terminal">
        <Terminal
          bind:this={terminal}
          session={pane.session}
          active={visible && !pane.ended}
          onexit={() => {}}
        />
      </div>
    {:else}
      <div class="c-pane__body" bind:this={scroller} onscroll={onScroll}>
        <!-- The scroller reports its own fixed height, not its content's, so the thing the
           `ResizeObserver` has to watch is a wrapper inside it. -->
        <div bind:this={content}>
          {#if pane.events.length === 0 && pane.ready}
            <p class="c-pane__empty">Ask {label} something.</p>
          {/if}
          <AgentTranscript events={pane.events} />
        </div>
      </div>

      {#if blocking}
        <!-- Above the composer and outside the scroller: the CLI does not continue the turn until this
           is answered, so a card that could be scrolled away would stall the session silently. -->
        <ApprovalCard
          request={blocking.request}
          reading={reading && plan !== null}
          onread={() => (reading = !reading)}
          onanswer={(answer) =>
            void sessions.answerAndKeep(pane.id, blocking.id, answer, blocking.request)}
        />
      {/if}

      {#if relayed}
        <!-- A child's question, relayed. Captioned, because the card itself says nothing about
             whose turn is stalled and answering the wrong agent's `rm -rf` is not a recoverable
             misread. See `delegated` for why this is here at all. -->
        <div class="c-pane__relayed">
          <p class="c-pane__relayed-from">
            <SessionDot status={sessions.statusOfPane(relayed.pane)} />
            <strong>{relayed.pane.agentTitle ?? 'A delegated agent'}</strong> needs you
            {#if delegated.length > 1}
              <span class="c-pane__relayed-more">
                · {delegated.length - 1} more waiting
              </span>
            {/if}
            <Button
              variant="link"
              size="sm"
              title="Open this agent's own pane"
              onclick={() => sessions.showRelated(relayed.pane.id)}
            >
              Open it
            </Button>
          </p>
          <ApprovalCard
            request={relayed.approval.request}
            onanswer={(answer) =>
              void sessions.answerAndKeep(
                relayed.pane.id,
                relayed.approval.id,
                answer,
                relayed.approval.request,
              )}
          />
        </div>
      {/if}

      {#if side}
        <SideQuestion {side} />
      {/if}

      {#if pane.limit && !pane.ended}
        <!--
        Here rather than as a transcript row, for the reason the approval card is: the transcript
        scrolls, and an offer that has scrolled away is an offer nobody takes. The transcript gets a
        `notice` row for the same moment, which is the durable record — this is the decision.
      -->
        <div class="c-pane__limit">
          <Banner variant="warn">
            {pane.limit.message}{#if resetsAt}
              — resets around {resetsAt}{/if}
            {#snippet action()}
              {#if continuation}
                <Button
                  variant="neutral"
                  size="sm"
                  title="Open a {continuation.label} session here and carry this conversation into it"
                  onclick={() => void sessions.continueOn(pane.id, continuation.id)}
                >
                  Continue on {continuation.label}
                </Button>
              {/if}
              <Button
                variant="quiet"
                size="sm"
                title="Hide this. The session stays out of usage."
                onclick={() => sessions.dismissLimit(pane.id)}
              >
                Dismiss
              </Button>
            {/snippet}
          </Banner>
        </div>
      {/if}

      <!--
      One card holding the message, what will run it, and the control that sends it.

      These were two strips: a settings row floating above a hairline, then the form. Nothing said
      the model belonged to the message you were writing, so the row read as pane chrome that had
      come loose. Both desktop clients put all three inside one bordered field for that reason, and
      it is also what lets the whole thing take the focus ring as a unit.
    -->
      <div class="c-pane__foot">
        {#if skillsOpen}
          <section class="c-context" aria-label="Skills available in this session">
            <div class="c-context__head">
              <strong>Skills</strong>
              <span>{skills.length} available</span>
            </div>
            {#if skills.length === 0}
              <p class="c-pane__empty">
                Nothing discovered yet. {label} reports its own list once the session has been
                asked something.
              </p>
            {:else}
              <ul class="o-plain-list c-context__skills">
                {#each skills as skill (skill.name)}
                  <li class="o-row">
                    <button
                      class="c-row-action"
                      title="Put /{skill.name} in the message"
                      onclick={() => {
                        draft = `/${skill.name} `;
                        skillsOpen = false;
                      }}
                    >
                      /{skill.name}
                    </button>
                    <span class="c-status--subtle">{skill.description ?? ''}</span>
                  </li>
                {/each}
              </ul>
            {/if}
          </section>
        {/if}
        {#if contextOpen}
          <section class="c-context" aria-label="Session context usage">
            <div class="c-context__head">
              <strong>Context window</strong>
              <!--
              Three states, not two. A pane with a footprint but no window to compare it against
              can still say something true, and "Waiting for usage" would have been a lie about a
              number that is sitting right there.
            -->
              <span
                >{contextPercent !== null
                  ? `${contextPercent}% used`
                  : contextUsed > 0
                    ? `${formatTokens(contextUsed)} in context`
                    : 'Waiting for usage'}</span
              >
            </div>
            <div
              class="c-context__track"
              role="progressbar"
              aria-label="Context window used"
              aria-valuemin="0"
              aria-valuemax="100"
              aria-valuenow={contextPercent ?? 0}
            >
              <span style:width="{contextPercent ?? 0}%"></span>
            </div>
            {#if pane.usage}
              <dl class="c-context__facts">
                <div>
                  <dt>In context</dt>
                  <dd>{formatTokens(contextUsed)}</dd>
                </div>
                <div>
                  <dt>Window</dt>
                  <dd>
                    {pane.usage.contextWindow
                      ? formatTokens(pane.usage.contextWindow)
                      : '—'}
                  </dd>
                </div>
                <div>
                  <dt>Input</dt>
                  <dd>{formatTokens(pane.usage.tokensIn)}</dd>
                </div>
                <div>
                  <dt>Cached</dt>
                  <dd>{formatTokens(pane.usage.cached)}</dd>
                </div>
                <div>
                  <dt>Output</dt>
                  <dd>{formatTokens(pane.usage.tokensOut)}</dd>
                </div>
              </dl>
              <!--
              Said out loud because the two halves of this card count different things, and a reader
              comparing them is right to be confused: the top row is the last request's prompt, the
              bottom three are everything the turn billed across all of its round trips. They do not
              add up to each other and are not supposed to.
            -->
              <p>Input, cached and output are totals for the last turn.</p>
            {:else}
              <p>Usage appears after the provider reports its first token update.</p>
            {/if}
          </section>
        {/if}

        <form class="c-composer" bind:this={form} onsubmit={(event) => void submit(event)}>
          {#if suggestions.length > 0}
            <!-- First child, so the card grows *upward* around it and the message stays where the
               caret already is. In the flow rather than floating: see the component's own header
               for why that is the design and not a limitation. -->
            <Suggest
              id="suggest-{pane.id}"
              items={suggestions}
              {active}
              onpick={(value) => pick(value)}
            />
          {/if}

          {#if attachments.length > 0}
            <div class="c-composer__attachments" aria-label="Attachments">
              {#each attachments as attachment (attachment.path)}
                <figure class="c-attachment">
                  {#if attachment.mime.startsWith('image/')}
                    <img
                      src="data:{attachment.mime};base64,{attachment.dataBase64}"
                      alt="Preview of {attachment.name}"
                    />
                  {:else}
                    <span class="c-attachment__file" aria-hidden="true">
                      <Icon name="file" size={16} />
                    </span>
                  {/if}
                  <button
                    type="button"
                    aria-label="Remove {attachment.name}"
                    title="Remove attachment"
                    onclick={() =>
                      (attachments = attachments.filter(
                        (item) => item.path !== attachment.path,
                      ))}
                  >
                    <Icon name="close" size={10} />
                  </button>
                  <figcaption>
                    <span class="c-attachment__name" title={attachment.name}
                      >{attachment.name}</span
                    >
                    <small class="c-attachment__size">{formatBytes(attachment.size)}</small>
                  </figcaption>
                </figure>
              {/each}
            </div>
          {/if}

          <!-- Deliberately NOT `.c-textarea`. That block is a bordered, filled form control, and its
             partial sorts after this one in `main.scss` — so at equal specificity it won, and the
             card ended up with a second bordered box and a resize grip inside it. The two
             declarations actually wanted from it are restated in `.c-composer__input`; see there. -->
          <textarea
            class="c-composer__input"
            bind:this={composer}
            placeholder="Ask {label}… — paste or @ files, / for commands"
            aria-label="Message {label}"
            bind:value={draft}
            oninput={noteCaret}
            onclick={noteCaret}
            onkeyup={noteCaret}
            onkeydown={onKeydown}
            onpaste={onPaste}
            role="combobox"
            aria-expanded={suggestions.length > 0}
            aria-controls="suggest-{pane.id}"
            aria-activedescendant={suggestions.length > 0
              ? `suggest-${pane.id}-${active}`
              : undefined}
            disabled={pane.ended !== null || pane.error !== null}></textarea>

          <div class="c-composer__bar">
            <!--
            Files first, then what will run them, then Send: the row reads left to right in the
            order a turn is assembled. Icon-only, because the pills beside it are already three
            words wide on a quarter-width pane and a `+` next to a message box is not ambiguous.

            A menu rather than a plain button, because macOS separates the two dialogs: one picks
            files, another picks directories, and there is no "either" mode to fall back on. So the
            second entry is not a shortcut for the first — it is the only route to the other one,
            and hiding it behind a right-click would make folders undiscoverable.

            `o-overlay-select` for the same reason the model pills use it: the native popup renders
            outside the stacking context, so this needs no third z-index. Fifth use of the idiom.
          -->
            <span class="c-composer__add o-overlay-select">
              <span class="c-composer__addmark" aria-hidden="true">
                <Icon name="plus" size={14} />
              </span>
              <!-- The sentinel is selectable and does nothing rather than `disabled`, because a
                 disabled option cannot reliably be re-selected programmatically — and this control
                 has to return to it after every use. Same note as `OpenInButton`. -->
              <select
                class="o-overlay-select__native"
                aria-label="Add files or folders"
                value=""
                disabled={pane.ended !== null || pane.error !== null}
                title="Add files or folders, or drop them here"
                onchange={onAdd}
              >
                <option value="">Add…</option>
                <option value="files">Files…</option>
                <option value="folders">Folders…</option>
              </select>
            </span>

            {#if dictation.available}
              <!--
              Hold-to-talk, or tap-to-toggle, by preference.

              `onpointerdown`/`onpointerup` rather than click, because hold mode needs the two ends
              of the press separately — and `onpointerleave` ends a recording whose button the
              pointer has wandered off, which is otherwise a hot microphone with no visible control
              to stop it.

              The transcript lands in the draft and is never sent. A mistranscription that sent
              itself is not recoverable, and this feature's whole failure mode is getting a word
              wrong.
            -->
              <button
                class="c-composer__mic"
                class:is-recording={dictation.recording}
                type="button"
                disabled={pane.ended !== null ||
                  pane.error !== null ||
                  dictation.transcribing}
                aria-pressed={dictation.recording}
                aria-label={dictation.recording ? 'Stop dictating' : 'Dictate'}
                title={micTitle}
                onpointerdown={dictation.mode === 'hold' ? beginDictation : undefined}
                onpointerup={dictation.mode === 'hold' ? endDictation : undefined}
                onpointerleave={dictation.mode === 'hold' ? endDictation : undefined}
                onclick={dictation.mode === 'tap' ? toggleDictation : undefined}
              >
                <Icon name="mic" size={14} />
                {#if dictation.recording || dictation.transcribing}
                  <span class="c-composer__mic-note">
                    {dictation.transcribing ? 'transcribing…' : 'listening…'}
                  </span>
                {/if}
              </button>
            {/if}

            <button
              class="c-composer__context"
              class:is-open={contextOpen}
              type="button"
              aria-expanded={contextOpen}
              title="Show context-window usage"
              onclick={() => (contextOpen = !contextOpen)}
            >
              <span class="c-composer__context-ring" style:--context={contextPercent ?? 0}
              ></span>
              <span>Context {contextPercent === null ? '—' : `${contextPercent}%`}</span>
            </button>

            <ModelPicker
              providers={groups}
              provider={provider ?? ''}
              pendingProvider={pane.pendingProvider}
              model={pane.model}
              effort={pane.effort}
              mode={pane.mode}
              fast={pane.fast}
              effortPending={pane.effortPending}
              disabled={pane.ended !== null || pane.error !== null}
              onchange={(next) => sessions.configure(pane.id, next)}
              onretry={(id) => void sessions.retryCapability(id)}
            />

            <div class="c-composer__send">
              <!--
              `pane.working`, deliberately **not** `status === 'working'`.

              This is the one place that wants the raw field. `attention` outranks `working` in the
              status vocabulary — correctly, because a blocked pane is waiting on you rather than
              thinking — but a turn *is* still in flight while an approval is unanswered, so keying the
              control off the status would flip it from Stop back to Send mid-turn, offering to send a
              second message into a session that cannot take one.
            -->
              {#if pane.working}
                <Button
                  variant="neutral"
                  size="sm"
                  onclick={() => void sessions.interrupt(pane.id)}
                >
                  Stop
                </Button>
              {:else}
                <!-- The shortcut lives here rather than in the placeholder, where it was competing
                   with the prompt for the one line of text a user reads before typing. It follows
                   the setting because a hint that names the wrong key is worse than no hint. -->
                <span class="c-composer__hint" aria-hidden="true">
                  {composerPrefs.sendKey === 'enter' ? '↵' : '⌘↵'}
                </span>
                <Button
                  variant="accent"
                  size="sm"
                  type="submit"
                  disabled={(draft.trim().length === 0 && attachments.length === 0) ||
                    pane.ended !== null ||
                    sending}
                >
                  {sending ? 'Sending…' : 'Send'}
                </Button>
              {/if}
              {#if copiedReply}
                <span class="c-status--ok">Copied the last reply.</span>
              {/if}
            </div>
          </div>
        </form>
      </div>
    {/if}
  </div>

  {#if reading && plan}
    <!--
      The plan, beside the session rather than over it.

      It was a modal: a scrim, a focus trap, and the approval card it belongs to hidden behind it.
      So reading a plan meant open, read, close, find the card again, and remember what you decided.
      As a sibling of `c-pane__main` the document and the decision are on screen together, the
      transcript that is the context for both stays scrollable, and Approve is one click from
      wherever you got to in the plan.
      -->
    <aside class="c-pane__aside" aria-label="Proposed plan">
      <header class="c-pane__aside-head">
        <strong>Proposed plan</strong>
        <Button
          variant="quiet"
          size="sm"
          title="Close the plan. The card below still has the decision."
          onclick={() => (reading = false)}
        >
          <Icon name="close" size={12} />
        </Button>
      </header>
      <div class="c-pane__aside-body">
        <div class="c-planview">
          <Markdown source={plan.markdown} />
        </div>
        {#if plan.path}
          <p class="c-planview__meta"><code>{plan.path}</code></p>
        {/if}
      </div>
    </aside>
  {/if}
</section>

{#if confirmRestart}
  <Dialog title="Restart session?" onclose={() => (confirmRestart = false)}>
    {#snippet body()}
      <p>
        Restarting clears this pane's visible {provider === null
          ? 'terminal scrollback'
          : 'chat'}
        {#if provider !== null}
          . The provider conversation will remain available under “Pick up where you left
          off”
        {/if}.
      </p>
      {#if pane.working}
        <p class="c-status--warn">The current turn will be stopped.</p>
      {/if}
      {#if pending !== null}<p class="c-note">{pending}.</p>{/if}
    {/snippet}
    {#snippet footer()}
      <Button variant="neutral" onclick={() => (confirmRestart = false)}>Cancel</Button>
      <Button variant="danger-solid" onclick={() => void restartConfirmed()}>Restart</Button
      >
    {/snippet}
  </Dialog>
{/if}

{#if confirmClose}
  <Dialog
    title="Close session?"
    onclose={() => (confirmClose = false)}
    closeDisabled={closingSession}
  >
    {#snippet body()}
      <p>
        Closing ends this {provider === null ? 'shell' : 'conversation'} and removes the pane.
        {#if provider !== null}
          Delegated children and any side question are closed with it.
        {/if}
      </p>
    {/snippet}
    {#snippet footer()}
      <Button
        variant="neutral"
        onclick={() => (confirmClose = false)}
        disabled={closingSession}>Cancel</Button
      >
      <Button
        variant="danger-solid"
        onclick={() => void closeConfirmed()}
        disabled={closingSession}>{closingSession ? 'Closing…' : 'Close'}</Button
      >
    {/snippet}
  </Dialog>
{/if}
