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
    }
  });
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
  {:else}
    <p class="c-approval__where">{request.prompt}</p>
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
  </div>
</div>
