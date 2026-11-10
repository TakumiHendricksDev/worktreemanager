<script lang="ts">
  /**
   * The text-event viewer for a plugin's session backend (API 1): list, attach, take control,
   * send, detach.
   *
   * Every answer is checked for shape before it is shown, because it comes from package code.
   * Detaching never stops anything: the session belongs to the backend's host, not to this view.
   * Detach goes back to the list; closing the dialog detaches too, once.
   *
   * # Why this polls, and how little
   *
   * ARCHITECTURE §8 bans polling, and this is an exception the backend protocol forces: a backend
   * is a helper process that answers one JSON-RPC request and exits, so there is no stream to
   * listen to, and every poll spawns that helper. So it polls as little as it can. The first wait
   * is a second, and each answer with nothing new doubles it up to `SLOWEST`; anything new, a
   * message sent or control taken starts again from a second. Nothing is asked while the window
   * is hidden or in the background, and coming back asks at once.
   */
  import { onDestroy, onMount, tick } from 'svelte';
  import { commands } from '../ipc/commands';
  import { errorMessage } from '../ipc/types';
  import Dialog from './ui/Dialog.svelte';
  import Button from './ui/Button.svelte';
  import Banner from './ui/Banner.svelte';
  import TextInput from './ui/TextInput.svelte';

  type Remote = { id: string; title: string; host: string };
  type Update = {
    reset?: boolean;
    cursor: number;
    events: { seq: number; text: string }[];
    lease?: string | null;
  };
  const {
    plugin,
    backend,
    title,
    onclose,
  }: { plugin: string; backend: string; title: string; onclose: () => void } = $props();

  const FASTEST = 1000;
  const SLOWEST = 15_000;
  const client = crypto.randomUUID();
  let list = $state<Remote[]>([]);
  let active = $state<Remote | null>(null);
  let cursor = $state(0);
  let transcript = $state('');
  let lease = $state<string | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let connected = $state(false);
  let message = $state('');
  let output = $state<HTMLElement | null>(null);
  /** Whether the reader is at the end of the output, so new output may scroll it. */
  let following = true;
  let gone = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  /** A poll is waiting on the helper, so coming back to the window does not start a second. */
  let polling = false;
  let wait = FASTEST;

  async function rpc(method: string, params: object = {}) {
    return commands.pluginSession(plugin, backend, method, { ...params, client });
  }

  /** Apply an event snapshot; true when it brought anything new. */
  function apply(value: unknown): boolean {
    const update = value as Update;
    if (
      !Number.isSafeInteger(update?.cursor) ||
      !Array.isArray(update.events) ||
      !update.events.every((e) => Number.isSafeInteger(e.seq) && typeof e.text === 'string')
    )
      throw new Error('Backend returned an invalid event snapshot.');
    let fresh = !!update.reset;
    if (update.reset) {
      cursor = 0;
      transcript = '';
    }
    for (const e of update.events)
      if (e.seq > cursor) {
        transcript = (transcript + e.text + '\n').slice(-100_000);
        cursor = e.seq;
        fresh = true;
      }
    cursor = Math.max(cursor, update.cursor);
    if ('lease' in update) lease = typeof update.lease === 'string' ? update.lease : null;
    return fresh;
  }

  function looking(): boolean {
    return document.visibilityState === 'visible' && document.hasFocus();
  }

  function schedule(delay: number) {
    clearTimeout(timer);
    timer = undefined;
    if (gone || !connected || !active || !looking()) return;
    timer = setTimeout(() => void poll(), delay);
  }

  async function poll() {
    timer = undefined;
    const session = active;
    if (!session || gone || !connected || polling) return;
    polling = true;
    try {
      const fresh = apply(await rpc('poll', { session: session.id, after: cursor }));
      wait = fresh ? FASTEST : Math.min(wait * 2, SLOWEST);
    } catch (e) {
      if (!gone && active === session) {
        error = errorMessage(e);
        connected = false;
        lease = null;
      }
    } finally {
      polling = false;
    }
    if (active === session) schedule(wait);
  }

  /** Back in front: ask now rather than finishing a long wait. */
  function resume() {
    if (timer === undefined && !polling && connected && looking()) {
      wait = FASTEST;
      schedule(0);
    }
  }

  async function load() {
    busy = true;
    error = null;
    try {
      const value = (await rpc('list')) as { sessions: Remote[] };
      if (
        !Array.isArray(value?.sessions) ||
        !value.sessions.every(
          (s) =>
            typeof s.id === 'string' &&
            typeof s.title === 'string' &&
            typeof s.host === 'string',
        )
      )
        throw new Error('Backend returned an invalid session list.');
      if (!gone) list = value.sessions;
    } catch (e) {
      if (!gone) error = errorMessage(e);
    } finally {
      busy = false;
    }
  }
  onMount(() => void load());

  async function attach(session: Remote) {
    if (busy) return;
    busy = true;
    error = null;
    try {
      const value = await rpc('attach', {
        session: session.id,
        after: active?.id === session.id ? cursor : 0,
      });
      if (gone) {
        await rpc('detach', { session: session.id }).catch(() => {});
        return;
      }
      if (active?.id !== session.id) {
        cursor = 0;
        transcript = '';
      }
      active = session;
      lease = null;
      apply(value);
      connected = true;
      wait = FASTEST;
      schedule(wait);
    } catch (e) {
      error = errorMessage(e);
      connected = false;
      lease = null;
    } finally {
      busy = false;
    }
  }

  async function control() {
    const session = active;
    if (!session || busy) return;
    busy = true;
    try {
      const value = (await rpc('control', {
        session: session.id,
        release: !!lease,
        lease,
      })) as { lease: string | null };
      lease = value.lease;
      error = null;
      wait = FASTEST;
      schedule(wait);
    } catch (e) {
      error = errorMessage(e);
      lease = null;
    } finally {
      busy = false;
    }
  }

  async function send() {
    const session = active;
    if (!session || !lease || !message.trim() || busy) return;
    busy = true;
    const text = message;
    try {
      await rpc('input', {
        session: session.id,
        lease,
        text,
        input_id: crypto.randomUUID(),
      });
      message = '';
      wait = FASTEST;
      schedule(wait);
    } catch (e) {
      error = errorMessage(e);
      connected = false;
      lease = null;
    } finally {
      busy = false;
    }
  }

  /**
   * Let go of the attached session, once. `active` is cleared before the request goes out, so
   * closing the dialog, which detaches and then unmounts, does not detach a second time.
   */
  async function detach() {
    const session = active;
    const held = lease;
    clearTimeout(timer);
    timer = undefined;
    active = null;
    connected = false;
    lease = null;
    if (session) await rpc('detach', { session: session.id, lease: held }).catch(() => {});
  }

  async function back() {
    if (busy) return;
    busy = true;
    await detach();
    busy = false;
    if (!gone) await load();
  }

  async function close() {
    if (busy) return;
    busy = true;
    await detach();
    gone = true;
    onclose();
  }

  onDestroy(() => {
    gone = true;
    void detach();
  });

  // Follow the output while the reader is at its end, and leave them where they are otherwise.
  $effect(() => {
    void transcript;
    if (!following) return;
    void tick().then(() => {
      if (output) output.scrollTop = output.scrollHeight;
    });
  });
</script>

<svelte:window onfocus={resume} onblur={() => schedule(wait)} />
<svelte:document onvisibilitychange={() => (looking() ? resume() : schedule(wait))} />

<Dialog title={active?.title ?? title} onclose={() => void close()} closeDisabled={busy}>
  {#snippet body()}<div class="o-stack">
      {#if error}<Banner>{error}</Banner>{/if}
      {#if active}
        <p>
          {connected ? 'Attached to' : 'Disconnected from'}
          {active.host} · {lease ? 'You control input' : 'Viewing'}
        </p>
        <pre
          class="c-plugins__code"
          role="log"
          aria-label="Session output"
          bind:this={output}
          onscroll={() => {
            if (output)
              following = output.scrollTop + output.clientHeight >= output.scrollHeight - 8;
          }}>{transcript || 'No events received yet.'}</pre>
        {#if connected}
          <div>
            <Button disabled={busy} onclick={() => void control()}
              >{lease ? 'Release control' : 'Request control'}</Button
            >
          </div>
          {#if lease}<form
              class="o-row c-plugins__inline"
              onsubmit={(event) => {
                event.preventDefault();
                void send();
              }}
            >
              <TextInput
                bind:value={message}
                ariaLabel="Message to remote session"
                disabled={busy}
              /><Button type="submit" disabled={busy || !message.trim()}>Send</Button>
            </form>{/if}
        {:else}<div>
            <Button disabled={busy} onclick={() => active && void attach(active)}
              >Reconnect to this session</Button
            >
          </div>{/if}
        <p class="c-field__help">
          Detaching leaves the host session running. Reconnecting never starts a new
          session. Only the host can grant input control.
        </p>
      {:else}
        <p class="c-field__help">
          Attach to an existing session on its original host. Work and credentials stay
          there.
        </p>
        <ul class="o-plain-list o-stack">
          {#each list as s (s.id)}<li class="o-row o-row--between">
              <span class="o-stack o-stack--tight"
                ><strong>{s.title}</strong><span class="c-note">{s.host}</span></span
              ><Button disabled={busy} onclick={() => void attach(s)}
                >Attach as viewer</Button
              >
            </li>{:else}<li>
              {busy ? 'Reading sessions…' : 'This backend has no sessions available.'}
            </li>{/each}
        </ul>
        <div><Button disabled={busy} onclick={() => void load()}>Refresh</Button></div>
      {/if}
    </div>{/snippet}
  {#snippet footer()}
    {#if active}<Button disabled={busy} onclick={() => void back()}>Detach</Button>{/if}
    <Button variant="accent" disabled={busy} onclick={() => void close()}
      >{active ? 'Detach and close' : 'Done'}</Button
    >
  {/snippet}
</Dialog>
