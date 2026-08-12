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
   * # Why the verbs differ by provider
   *
   * `AllowWithEdits` exists because Claude Code's allow can carry a replacement payload and rewrite
   * a tool call. Codex has no such verb and refuses the answer rather than running the original
   * unedited. So the affordance is absent where it cannot be honoured rather than present and
   * broken — `canEdit` is the prop that says which.
   */
  import type { ApprovalAnswer, ApprovalRequest } from '../ipc/types';
  import Button from './ui/Button.svelte';

  const {
    request,
    canEdit = false,
    onanswer,
  }: {
    request: ApprovalRequest;
    /** True only for a provider whose allow can carry a rewritten payload. */
    canEdit?: boolean;
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

  const questionsKey = $derived(
    request.kind === 'user_input'
      ? request.questions.map((question) => question.id).join('\0')
      : '',
  );

  // A second queued question can replace this card without remounting the component. Never carry
  // choices or notes from one provider request into the next one.
  $effect(() => {
    void questionsKey;
    selections = {};
    other = {};
    otherSelected = {};
    notes = '';
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

<div class="c-approval" role="alertdialog" aria-label={heading}>
  <p class="c-approval__ask">{heading}</p>

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
    <pre class="c-approval__body c-approval__body--plan">{request.markdown}</pre>
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
            {#if question.multiple}<small>Select all that apply</small>{/if}
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
                    {#if option.description}<small>{option.description}</small>{/if}
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
            <input
              class="c-input c-approval__freeform"
              type={question.secret ? 'password' : 'text'}
              aria-label={question.question}
              placeholder="Type your answer"
              value={other[question.id] ?? ''}
              oninput={(event) =>
                (other = { ...other, [question.id]: event.currentTarget.value })}
            />
          {/if}
        </fieldset>
      {/each}
    </div>

    <label class="c-approval__notes">
      <span>Notes <small>optional</small></span>
      <textarea
        class="c-textarea"
        rows="2"
        placeholder="Add context, constraints, or a custom response"
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

  <div class="o-row c-approval__actions">
    {#if request.kind === 'user_input'}
      <Button variant="accent" size="sm" disabled={!complete} onclick={submitAnswers}>
        Submit answer
      </Button>
    {:else}
      <Button variant="accent" size="sm" onclick={() => onanswer({ kind: 'allow' })}
        >Allow</Button
      >
      <Button
        variant="neutral"
        size="sm"
        title="Allow this and anything like it for the rest of this session"
        onclick={() => onanswer({ kind: 'allow_for_session' })}
      >
        Always this session
      </Button>
      {#if canEdit}
        <Button
          variant="neutral"
          size="sm"
          title="Not available for this agent"
          onclick={() => onanswer({ kind: 'allow_with_edits', input: {} })}
        >
          Edit…
        </Button>
      {/if}
      <!-- Past the divider, alone, matching the worktree header's treatment of Remove: a
         destructive-feeling control flush against a neutral one is how it gets clicked by
         accident. Deny is not destructive, but it does end something the user was waiting for. -->
      <span class="c-approval__divider" aria-hidden="true"></span>
      <Button
        variant="danger-outline"
        size="sm"
        title="Refuse this one. The session carries on with the rest of the turn."
        onclick={() => onanswer({ kind: 'deny', message: null })}
      >
        Deny
      </Button>
    {/if}
  </div>
</div>
