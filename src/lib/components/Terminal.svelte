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
  import xtermCss from '@xterm/xterm/css/xterm.css?inline';
  import { listen } from '@tauri-apps/api/event';

  import { commands } from '../ipc/commands';
  import type { PtyExit, PtyOutput } from '../ipc/types';
  import { theme } from '../state/theme.svelte';

  const {
    session,
    onexit,
  }: {
    /** Null until the caller learns the id — see the note above. */
    session: string | null;
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

  /** Deliberately not `$state`: the flush effect writes it, and reading it must not re-trigger. */
  let attachedTo: string | null = null;
  let term: Terminal | null = null;
  let ready = $state(false);

  /** Read the app's own tokens so the terminal matches the window rather than fighting it. */
  function paletteFrom(root: HTMLElement) {
    const read = (name: string, fallback: string) =>
      getComputedStyle(root).getPropertyValue(name).trim() || fallback;
    return {
      background: read('--bg-code', '#171614'),
      foreground: read('--fg', '#f5f4f2'),
      cursor: read('--accent', '#d97757'),
      selectionBackground: read('--bg-active', '#3d3a37'),
    };
  }

  $effect(() => {
    if (!host) return;

    // xterm ships its own stylesheet. Inlined and injected once, because the CSP forbids
    // loading anything from a remote origin and a bundled <link> would be a second request.
    const styleId = 'xterm-css';
    if (!document.getElementById(styleId)) {
      const style = document.createElement('style');
      style.id = styleId;
      style.textContent = xtermCss;
      document.head.append(style);
    }

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

    const fit = new FitAddon();
    created.loadAddon(fit);
    created.open(host);
    fit.fit();
    term = created;
    ready = true;

    // Keystrokes back to the child.
    const input = created.onData((data) => {
      if (!session) return;
      const bytes = new TextEncoder().encode(data);
      void commands.ptyWrite(session, bytesToBase64(bytes)).catch(() => {
        /* The session ended; xterm keeps the transcript. */
      });
    });

    const resize = new ResizeObserver(() => {
      try {
        fit.fit();
        if (session)
          void commands.ptyResize(session, created.rows, created.cols).catch(() => {});
      } catch {
        /* The pane was hidden mid-measure. */
      }
    });
    resize.observe(host);

    const unlistenOutput = listen<PtyOutput>('pty:output', (event) => {
      const bytes = base64ToBytes(event.payload.chunkBase64);

      // Buffer until we both know our session and have flushed its backlog. Writing straight
      // through before the flush would put late chunks ahead of early ones.
      if (session === null || attachedTo !== session) {
        if (session !== null && event.payload.session !== session) return;
        buffer(event.payload.session, bytes);
        return;
      }

      if (event.payload.session !== session) return;
      created.write(bytes);
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
      ready = false;
      attachedTo = null;
      pending.clear();
    };
  });

  function buffer(id: string, bytes: Uint8Array) {
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

  // Re-theme in place when the window theme changes, rather than tearing the terminal down
  // and losing the transcript.
  $effect(() => {
    void theme.resolved;
    if (!ready || !term) return;
    term.options.theme = paletteFrom(document.documentElement);
  });
</script>

<div class="wrap">
  <div class="term" bind:this={host}></div>
  {#if status}
    <p class="status">{status}</p>
  {/if}
</div>

<style>
  .wrap {
    display: flex;
    flex-direction: column;
    min-height: 0;
    height: 100%;
    gap: var(--sp-2);
  }

  .term {
    flex: 1 1 auto;
    min-height: 180px;
    padding: var(--sp-2);
    background: var(--bg-code);
    border: 1px solid var(--border);
    border-radius: var(--r-md);
    overflow: hidden;
  }

  .status {
    flex: 0 0 auto;
    font-size: var(--step--2);
    color: var(--fg-muted);
    font-family: var(--font-mono);
  }
</style>
