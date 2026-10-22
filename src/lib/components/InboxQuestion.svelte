<script lang="ts">
  /**
   * A question a run asked, laid out to be judged without leaving where it is shown: the
   * question, why it matters, what it unblocks, what it points at, what the asker recommends,
   * then the answer — the plan's §2.4 question, the same object in the inbox and in the issue's
   * Plan tab, so answering in either place answers it.
   *
   * # A planner's question, and a reviewer's
   *
   * A planner waits for its answer: its run is held, its session closed, and the answer resumes
   * the same provider conversation (`flow/planner.rs`). So its card offers **Discuss**, a threaded
   * follow-up in that conversation before answering, and says nothing is waiting on a CLI. A review
   * role was told not to wait; its card says it went ahead on its own reading, and Discuss has no
   * session to talk to.
   *
   * # The first answer counts
   *
   * Rust refuses a second answer (`answer_question`), so answering here after answering somewhere
   * else is an error that says so, never a second answer.
   *
   * # Send to Jira takes two clicks
   *
   * Answers stay in wtm. **Send to Jira…** asks Rust for a draft — the question, why it mattered
   * and your answer, never the discussion — and shows it to edit; only **Post** sends, exactly
   * the text in the box, through the outbox. The first click records nothing anywhere.
   */
  import { untrack } from 'svelte';

  import type { ContextLink, InboxItem, Question } from '../ipc/types';
  import { inbox } from '../state/inbox.svelte';
  import { plan } from '../state/plan.svelte';
  import { answerWords, commentWords } from '../plan';
  import CodeText from './CodeText.svelte';
  import Markdown from './Markdown.svelte';
  import Button from './ui/Button.svelte';

  const {
    item = null,
    question,
    onopenfile,
  }: {
    /** The inbox item about it, when it is shown from the inbox. */
    item?: InboxItem | null;
    question: Question;
    /** Open a file the question points at, at its line. */
    onopenfile: (path: string, line: number | null) => void;
  } = $props();

  const thread = $derived(plan.threads[question.id] ?? null);
  /** The newest word on the question: the thread's read when there is one. */
  const asked = $derived(thread?.question ?? question);
  const answered = $derived(asked.answer !== null);
  const waits = $derived(asked.blocks !== null);
  const draft = $derived(plan.drafts[question.id] ?? null);

  // The thread is read when a question is shown, untracked for the reason `WorkIssue` gives: the
  // read writes the store it would otherwise be tracking.
  $effect(() => {
    const id = question.id;
    untrack(() => void plan.thread(id));
  });

  /**
   * The option the asker recommends, when its words lead the recommendation: the whole option, or
   * the option's head before its explanation — a planner writes "CSV first (stdlib, no dependency)"
   * as the option and "CSV first. It fits the estimate…" as the recommendation. The longest head
   * that matches wins, so "CSV" does not beat "CSV first" when both are offered.
   */
  const recommended = $derived.by(() => {
    const said = asked.recommendation?.toLowerCase().trim() ?? '';
    if (said === '') return null;
    let best: number | null = null;
    let longest = 0;
    for (const [index, option] of asked.options.entries()) {
      const whole = option.toLowerCase().trim();
      const head = whole.split(/\s[(—–-]|[(:;,]/)[0]?.trim() ?? '';
      for (const lead of [whole, head]) {
        const ends =
          said.length === lead.length || /[^a-z0-9]/.test(said[lead.length] ?? '');
        if (lead.length >= 2 && lead.length > longest && said.startsWith(lead) && ends) {
          best = index;
          longest = lead.length;
        }
      }
    }
    return best;
  });

  // Seeded once per question, from the recommendation: what the card opens with is the answer
  // the asker would give, which is one click from being yours.
  let choice = $state<number | null>(null);
  let note = $state('');
  let discussing = $state(false);
  let message = $state('');
  let seededFor = -1;
  $effect(() => {
    if (question.id === seededFor) return;
    seededFor = question.id;
    choice = recommended;
    note = '';
    discussing = false;
    message = '';
  });

  const key = $derived(`q${question.id}`);
  const busy = $derived(
    (item ? (inbox.busy[item.id] ?? false) : false) || (plan.busy[key] ?? false),
  );
  const error = $derived((item ? inbox.errors[item.id] : null) ?? plan.errors[key] ?? null);
  const canAnswer = $derived(choice !== null || note.trim() !== '');
  const replying = $derived(thread?.replying ?? false);

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
    if (item) void inbox.answer(item, choice, note);
    else void plan.answer(question.id, choice, note);
  }

  async function discuss(): Promise<void> {
    const said = message.trim();
    if (said === '') return;
    if (await plan.discuss(question.id, said)) {
      message = '';
      discussing = false;
    }
  }
</script>

<article class="c-question" aria-labelledby="question-{question.id}">
  <div class="c-question__meta">
    <span class="c-question__badge">Question</span>
    <span class="c-question__who">
      {asked.role ? `from ${asked.role}` : 'from the run'} · {waits
        ? answered
          ? 'answered; the run carried on'
          : 'the run waits for your answer, and no CLI is held open'
        : 'the run did not wait for it'}
    </span>
  </div>
  <h2 class="c-question__title" id="question-{question.id}">
    <CodeText text={asked.text} />
  </h2>

  <div class="c-question__grid">
    <div class="c-question__field">
      <span class="c-question__label">Why it matters</span>
      <p><CodeText text={asked.why} /></p>
    </div>
    <div class="c-question__field">
      <span class="c-question__label">Unblocks</span>
      <p><CodeText text={asked.unblocks} /></p>
    </div>
  </div>

  {#if asked.context.length > 0}
    <div class="c-question__field">
      <span class="c-question__label">Context</span>
      <ul class="c-question__links">
        {#each asked.context as link, index (index)}
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

  {#if asked.recommendation}
    <div class="c-question__recommendation">
      <p>
        <strong>Recommendation:</strong>
        <CodeText text={asked.recommendation} />
      </p>
    </div>
  {/if}

  {#if !waits}
    <p class="c-question__note">
      This role went ahead on its own reading — review runs don't stop for answers — and
      says which it chose in its findings. Your answer is kept with the run and given to the
      role when it runs again.
    </p>
  {/if}

  {#if thread && (thread.discussion.length > 0 || replying)}
    <section class="c-discussion" aria-label="Discussion">
      <span class="c-question__label">Discussion</span>
      <ol class="c-discussion__list">
        {#each thread.discussion as said (said.id)}
          <li class="c-discussion__message" class:is-agent={said.from === 'agent'}>
            <strong class="c-discussion__who"
              >{said.from === 'you' ? 'You' : (asked.role ?? 'The role')}:</strong
            >
            {#if said.from === 'agent'}
              <!-- A model's reply is Markdown; the renderer builds elements, never markup. -->
              <Markdown source={said.text} />
            {:else}
              <CodeText text={said.text} />
            {/if}
          </li>
        {/each}
        {#if replying}
          <li class="c-discussion__message is-agent">
            <strong class="c-discussion__who">{asked.role ?? 'The role'}:</strong>
            <span class="c-question__note">replying in the same conversation…</span>
          </li>
        {/if}
      </ol>
    </section>
  {/if}

  {#if answered && asked.answer}
    <div class="c-question__field">
      <span class="c-question__label">Your answer</span>
      <p><CodeText text={answerWords(asked, asked.answer)} /></p>
    </div>
  {:else}
    <form class="c-question__answer" onsubmit={submit}>
      <fieldset class="c-question__options" disabled={busy}>
        <legend class="c-question__legend">Your answer</legend>
        {#each asked.options as option, index (index)}
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
          {asked.options.length > 0 ? 'Add a note (optional)' : 'Answer'}
          <textarea class="c-textarea" rows="3" bind:value={note}></textarea>
        </label>
      </fieldset>
      <div class="c-question__actions">
        <Button variant="accent" type="submit" disabled={busy || replying || !canAnswer}
          >{waits ? 'Answer and resume' : 'Answer'}</Button
        >
        <Button
          disabled={busy || !thread?.canDiscuss}
          ariaExpanded={discussing}
          title={thread?.canDiscuss
            ? 'Ask the role about its question first, in the same conversation. The run keeps waiting for your answer.'
            : waits
              ? replying
                ? 'It is replying now.'
                : 'Discuss is for an open question whose run waits for it.'
              : "This role's session has ended: it did not wait for an answer."}
          onclick={() => (discussing = !discussing)}>Discuss first</Button
        >
        <span class="c-question__spacer"></span>
        <Button
          variant="quiet"
          disabled
          title="Answer it first: Send to Jira sends the question with your answer."
          >Send to Jira…</Button
        >
        {#if item}
          <Button variant="quiet" disabled={busy} onclick={() => void inbox.dismiss(item)}
            >Dismiss</Button
          >
        {/if}
      </div>
    </form>
  {/if}

  {#if discussing && !answered}
    <form
      class="c-discussion__compose"
      onsubmit={(event) => {
        event.preventDefault();
        void discuss();
      }}
    >
      <label class="c-question__note-field">
        Ask {asked.role ?? 'the role'} about it
        <textarea class="c-textarea" rows="2" bind:value={message}></textarea>
      </label>
      <div class="c-question__actions">
        <Button variant="accent" type="submit" disabled={busy || message.trim() === ''}
          >Send to {asked.role ?? 'the role'}</Button
        >
        <Button variant="quiet" onclick={() => (discussing = false)}>Cancel</Button>
      </div>
      <p class="c-question__note">
        It replies in the same conversation, here. Nothing is posted anywhere, and the run
        keeps waiting for your answer.
      </p>
    </form>
  {/if}

  {#if answered}
    {#each thread?.comments ?? [] as sent (sent.approval)}
      <p class="c-comment-sent c-status--{sent.state === 'failed' ? 'danger' : 'ok'}">
        {commentWords(sent)}
        {#if sent.state === 'failed'}
          <Button
            size="sm"
            disabled={busy}
            onclick={() => void plan.retry(question.id, sent.approval)}>Retry</Button
          >
        {/if}
      </p>
    {/each}
    {#if draft}
      <section class="c-comment-draft" aria-label="Draft comment for {draft.key}">
        <label class="c-question__note-field">
          Draft comment on {draft.key} — edit it as you like
          <textarea
            class="c-textarea"
            rows="6"
            value={draft.body}
            oninput={(event) =>
              plan.editDraft(
                question.id,
                (event.currentTarget as HTMLTextAreaElement).value,
              )}></textarea>
        </label>
        <p class="c-question__note">
          Nothing has been sent. Post sends exactly this text to {draft.key} as a comment by you.
          The discussion with {asked.role ?? 'the role'} is not in it, and nothing else is posted.
        </p>
        <div class="c-question__actions">
          <Button
            variant="accent"
            disabled={busy || draft.body.trim() === ''}
            onclick={() => void plan.post(question.id)}>Post to {draft.key}</Button
          >
          <Button
            variant="quiet"
            disabled={busy}
            onclick={() => plan.closeDraft(question.id)}>Cancel</Button
          >
        </div>
      </section>
    {:else if thread?.issue}
      <div class="c-question__actions">
        <Button
          variant="quiet"
          disabled={busy}
          title="Opens a draft to edit. Nothing is posted until you click Post."
          onclick={() => void plan.draft(question.id)}>Send to Jira…</Button
        >
      </div>
    {/if}
  {/if}

  {#if error}
    <p class="c-question__error" role="alert"><CodeText text={error} /></p>
  {/if}
  <p class="c-question__note">
    {waits
      ? 'Answers stay in wtm. Your answer resumes the same conversation the question was asked in. Send to Jira opens an editable draft and posts only when you post it.'
      : 'Answers stay in wtm. Nothing is posted anywhere.'}
  </p>
</article>
