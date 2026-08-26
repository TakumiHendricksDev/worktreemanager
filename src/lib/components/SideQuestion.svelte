<script lang="ts">
  /** A single-response, dismissible fork of the parent conversation. */
  import type { Pane } from '../state/sessions.svelte';
  import { sessions } from '../state/sessions.svelte';
  import Markdown from './Markdown.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';
  import TextInput from './ui/TextInput.svelte';

  const { side }: { side: Pane } = $props();

  let draft = $state('');
  let sending = $state(false);

  const question = $derived(
    side.events.find((event) => event.kind === 'user_echo')?.text ?? null,
  );
  const answer = $derived.by(() => {
    let text = '';
    for (const event of side.events) {
      if (event.kind === 'message_delta' || event.kind === 'message') text += event.text;
    }
    return text;
  });
  const failure = $derived.by(() => {
    if (side.error) return side.error;
    for (let index = side.events.length - 1; index >= 0; index -= 1) {
      const event = side.events[index];
      if (event?.kind === 'failed') return event.message;
    }
    return null;
  });

  async function ask(event: Event) {
    event.preventDefault();
    const text = draft.trim();
    if (!text || sending) return;
    sending = true;
    const sent = await sessions.send(side.id, text);
    sending = false;
    if (sent) draft = '';
  }
</script>

<aside class="c-side-question" aria-label="Ephemeral side question">
  <header class="c-side-question__head">
    <div>
      <strong>Side question</strong>
      <span>Ephemeral · not added to the main conversation</span>
    </div>
    <Button
      variant="quiet"
      size="sm"
      icon="sm"
      title="Dismiss side question"
      ariaLabel="Dismiss side question"
      onclick={() => void sessions.close(side.id)}
    >
      <Icon name="close" size={11} />
    </Button>
  </header>

  {#if question !== null}
    <p class="c-side-question__prompt">{question}</p>
  {/if}

  {#if answer}
    <div class="c-side-question__answer">
      <Markdown source={answer} />
    </div>
  {:else if failure}
    <p class="c-side-question__error">{failure}</p>
  {:else if side.working}
    <p class="c-side-question__waiting" aria-live="polite">Thinking…</p>
  {:else if question !== null}
    <p class="c-side-question__waiting" aria-live="polite">No written reply.</p>
  {:else if !side.ready}
    <p class="c-side-question__waiting" aria-live="polite">Opening side chat…</p>
  {:else}
    <form class="c-side-question__ask" onsubmit={(event) => void ask(event)}>
      <TextInput
        ariaLabel="Side question"
        placeholder="Ask about what’s already in this conversation…"
        bind:value={draft}
      />
      <Button type="submit" variant="accent" size="sm" disabled={!draft.trim() || sending}>
        Ask
      </Button>
    </form>
  {/if}
</aside>
