<script lang="ts">
  /**
   * A question a run asked, laid out to be judged without leaving the inbox: the question, why it
   * matters, what it unblocks, what it points at, what the asker recommends, then the answer.
   *
   * # The planner's layout, drawn for a reviewer's question
   *
   * Every field is the plan's §2.4 question — the M3 planner's questions are what it is for. A
   * review role's question is rarer and has less behind it: it was told not to wait, so the card
   * says the role went ahead on its own reading, and that the answer reaches it when it runs again.
   * Discuss, a threaded follow-up with the asker, needs a session that is still there to talk to,
   * which a review role's is not; it is drawn, disabled, with the reason.
   *
   * # The first answer counts
   *
   * Rust refuses a second answer (`answer_question`), so answering here after answering somewhere
   * else is an error that says so, never a second answer.
   */
  import type { ContextLink, InboxItem, Question } from '../ipc/types';
  import { inbox } from '../state/inbox.svelte';
  import CodeText from './CodeText.svelte';
  import Button from './ui/Button.svelte';

  const {
    item,
    question,
    onopenfile,
  }: {
    item: InboxItem;
    question: Question;
    /** Open a file the question points at, at its line. */
    onopenfile: (path: string, line: number | null) => void;
  } = $props();

  /** The option the asker recommends, when its words lead the recommendation. */
  const recommended = $derived.by(() => {
    const said = question.recommendation?.toLowerCase().trim() ?? '';
    if (said === '') return null;
    const index = question.options.findIndex((option) =>
      said.startsWith(option.toLowerCase().trim()),
    );
    return index >= 0 ? index : null;
  });

  // Seeded once per question, from the recommendation: what the card opens with is the answer
  // the asker would give, which is one click from being yours.
  let choice = $state<number | null>(null);
  let note = $state('');
  let seededFor = -1;
  $effect(() => {
    if (question.id === seededFor) return;
    seededFor = question.id;
    choice = recommended;
    note = '';
  });

  const busy = $derived(inbox.busy[item.id] ?? false);
  const error = $derived(inbox.errors[item.id] ?? null);
  const canAnswer = $derived(choice !== null || note.trim() !== '');

  function label(link: ContextLink): string {
    switch (link.kind) {
      case 'file':
        return link.line === null ? link.path : `${link.path}:${link.line}`;
      case 'excerpt':
        return link.source || 'Excerpt';
      case 'design_revision':
        return `${link.prototype} revision ${link.revision}`;
      case 'mention':
        return link.text;
    }
  }

  function submit(event: SubmitEvent): void {
    event.preventDefault();
    if (!canAnswer) return;
    void inbox.answer(item, choice, note);
  }
</script>

<article class="c-question" aria-labelledby="question-{question.id}">
  <div class="c-question__meta">
    <span class="c-question__badge">Question</span>
    <span class="c-question__who">
      {question.role ? `from ${question.role}` : 'from the run'} · {question.blocks
        ? `blocks ${question.blocks.replaceAll('_', ' ')}`
        : 'the run did not wait for it'}
    </span>
  </div>
  <h2 class="c-question__title" id="question-{question.id}">
    <CodeText text={question.text} />
  </h2>

  <div class="c-question__grid">
    <div class="c-question__field">
      <span class="c-question__label">Why it matters</span>
      <p><CodeText text={question.why} /></p>
    </div>
    <div class="c-question__field">
      <span class="c-question__label">Unblocks</span>
      <p><CodeText text={question.unblocks} /></p>
    </div>
  </div>

  {#if question.context.length > 0}
    <div class="c-question__field">
      <span class="c-question__label">Context</span>
      <ul class="c-question__links">
        {#each question.context as link, index (index)}
          <li>
            {#if link.kind === 'file'}
              <Button
                variant="link"
                size="sm"
                title="Open in the Code tab"
                onclick={() => onopenfile(link.path, link.line)}
                ><span class="u-mono">{label(link)}</span></Button
              >
              {#if link.note}
                <span class="c-question__excerpt"><CodeText text={link.note} /></span>
              {/if}
            {:else}
              <span class="u-mono">{label(link)}</span>
              {#if link.kind === 'excerpt'}
                <span class="c-question__excerpt">“{link.text}”</span>
              {/if}
            {/if}
          </li>
        {/each}
      </ul>
    </div>
  {/if}

  {#if question.recommendation}
    <div class="c-question__recommendation">
      <p>
        <strong>Recommendation:</strong>
        <CodeText text={question.recommendation} />
      </p>
    </div>
  {/if}

  {#if question.blocks === null}
    <p class="c-question__note">
      This role went ahead on its own reading — review runs don't stop for answers yet — and
      says which it chose in its findings. Your answer is kept with the run and given to the
      role when it runs again.
    </p>
  {/if}

  <form class="c-question__answer" onsubmit={submit}>
    <fieldset class="c-question__options" disabled={busy}>
      <legend class="c-question__legend">Your answer</legend>
      {#each question.options as option, index (index)}
        <label class="c-question__option">
          <input
            type="radio"
            name="question-{question.id}"
            checked={choice === index}
            onchange={() => (choice = index)}
          />
          <CodeText text={option} />{#if index === recommended}
            <span class="c-question__recommended">recommended</span>{/if}
        </label>
      {/each}
      <label class="c-question__note-field">
        {question.options.length > 0 ? 'Add a note (optional)' : 'Answer'}
        <textarea class="c-textarea" rows="3" bind:value={note}></textarea>
      </label>
    </fieldset>
    <div class="c-question__actions">
      <Button variant="accent" type="submit" disabled={busy || !canAnswer}>Answer</Button>
      <Button
        disabled
        title="Discussing a question with the role that asked it arrives with the planner's questions. This role's session has ended."
        >Discuss</Button
      >
      <span class="c-question__spacer"></span>
      <Button variant="quiet" disabled={busy} onclick={() => void inbox.dismiss(item)}
        >Dismiss</Button
      >
    </div>
    {#if error}
      <p class="c-question__error" role="alert"><CodeText text={error} /></p>
    {/if}
    <p class="c-question__note">Answers stay in wtm. Nothing is posted anywhere.</p>
  </form>
</article>
