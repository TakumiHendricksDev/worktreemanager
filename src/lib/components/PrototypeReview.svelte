<script lang="ts">
  import { setContext as setExtensionContext, untrack } from 'svelte';
  import { EXTENSION_PROTECTED } from '../state/extensions.svelte';
  setExtensionContext(EXTENSION_PROTECTED, true);
  import { listen } from '@tauri-apps/api/event';
  import { commands } from '../ipc/commands';
  import { errorMessage } from '../ipc/types';
  import type { PrototypeChanged, PrototypeState } from '../open-design';
  import { sessions, type Pane } from '../state/sessions.svelte';
  import Button from './ui/Button.svelte';
  import { nativeSessions } from '../state/native-sessions.svelte';

  const {
    pane,
    onreview,
  }: {
    pane: Pane;
    onreview?: (review: PrototypeState | null) => void;
  } = $props();
  let records = $state<PrototypeState[]>([]);
  let feedback = $state('');
  let error = $state<string | null>(null);
  let busy = $state(false);
  let selected = $state<string[]>([]);
  let contextChanged = $state(false);
  // Newest design still awaiting review. An approved or dismissed record stays saved,
  // but it must not hide Show preview and Implement for another design in this chat.
  const pending = $derived(
    records.filter((r) => !r.prototype.implementedReview && !r.prototype.dismissed),
  );
  const current = $derived(pending[0] ?? null);
  const design = $derived(current?.prototype ?? null);
  const earlier = $derived(pending.slice(1));
  const native = $derived(pane.session ? nativeSessions.states[pane.session] : undefined);
  const conversation = $derived(native?.conversation ?? pane.providerSession);
  const receipt = $derived(
    native?.deliveries.find(
      (r) =>
        r.prototype === design?.id &&
        r.review === design?.review?.id &&
        r.status !== 'rejected',
    ),
  );
  const nativeBlocked = $derived(
    native &&
      (native.status !== 'idle' ||
        native.deliveries.some((r) => r.status === 'pending' || r.status === 'unknown')),
  );

  // A native review stays subscribed while hidden, so a newly published revision can reveal it.
  $effect(() => {
    const review = current;
    untrack(() => onreview?.(review));
  });

  function accept(state: PrototypeState) {
    const next = [state, ...records.filter((r) => r.prototype.id !== state.prototype.id)];
    const shown = next.find(
      (r) => !r.prototype.implementedReview && !r.prototype.dismissed,
    );
    if (shown?.prototype.id !== design?.id) feedback = '';
    records = next;
    selected = shown?.prototype.sources.map((s) => s.path) ?? [];
    contextChanged = false;
  }

  $effect(() => {
    const session = pane.session;
    // A native /clear or /resume keeps the PTY but changes which review belongs here.
    const owner = conversation;
    records = [];
    feedback = '';
    selected = [];
    contextChanged = false;
    error = null;
    if (!session) return;
    let stopped = false;
    let updated = false;
    let off: (() => void) | undefined;
    void listen<PrototypeChanged>('prototype:changed', (event) => {
      if (!stopped && owner === conversation && event.payload.session === session) {
        updated = true;
        accept(event.payload.state);
      }
    }).then((unlisten) => {
      if (stopped) unlisten();
      else off = unlisten;
    });
    void commands
      .listPrototypes(session)
      .then((list) => {
        if (stopped || updated || owner !== conversation) return;
        records = list;
        selected =
          list
            .find((r) => !r.prototype.implementedReview && !r.prototype.dismissed)
            ?.prototype.sources.map((s) => s.path) ?? [];
      })
      .catch((e) => {
        if (!stopped) error = errorMessage(e);
      });
    return () => {
      stopped = true;
      off?.();
    };
  });

  async function perform(action: () => Promise<unknown>) {
    busy = true;
    error = null;
    try {
      await action();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  async function revise(refresh = false) {
    if (!design) return;
    const note =
      feedback.trim() ||
      (refresh
        ? 'Refresh the project context and regenerate the design.'
        : design.feedback.at(-1) || design.brief);
    const context = contextChanged
      ? ` Use these source paths in prototype_prepare: ${JSON.stringify(selected)}.`
      : '';
    const sent = await sessions.send(
      pane.id,
      `Revise Open Design prototype ${design.id}: ${note}. Call prototype_prepare with this id, then prototype_publish and wait for review.${context}`,
    );
    if (sent) {
      feedback = '';
      contextChanged = false;
    } else
      throw new Error(sessions.error ?? 'The feedback was not sent. Retry in this chat.');
  }
</script>

{#if design && current && !design.implementedReview && !design.dismissed}
  <section class="c-design-first" aria-label="Prototype review">
    <div class="c-design-first__heading">
      <div class="c-design-first__identity">
        <span class="c-eyebrow">Open Design · this conversation</span>
        <h3 class="c-design-first__title">{design.title}</h3>
      </div>
      <div class="c-design-first__meta">
        <span class="c-note" role="status">{design.phase}</span>
        <Button
          size="sm"
          ariaLabel="Dismiss design review"
          title="Hide this review card; the saved design is kept"
          disabled={busy || !pane.session}
          onclick={() =>
            void perform(async () =>
              accept(await commands.dismissPrototype(pane.session!, design.id)),
            )}>Dismiss</Button
        >
      </div>
    </div>
    <p class="c-note">
      Uses {design.sources.filter((s) => s.kind === 'component').length} component references
      and {design.sources.filter((s) => s.kind === 'tokens').length} token/theme sources from
      this worktree, including local edits. Your current chat agent creates the prototype.
    </p>
    <details>
      <summary>Inspect project context</summary>
      <p class="c-note">
        Adjust the context for the next revision. Framework components are visually
        approximated in HTML; implementation uses the real APIs.
      </p>
      <div class="c-design-first__sources">
        {#each design.sources as source}
          <div class="c-design-first__source">
            <label class="c-design-first__choice">
              <input
                type="checkbox"
                value={source.path}
                bind:group={selected}
                onchange={() => (contextChanged = true)}
              />
              {source.path} · {source.kind}
            </label>
            <details>
              <summary>Source excerpt</summary>
              <pre class="c-design-first__excerpt">{source.text}</pre>
            </details>
          </div>
        {/each}
      </div>
    </details>
    {#if current.stale}
      <p class="c-status--danger">
        Project context changed. Refresh and review a new revision before implementation.
      </p>
    {/if}
    {#if design.review}
      <div class="c-design-first__actions">
        <Button
          size="sm"
          disabled={busy || !pane.session}
          onclick={() =>
            void perform(() =>
              commands.openPrototypePreview(pane.session!, design.id, design.review!.id),
            )}>Show preview</Button
        >
        <Button
          variant="accent"
          size="sm"
          disabled={busy ||
            !!receipt ||
            !!nativeBlocked ||
            pane.working ||
            current.stale ||
            !design.review.opened ||
            feedback.trim().length > 0 ||
            contextChanged ||
            !pane.session ||
            pane.ended !== null}
          onclick={() =>
            void perform(() =>
              commands.implementPrototype(pane.session!, design.id, design.review!.id),
            )}
        >
          {receipt?.status === 'delivered'
            ? 'Delivered to this conversation'
            : receipt?.status === 'pending'
              ? 'Approved · awaiting your next message'
              : receipt?.status === 'unknown'
                ? 'Delivery unknown'
                : 'Implement this design'}
        </Button>
      </div>
      {#if receipt}<p class="c-note" role="status">
          {receipt.status === 'pending'
            ? 'Type your next message in the native CLI to collect this exact approval. It is not delivered yet.'
            : receipt.status === 'unknown'
              ? 'WTM cannot confirm delivery. Inspect this conversation; this approval will not be replayed automatically.'
              : 'The provider acknowledged this exact review. This is delivery confirmation, not implementation completion.'}
        </p>{/if}
      <p class="c-note">
        Implement sends this exact reviewed revision to this conversation with its current
        model and permissions.
      </p>
    {:else if design.phase === 'generating'}
      <p role="status">
        {pane.working
          ? 'Creating your prototype…'
          : 'No preview was published. Ask for a revision or retry below.'}
      </p>
      <div class="c-design-first__actions">
        {#if pane.working}
          <Button
            size="sm"
            disabled={busy}
            onclick={() =>
              void perform(() => commands.cancelPrototype(pane.session!, design.id))}
            >Cancel design</Button
          >
        {:else}
          <Button
            size="sm"
            disabled={busy || !pane.session}
            onclick={() => void perform(() => revise())}>Retry design</Button
          >
        {/if}
      </div>
    {/if}
    {#if !design.implementedReview}
      <label class="c-design-first__feedback">
        Feedback for this design
        <textarea
          class="c-input"
          rows="2"
          bind:value={feedback}
          placeholder="Try a simpler layout, keep the existing button style…"></textarea>
      </label>
      <div class="c-design-first__actions">
        <Button
          size="sm"
          disabled={busy ||
            pane.working ||
            !!nativeBlocked ||
            (!feedback.trim() && !contextChanged && !current.stale) ||
            (design.sources.length > 0 && !selected.length) ||
            pane.ended !== null}
          onclick={() => void perform(() => revise(current.stale))}
          >{current.stale ? 'Refresh and revise' : 'Revise in this chat'}</Button
        >
      </div>
    {/if}
    {#if earlier.length > 0}
      <details>
        <summary>Earlier prototypes in this conversation</summary>
        {#each earlier as record}
          <Button
            variant="link"
            size="sm"
            onclick={() => {
              const other = records.filter((r) => r !== record);
              records = [record, ...other];
              selected = record.prototype.sources.map((s) => s.path);
            }}>{record.prototype.title}</Button
          >
        {/each}
      </details>
    {/if}
    {#if error}<p role="alert" class="c-status--danger">{error}</p>{/if}
  </section>
{:else if error}
  <p class="c-note" role="alert">Prototype history: {error}</p>
{/if}
