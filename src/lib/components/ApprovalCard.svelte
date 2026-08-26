<script lang="ts">
  /**
   * A session asking permission before it does something.
   *
   * # Why this cannot be scrolled past
   *
   * The server does not continue the turn until it has a reply, so an unanswered card is a stalled
   * session. A chip in the transcript would let the user scroll on and wonder why nothing is
   * happening; this sits at the bottom of the pane, above the composer, and stays until answered.
   * `role="alertdialog"` because that is what it is — a thing demanding a decision — without the
   * scrim and focus trap of a real modal, which would be wrong for something the transcript behind
   * it is the context for.
   *
   * # Why the verbs differ by request, and where `Edit…` went
   *
   * There was an `Edit…` button, shown for Claude because `AllowWithEdits` is a verb only Claude
   * has: its allow can carry a replacement payload and rewrite a tool call. Nothing was ever built
   * behind it. Clicking it sent `allow_with_edits` with an **empty** input, which the adapter turns
   * into `{"behavior":"allow","updatedInput":{}}` — so on a plan it read as Approve, and on a
   * command it approved the call with its arguments erased.
   *
   * It is gone rather than fixed here, because the fix is a payload editor and this card is not
   * one. The verb stays in the protocol for when something can honestly offer it.
   *
   * # Why a plan is not offered the same verbs as a command
   *
   * "Always this session" grants a *class* of action — the CLI proposes the rule and the adapter
   * passes it back. A plan is one document, approved once; there is no class of it to allow, and a
   * button whose meaning is undefined for the thing in front of you is worse than one fewer button.
   *
   * And a denied plan is not a refusal, it is a revision request: the agent is going to write
   * another one, and the only thing that makes the next one better is being told what was wrong.
   * So `deny` carries a message here where elsewhere it carries `null`.
   *
   * # Why a question can be declined
   *
   * `user_input` used to have exactly one verb, disabled until every question was answered — which
   * assumed the reader always has an answer. Often they have a question instead, and there was
   * nowhere to put it: the composer's Send is a Stop while the turn is in flight, so the only exit
   * was to interrupt and lose the question. `Discuss first` is a `deny` carrying the notes field,
   * and it is the same shape as the plan's `Request changes` for the same reason — a refusal whose
   * whole purpose is to be read.
   */
  import type { ApprovalAnswer, ApprovalRequest } from '../ipc/types';
  import Markdown from './Markdown.svelte';
  import Button from './ui/Button.svelte';
  import TextInput from './ui/TextInput.svelte';

  const {
    request,
    reading = false,
    onread,
    onanswer,
  }: {
    request: ApprovalRequest;
    /** True while the pane is showing the plan panel beside the transcript. */
    reading?: boolean;
    /** Toggle that panel. The pane owns it, so it can sit beside the card rather than over it. */
    onread?: () => void;
    onanswer: (answer: ApprovalAnswer) => void;
  } = $props();

  /** The one-line question. Deliberately not the body: a diff is not a sentence. */
  const heading = $derived.by(() => {
    switch (request.kind) {
      case 'command':
        return 'Run this command?';
      case 'file_change':
        return 'Apply this change?';
      case 'permissions':
        return 'Grant these permissions?';
      case 'plan_review':
        return 'Approve this plan?';
      case 'tool_input':
        return `${request.tool} needs a value`;
      case 'user_input':
        return request.questions.length === 1
          ? request.questions[0]?.header || 'A question for you'
          : `${request.questions.length} questions for you`;
    }
  });

  let selections = $state<Record<string, string[]>>({});
  let other = $state<Record<string, string>>({});
  let otherSelected = $state<Record<string, boolean>>({});
  let notes = $state('');

  /** What to send back with a plan denial, so the next plan is a revision rather than a guess. */
  let changes = $state('');
  let card = $state<HTMLElement | null>(null);

  /**
   * What makes this a different request from the last one.
   *
   * A second queued approval replaces this card without remounting the component, so anything typed
   * into it has to be cleared or it is carried into a decision it was not written for. This used to
   * cover `user_input` only, which meant two consecutive plan reviews shared a revision note — the
   * text you wrote about the first plan going back as the reason you rejected the second.
   */
  const requestKey = $derived.by(() => {
    switch (request.kind) {
      case 'user_input':
        return request.questions.map((question) => question.id).join('\0');
      case 'plan_review':
        return `plan\0${request.path ?? ''}\0${request.markdown.length}`;
      default:
        return request.kind;
    }
  });

  $effect(() => {
    void requestKey;
    selections = {};
    other = {};
    otherSelected = {};
    notes = '';
    changes = '';
    queueMicrotask(() => {
      card?.querySelector<HTMLElement>('button:not([disabled]), textarea, input')?.focus();
    });
  });

  const complete = $derived.by(() => {
    if (request.kind !== 'user_input') return true;
    return request.questions.every((question) => {
      if ((selections[question.id] ?? []).length > 0) return true;
      if (question.options.length === 0)
        return (other[question.id] ?? '').trim().length > 0;
      return (
        otherSelected[question.id] === true && (other[question.id] ?? '').trim().length > 0
      );
    });
  });

  function choose(questionId: string, label: string, multiple: boolean, checked: boolean) {
    if (!multiple) {
      selections = { ...selections, [questionId]: checked ? [label] : [] };
      if (checked) otherSelected = { ...otherSelected, [questionId]: false };
      return;
    }
    const current = selections[questionId] ?? [];
    const next = checked
      ? [...current.filter((value) => value !== label), label]
      : current.filter((value) => value !== label);
    selections = { ...selections, [questionId]: next };
  }

  function chooseOther(questionId: string, multiple: boolean, checked: boolean) {
    otherSelected = { ...otherSelected, [questionId]: checked };
    if (!multiple && checked) selections = { ...selections, [questionId]: [] };
    if (!checked) other = { ...other, [questionId]: '' };
  }

  /**
   * What goes back when the user would rather talk than choose.
   *
   * A default rather than `null`, for the reason `plan_review`'s denial gives: the adapter turns a
   * null message into "Denied in wtm", which is true of any refusal and tells a model nothing about
   * what to do next. Here the whole point is to be read — the answer to a declined question is a
   * conversation, and a model that reads "denied" will simply ask again.
   *
   * The notes box carries the follow-up. It is already on screen, already cleared per request by
   * the `requestKey` effect, and typing the question you have *while looking at* the questions you
   * were asked is the natural motion. Empty is fine: "they want to discuss it" is itself actionable.
   */
  function discuss(): string {
    const head =
      'The user does not want to pick from these options yet — they have follow-up questions. Do not call AskUserQuestion again right now. Answer them in the conversation first, and ask again once it is settled.';
    const extra = notes.trim();
    return extra === '' ? head : `${head}\n\nThey said: ${extra}`;
  }

  function submitAnswers() {
    if (request.kind !== 'user_input' || !complete) return;
    const answers: Record<string, string[]> = {};
    for (const question of request.questions) {
      answers[question.id] = [...(selections[question.id] ?? [])];
      const custom = (other[question.id] ?? '').trim();
      if (
        custom !== '' &&
        (question.options.length === 0 || otherSelected[question.id] === true)
      ) {
        answers[question.id]?.push(custom);
      }
    }
    onanswer({
      kind: 'user_input',
      answers,
      notes: notes.trim() === '' ? null : notes.trim(),
    });
  }
</script>

<div class="c-approval" role="alertdialog" aria-label={heading} bind:this={card}>
  <p class="c-approval__ask" aria-live="assertive">{heading}</p>

  {#if request.kind === 'command'}
    <code class="c-approval__body">{request.command}</code>
    {#if request.cwd}
      <p class="c-approval__where">in <code>{request.cwd}</code></p>
    {/if}
  {:else if request.kind === 'file_change'}
    {#if request.unified_diff}
      <pre class="c-approval__body c-approval__body--diff">{request.unified_diff}</pre>
    {:else}
      <!-- The diff arrives on the file-change stream rather than in the approval params, so it is
           usually the patch row above this card. Saying so beats an empty box. -->
      <p class="c-approval__where">The change is shown above.</p>
    {/if}
  {:else if request.kind === 'permissions'}
    {#if request.items.length > 0}
      <ul class="o-plain-list c-approval__grants">
        {#each request.items as item, i (i)}
          <li><code>{item}</code></li>
        {/each}
      </ul>
    {/if}
    <p class="c-approval__where">{request.summary}</p>
  {:else if request.kind === 'plan_review'}
    <!-- Through the renderer, like every other markdown in the app. As literal source this was the
         only document in the transcript showing its own `##` and `**` as punctuation.

         Suppressed while the panel is open: the same document twice on one screen, one copy in a
         bounded scroller, is not a second reading surface. -->
    {#if !reading}
      <div class="c-approval__body c-approval__body--plan">
        <Markdown source={request.markdown} />
      </div>
    {/if}
  {:else if request.kind === 'tool_input'}
    <p class="c-approval__where">{request.prompt}</p>
  {:else}
    <div class="c-approval__questions">
      {#each request.questions as question (question.id)}
        <fieldset class="c-approval__question">
          <legend>
            {#if question.header}<span class="c-approval__eyebrow">{question.header}</span
              >{/if}
            <span>{question.question}</span>
            {#if question.multiple}<small class="c-approval__hint"
                >Select all that apply</small
              >{/if}
          </legend>

          {#if question.options.length > 0}
            <div class="c-approval__choices">
              {#each question.options as option (option.label)}
                <label class="c-approval__choice">
                  <input
                    type={question.multiple ? 'checkbox' : 'radio'}
                    name="answer-{question.id}"
                    checked={(selections[question.id] ?? []).includes(option.label)}
                    onchange={(event) =>
                      choose(
                        question.id,
                        option.label,
                        question.multiple,
                        event.currentTarget.checked,
                      )}
                  />
                  <span>
                    <strong>{option.label}</strong>
                    {#if option.description}<small class="c-approval__hint"
                        >{option.description}</small
                      >{/if}
                  </span>
                </label>
              {/each}
              {#if question.allowsOther}
                <label class="c-approval__choice c-approval__choice--other">
                  <input
                    type={question.multiple ? 'checkbox' : 'radio'}
                    name="answer-{question.id}"
                    checked={otherSelected[question.id] === true}
                    onchange={(event) =>
                      chooseOther(
                        question.id,
                        question.multiple,
                        event.currentTarget.checked,
                      )}
                  />
                  <span><strong>Other</strong></span>
                  <input
                    class="c-approval__other"
                    type={question.secret ? 'password' : 'text'}
                    aria-label="Other answer for {question.question}"
                    placeholder="Type your answer"
                    value={other[question.id] ?? ''}
                    oninput={(event) => {
                      const value = event.currentTarget.value;
                      other = { ...other, [question.id]: value };
                      if (value !== '') {
                        otherSelected = { ...otherSelected, [question.id]: true };
                      }
                      if (!question.multiple && value !== '') {
                        selections = { ...selections, [question.id]: [] };
                      }
                    }}
                  />
                </label>
              {/if}
            </div>
          {:else}
            <TextInput
              type={question.secret ? 'password' : 'text'}
              ariaLabel={question.question}
              placeholder="Type your answer"
              value={other[question.id] ?? ''}
              oninput={(event) =>
                (other = {
                  ...other,
                  [question.id]: (event.currentTarget as HTMLInputElement).value,
                })}
            />
          {/if}
        </fieldset>
      {/each}
    </div>

    <label class="c-approval__notes">
      <span>Notes <small class="c-approval__hint">optional</small></span>
      <textarea
        class="c-textarea"
        rows="2"
        placeholder="Add context, constraints, or the question you want to ask instead"
        bind:value={notes}></textarea>
    </label>
  {/if}

  <!-- Narrowed by naming the two kinds that carry a reason rather than excluding the ones that do
       not: `plan_review` has a `path` where these have a `reason`, and an exclusion list would have
       to be corrected every time a variant is added. This way a new variant is a type error. -->
  {#if (request.kind === 'command' || request.kind === 'file_change') && request.reason}
    <p class="c-approval__reason">{request.reason}</p>
  {:else if request.kind === 'plan_review' && request.path}
    <p class="c-approval__reason">Written to <code>{request.path}</code></p>
  {/if}

  {#if request.kind === 'plan_review'}
    <!-- Optional, and unlabelled as required, because Approve must stay a one-click answer. It is
         here rather than behind Request changes so it can be written *while* reading the plan,
         which is when the objection actually occurs to you. -->
    <label class="c-approval__notes">
      <span>What should change? <small>optional</small></span>
      <textarea
        class="c-textarea"
        rows="2"
        placeholder="Sent to the agent if you request changes"
        bind:value={changes}></textarea>
    </label>
  {/if}

  <div class="o-row c-approval__actions">
    {#if request.kind === 'user_input'}
      <Button variant="accent" size="sm" disabled={!complete} onclick={submitAnswers}>
        Submit answer
      </Button>
      <span class="c-approval__divider" aria-hidden="true"></span>
      <!--
        The only way out of a question, and until now there wasn't one.

        This card had a single control, disabled until *every* question was answered — so two
        questions could not be half-answered, notes could not be sent alone, and there was no
        decline. Meanwhile the composer's Send is replaced by Stop while a turn is in flight, so
        the one visible exit was to interrupt the turn and lose the question with it. A person who
        has a question *about* the question was cornered.

        `deny` rather than a new verb: the protocol already carries it end to end and Claude accepts
        it for `AskUserQuestion` like any other tool, so this is a button, not a wire change. It is
        `neutral` and not `danger-outline` — unlike the Deny beside a command, declining to choose
        refuses nothing and destroys nothing. It just moves the conversation back into the
        transcript, which is where a question about a question belongs.
      -->
      <Button
        variant="neutral"
        size="sm"
        title="Ask about these options instead of picking one. The agent replies in the transcript, and can ask again after."
        onclick={() => onanswer({ kind: 'deny', message: discuss() })}
      >
        Discuss first
      </Button>
    {:else if request.kind === 'plan_review'}
      <!-- First, ahead of Approve. Reading the plan is the step this card is asking you to take,
           and putting it after the approve button implies the reverse order.

           Only where a panel exists to open. The pane that owns the plan supplies `onread`; a card
           relayed into *another* pane — a delegated child's plan, shown to its orchestrator — has
           no panel beside it, and the button would have been a control that does nothing. The
           document is still readable, because the body below renders it inline whenever the panel
           is closed. -->
      {#if onread}
        <Button
          variant="neutral"
          size="sm"
          title="Show the whole plan beside the transcript"
          onclick={() => onread?.()}
        >
          {reading ? 'Hide the plan' : 'Read the plan'}
        </Button>
      {/if}
      <Button
        variant="danger-outline"
        size="sm"
        title="Send this back with your notes. The agent keeps planning."
        onclick={() =>
          onanswer({
            kind: 'deny',
            // A default rather than `null`, which the adapter turns into "Denied in wtm" — true of
            // any refusal and useless to a model that is about to write another plan. This is the
            // one denial whose whole purpose is to be read.
            message:
              changes.trim() === ''
                ? 'The plan was not approved. Revise it and propose again.'
                : changes.trim(),
          })}
      >
        Request changes
      </Button>
      <Button variant="accent" size="sm" onclick={() => onanswer({ kind: 'allow' })}>
        Approve
      </Button>
    {:else}
      <Button
        variant="neutral"
        size="sm"
        title="Allow this and anything like it for the rest of this session"
        onclick={() => onanswer({ kind: 'allow_for_session' })}
      >
        Always this session
      </Button>
      <span class="c-approval__divider" aria-hidden="true"></span>
      <Button
        variant="danger-outline"
        size="sm"
        title="Refuse this one. The session carries on with the rest of the turn."
        onclick={() => onanswer({ kind: 'deny', message: null })}
      >
        Deny
      </Button>
      <Button variant="accent" size="sm" onclick={() => onanswer({ kind: 'allow' })}
        >Allow</Button
      >
    {/if}
  </div>
</div>
