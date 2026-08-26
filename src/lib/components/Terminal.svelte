<script lang="ts">
  /**
   * A live terminal attached to a PTY session.
   *
   * Two details matter and both come from the same fact — the backend streams *bytes*:
   *
   * 1. Chunks arrive base64-encoded and are written to xterm as a `Uint8Array`, never decoded
   *    to a string here. Terminal output is not guaranteed to split on UTF-8 boundaries, and
   *    an escape sequence can straddle two reads; xterm reassembles both correctly, a
   *    `TextDecoder` per chunk would not.
   * 2. Input goes back the same way, so a prompt is genuinely answerable — which is the whole
   *    reason setup runs under a pty rather than a captured pipe.
   *
   * The terminal is created in an `$effect` with a teardown, so Svelte's lifecycle owns the
   * imperative object rather than the other way round.
   *
   * # Why `session` may be null
   *
   * A session id is minted by the spawn, so a caller that is *about to* start one cannot know
   * it yet. Mounting only once the id is known loses the first output — and for `create`, where
   * the id historically arrived with the return value, it lost the entire run. So this
   * component attaches first and learns its session second: while `session` is null it buffers
   * output for **every** session, and flushes the matching one the moment it is told which is
   * its own. That makes the attach race unlosable rather than merely unlikely.
   */
  import { FitAddon } from '@xterm/addon-fit';
  import { Terminal } from '@xterm/xterm';
  import { listen } from '@tauri-apps/api/event';

  import { commands } from '../ipc/commands';
  import type { PtyExit, PtyOutput } from '../ipc/types';
  import { theme } from '../state/theme.svelte';

  const {
    session,
    active = true,
    onexit,
  }: {
    /** Null until the caller learns the id — see the note above. */
    session: string | null;
    /**
     * False while the pane is mounted but hidden.
     *
     * That is how the terminal dock keeps a shell's scrollback across a worktree switch: the
     * transcript lives in this component, so unmounting throws it away, and every pane but the
     * active one is `display: none` instead. Defaults to true — a terminal that is on screen
     * for its whole life, like the create pane's or the remove dialog's, has nothing to
     * declare.
     */
    active?: boolean;
    onexit?: (exit: PtyExit) => void;
  } = $props();

  let host = $state<HTMLDivElement | null>(null);
  let status = $state<string | null>(null);

  /**
   * Output held while the session is unknown, keyed by session.
   *
   * Bounded: a project's setup can print a great deal, and buffering an unbounded amount of a
   * session that may not even be ours would be a leak. Overflow drops the oldest chunks, which
   * degrades to "the transcript starts a little late" rather than to unbounded memory.
   */
  const pending = new Map<string, Uint8Array[]>();
  const MAX_PENDING_CHUNKS = 2048;
  const MAX_PENDING_SESSIONS = 40;

  /** Deliberately not `$state`: the flush effect writes it, and reading it must not re-trigger. */
  let attachedTo: string | null = null;
  let term: Terminal | null = null;
  /** Hoisted out of the creation effect so `refit` can reach it. Nothing renders it. */
  let fit: FitAddon | null = null;
  /** The last size the child was told, so an observer fire that changed nothing sends nothing. */
  let sent = { rows: 0, cols: 0 };
  let ready = $state(false);

  /**
   * Read the app's own tokens so the terminal matches the window rather than fighting it.
   *
   * The fallbacks are the only colour literals outside `src/styles/settings`, and they are
   * dark-theme values — a light-mode window whose computed read came back empty would get a
   * dark terminal. That is the right trade: the read has never failed, and a terminal is the
   * one surface where dark is a defensible default. They do have to be updated by hand when
   * the primitives move, which is the cost of having them at all.
   */
  function paletteFrom(root: HTMLElement) {
    const read = (name: string, fallback: string) =>
      getComputedStyle(root).getPropertyValue(name).trim() || fallback;
    return {
      background: read('--bg-code', '#080d0b'),
      foreground: read('--fg', '#f0f5f2'),
      cursor: read('--accent', '#3fb27a'),
      selectionBackground: read('--bg-active', '#2a3831'),
    };
  }

  /**
   * Re-measure, and tell the child, unless the pane has no box or the grid did not move.
   *
   * The zero guard is the whole point of this existing. A hidden pane's `ResizeObserver` fires
   * at 0×0, and the fit addon's `proposeDimensions` floors its answer at two columns by one row
   * rather than declining — so an unguarded fit informs a live shell that its window is 2×1, and
   * the shell reflows its prompt to match. The one case that saves itself is `display: none`,
   * where the parent's computed height reads `auto`, `parseInt` gives `NaN`, and the addon's own
   * `isNaN` check catches it. That is luck rather than design, and a height dragged toward zero
   * is not covered by it. The two panes that predate the dock are never hidden, which is the
   * only reason this has not bitten yet.
   *
   * Skipping the resize when the grid is unchanged matters because the dock's height is
   * *dragged*: unconditional would be one SIGWINCH per pointermove for a row count that only
   * changes every fourteen pixels, and a shell redraws on each one.
   *
   * # Never call this synchronously from the creation effect
   *
   * It reads `session`, which is a `$props()` getter — so a synchronous call would make that
   * effect depend on `session` and tear the terminal down the instant the id arrived, losing the
   * transcript on every single open. Deferred callbacks (the observer, the effects below) are
   * outside Svelte's tracking window and are fine.
   */
  function refit(): void {
    if (!host || !term || !fit) return;
    if (host.clientWidth === 0 || host.clientHeight === 0) return;

    try {
      fit.fit();
    } catch {
      /* The pane was hidden mid-measure. */
      return;
    }

    if (!session) return;
    if (term.rows === sent.rows && term.cols === sent.cols) return;
    sent = { rows: term.rows, cols: term.cols };
    void commands.ptyResize(session, term.rows, term.cols).catch(() => {});
  }

  /**
   * Put the caret in the terminal.
   *
   * The only export this component has, and the only imperative thing a parent needs from it:
   * the dock has to focus the pane it just revealed, and there is no way to express that as a
   * prop which is not a counter in disguise. Safe to call before the terminal exists — a focus
   * request and a pane's mount are not ordered against each other.
   */
  export function focus(): void {
    term?.focus();
  }

  $effect(() => {
    if (!host) return;

    const created = new Terminal({
      // Collapsed to one line before handing it over: `--font-mono` is declared across
      // two source lines, so the computed value carries a newline and the indentation
      // with it. xterm uses this string to measure a character cell and as a cache key,
      // neither of which wants embedded whitespace.
      fontFamily: getComputedStyle(document.documentElement)
        .getPropertyValue('--font-mono')
        .replace(/\s+/g, ' ')
        .trim(),
      fontSize: 12,
      // The DOM renderer, deliberately: the WebGL addon is incompatible with xterm 6 and the
      // canvas renderer was removed. A setup log does not need GPU acceleration.
      theme: paletteFrom(document.documentElement),
      cursorBlink: false,
      convertEol: true,
      scrollback: 5000,
    });

    const addon = new FitAddon();
    created.loadAddon(addon);
    created.open(host);
    // `addon.fit()` rather than `refit()`, because `refit` reads `session` — see its note.
    addon.fit();
    term = created;
    fit = addon;
    ready = true;

    // Keystrokes back to the child.
    const input = created.onData((data) => {
      if (!session) return;
      const bytes = new TextEncoder().encode(data);
      void commands.ptyWrite(session, bytesToBase64(bytes)).catch(() => {
        /* The session ended; xterm keeps the transcript. */
      });
    });

    const resize = new ResizeObserver(() => refit());
    resize.observe(host);

    const unlistenOutput = listen<PtyOutput>('pty:output', (event) => {
      const incoming = event.payload.session;
      // Filter before decoding: every pane hears every session's chunks, and base64 is the
      // expensive half of the hottest path in the app.
      if (session !== null && attachedTo === session) {
        if (incoming !== session) return;
        created.write(base64ToBytes(event.payload.chunkBase64));
        return;
      }
      if (session !== null && incoming !== session) return;
      buffer(incoming, base64ToBytes(event.payload.chunkBase64));
    });

    const unlistenExit = listen<PtyExit>('pty:exit', (event) => {
      if (event.payload.session !== session) return;
      status = event.payload.summary;
      onexit?.(event.payload);
    });

    return () => {
      input.dispose();
      resize.disconnect();
      void unlistenOutput.then((off) => off());
      void unlistenExit.then((off) => off());
      created.dispose();
      term = null;
      fit = null;
      sent = { rows: 0, cols: 0 };
      ready = false;
      attachedTo = null;
      pending.clear();
    };
  });

  function buffer(id: string, bytes: Uint8Array) {
    if (!pending.has(id) && pending.size >= MAX_PENDING_SESSIONS) {
      const first = pending.keys().next().value;
      if (first !== undefined) pending.delete(first);
    }
    const chunks = pending.get(id) ?? [];
    chunks.push(bytes);
    if (chunks.length > MAX_PENDING_CHUNKS)
      chunks.splice(0, chunks.length - MAX_PENDING_CHUNKS);
    pending.set(id, chunks);
  }

  /** Once the session is known, drain its backlog and drop everyone else's. */
  $effect(() => {
    const id = session;
    if (!ready || !term || id === null || attachedTo === id) return;

    for (const chunk of pending.get(id) ?? []) term.write(chunk);
    pending.clear();
    attachedTo = id;
    // Now that there is somewhere to send it, tell the child how big the pane really is.
    //
    // Every caller spawns with a guessed 24×100, and the observer's first fire happened before
    // there was an id to send it to — so without this the child kept the guess for its whole
    // life and wrapped its output at the wrong column. That was true of the create pane and the
    // remove dialog too, long before the dock existed.
    refit();
  });

  function bytesToBase64(bytes: Uint8Array): string {
    let binary = '';
    for (const byte of bytes) binary += String.fromCharCode(byte);
    return btoa(binary);
  }

  function base64ToBytes(text: string): Uint8Array {
    const binary = atob(text);
    const out = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i);
    return out;
  }

  /*
   * Re-fit when a hidden pane comes back.
   *
   * Every fit that fired while it was hidden did nothing, and the dock's height may have moved
   * since — so a revealed pane has to measure itself again. Reading `host.clientWidth` inside
   * `refit` forces a synchronous layout, which is what makes it see the just-un-hidden box
   * rather than the stale one.
   *
   * Tracks `active`, and through `refit` also `session` — harmless precisely because `refit` is
   * idempotent and sends nothing when the grid has not changed. What this must never become is
   * part of the creation effect above; see `refit`.
   */
  $effect(() => {
    if (!active || !ready) return;
    refit();
  });

  // Re-theme in place when the window theme changes, rather than tearing the terminal down
  // and losing the transcript.
  $effect(() => {
    void theme.resolved;
    if (!ready || !term) return;
    term.options.theme = paletteFrom(document.documentElement);
  });
</script>

<div class="c-terminal">
  <div
    class="c-terminal__screen"
    bind:this={host}
    role="log"
    aria-label="Terminal output"
  ></div>
  {#if status}
    <p class="c-terminal__status" aria-live="polite">{status}</p>
  {/if}
</div>
