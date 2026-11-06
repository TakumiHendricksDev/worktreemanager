<script lang="ts">
  import { setContext as setExtensionContext } from 'svelte';
  import { EXTENSION_PROTECTED } from '../state/extensions.svelte';
  setExtensionContext(EXTENSION_PROTECTED, true);
  import { onMount } from 'svelte';
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
  let gone = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  async function rpc(method: string, params: object = {}) {
    return commands.pluginSession(plugin, backend, method, { ...params, client });
  }
  function apply(value: unknown) {
    const update = value as Update;
    if (
      !Number.isSafeInteger(update?.cursor) ||
      !Array.isArray(update.events) ||
      !update.events.every((e) => Number.isSafeInteger(e.seq) && typeof e.text === 'string')
    )
      throw new Error('Backend returned an invalid event snapshot.');
    if (update.reset) {
      cursor = 0;
      transcript = '';
    }
    for (const e of update.events)
      if (e.seq > cursor) {
        transcript = (transcript + e.text + '\n').slice(-100_000);
        cursor = e.seq;
      }
    cursor = Math.max(cursor, update.cursor);
    if ('lease' in update) lease = typeof update.lease === 'string' ? update.lease : null;
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
      list = value.sessions;
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }
  async function attach(session: Remote) {
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
      clearTimeout(timer);
      timer = setTimeout(() => void poll(), 1000);
    } catch (e) {
      error = errorMessage(e);
      connected = false;
      lease = null;
    } finally {
      busy = false;
    }
  }
  async function poll() {
    if (!active || gone || !connected) return;
    try {
      const value = await rpc('poll', { session: active.id, after: cursor });
      if (!gone) apply(value);
    } catch (e) {
      error = errorMessage(e);
      connected = false;
      lease = null;
    }
    if (!gone && connected) timer = setTimeout(() => void poll(), 1000);
  }
  async function control() {
    if (!active) return;
    busy = true;
    try {
      const value = (await rpc('control', {
        session: active.id,
        release: !!lease,
        lease,
      })) as { lease: string | null };
      lease = value.lease;
      error = null;
    } catch (e) {
      error = errorMessage(e);
      lease = null;
    } finally {
      busy = false;
    }
  }
  async function send() {
    if (!active || !lease || !message.trim()) return;
    busy = true;
    const text = message;
    try {
      await rpc('input', {
        session: active.id,
        lease,
        text,
        input_id: crypto.randomUUID(),
      });
      message = '';
    } catch (e) {
      error = errorMessage(e);
      connected = false;
      lease = null;
    } finally {
      busy = false;
    }
  }
  async function close() {
    gone = true;
    clearTimeout(timer);
    if (active) await rpc('detach', { session: active.id, lease }).catch(() => {});
    onclose();
  }
  onMount(() => {
    void load();
    return () => {
      gone = true;
      clearTimeout(timer);
      if (active) void rpc('detach', { session: active.id, lease }).catch(() => {});
    };
  });
</script>

<Dialog title={active?.title ?? title} onclose={() => void close()} closeDisabled={busy}>
  {#snippet body()}<div class="o-stack">
      {#if error}<Banner>{error}</Banner>{/if}
      {#if active}
        <p>
          {connected ? 'Attached to' : 'Disconnected from'}
          {active.host} · {lease ? 'You control input' : 'Viewing'}
        </p>
        <pre class="c-plugins__code">{transcript || 'No events received yet.'}</pre>
        {#if connected}<div class="c-plugins__actions">
            <Button disabled={busy} onclick={() => void control()}
              >{lease ? 'Release control' : 'Request control'}</Button
            >
          </div>
          {#if lease}<TextInput
              bind:value={message}
              ariaLabel="Message to remote session"
              disabled={busy}
            /><Button disabled={busy || !message.trim()} onclick={() => void send()}
              >Send</Button
            >{/if}
        {:else}<Button disabled={busy} onclick={() => active && void attach(active)}
            >Reconnect to this session</Button
          >{/if}
        <p class="c-field__help">
          Detaching leaves the host session running. Reconnecting never starts a new
          session. Only the host can grant input control.
        </p>
      {:else}
        <p class="c-field__help">
          Attach to an existing session on its original host. Work and credentials remain
          there.
        </p>
        {#each list as s}<div class="c-plugins__contribution">
            <span
              ><strong>{s.title}</strong><small class="c-plugins__muted">{s.host}</small
              ></span
            ><Button disabled={busy} onclick={() => void attach(s)}>Attach as viewer</Button
            >
          </div>{:else}<p>
            {busy ? 'Reading sessions…' : 'This backend has no sessions available.'}
          </p>{/each}
        <Button disabled={busy} onclick={() => void load()}>Refresh</Button>
      {/if}
    </div>{/snippet}
  {#snippet footer()}<Button disabled={busy} onclick={() => void close()}
      >{active ? 'Detach' : 'Done'}</Button
    >{/snippet}
</Dialog>
