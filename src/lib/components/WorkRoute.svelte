<script lang="ts">
  /**
   * One route of a local fix, as far as it got: a follow-up pull request, or a push to their
   * branch. Suggestions post as a review and are a `WorkPublication` card.
   *
   * # Where it went, and each write
   *
   * The card says exactly where the approval sent the fix — the remote, its URL and the ref, and
   * the pull request's two branches for a follow-up — and lists each write with what happened to
   * it, so a route stopped halfway says which half reached the remote. The words are
   * `local-fix.ts`'s; this lays them out, in `.c-work-state` and `.c-publication`'s classes, so a
   * route reads as a publish does.
   *
   * # What it offers is what can settle it
   *
   * A failure offers Retry, which reads the remote before sending anything. A branch that moved
   * after you approved offers nothing that would push: a new approval is a new route, from the
   * outcome sheet, once you have looked at what they pushed.
   */
  import type { RouteSendView } from '../ipc/types';
  import {
    destinationLine,
    routeActions,
    routeApprovalLine,
    routeEyebrow,
    routeOperationDetail,
    routeOperationTitle,
    routeTitle,
    ROUTE_TONE,
  } from '../local-fix';
  import { operationTone } from '../publish';
  import CodeText from './CodeText.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    view,
    busy,
    error,
    onretry,
    onleave,
    onopen,
  }: {
    view: RouteSendView;
    busy: boolean;
    error: string | null;
    onretry: () => void;
    onleave: () => void;
    onopen: (url: string) => void;
  } = $props();

  const tone = $derived(ROUTE_TONE[view.state]);
  const actions = $derived(routeActions(view));
  const checking = $derived(view.state === 'checking');
  const moved = $derived(
    view.operations.some(
      (op) => op.state === 'needs_attention' && op.attention?.kind === 'head_moved',
    ),
  );
</script>

<section
  id="work-route-{view.approval.id}"
  tabindex="-1"
  class="c-work-state c-publication"
  class:c-work-state--info={tone === 'info'}
  class:c-work-state--warn={tone === 'warn'}
  class:c-work-state--danger={tone === 'danger'}
  class:c-work-state--ok={tone === 'ok'}
  aria-labelledby="route-{view.approval.id}"
>
  <span class="c-work-state__eyebrow">{routeEyebrow(view)}</span>
  <h3 class="c-work-state__title" id="route-{view.approval.id}">{routeTitle(view)}</h3>
  <p class="c-work-state__note"><CodeText text={destinationLine(view)} /></p>
  <p class="c-work-state__note">{routeApprovalLine(view)}</p>

  <ol class="o-plain-list c-publication__steps">
    {#each view.operations as op (op.id)}
      <li class="c-publication__step c-publication__step--{operationTone(op)}">
        <span class="c-publication__mark" aria-hidden="true">
          {#if op.state === 'done'}
            <Icon name="check" size={14} />
          {:else if op.state === 'failed'}
            <Icon name="close" size={14} />
          {:else if op.state === 'needs_attention'}
            <Icon name="warn" size={14} />
          {:else if op.state === 'executing'}
            <Icon name="restart" size={14} />
          {:else}
            <span class="c-publication__dot"></span>
          {/if}
        </span>
        <span class="c-publication__text">
          <span class="c-publication__what">{routeOperationTitle(op, checking)}</span>
          <span class="c-publication__detail">
            <CodeText text={routeOperationDetail(op, view.plan.their.branch)} />
            {#if op.state === 'done' && op.result?.kind === 'pull_request'}
              ·
              <Button variant="link" size="sm" onclick={() => onopen(view.url ?? '')}
                >View on GitHub</Button
              >
            {/if}
          </span>
        </span>
      </li>
    {/each}
  </ol>

  {#if checking}
    <p class="c-publication__info" role="status">
      Reading the remote for what this route sent before wtm stopped… If it is there, the
      step is marked done; if not, it waits for your Retry. Nothing is sent on its own.
    </p>
  {:else if moved}
    <p class="c-publication__info">
      Their branch is not where you approved it, so the push was not sent: a push from there
      would be over commits you have not read, and wtm never forces. Re-review their
      changes, or send the fix as a follow-up pull request instead.
    </p>
  {:else if actions.retry}
    <p class="c-publication__info">
      Retry reads the remote first, so a push that arrived is not sent again and a pull
      request that was opened is found rather than opened twice.
    </p>
  {/if}

  {#if error}
    <p class="c-work-state__error" role="alert"><CodeText text={error} /></p>
  {/if}

  {#if actions.retry || actions.leave}
    <div class="c-work-state__actions">
      {#if actions.retry}
        <Button variant="accent" size="sm" disabled={busy} onclick={onretry}>Retry</Button>
      {/if}
      {#if actions.leave}
        <Button variant="quiet" size="sm" disabled={busy} onclick={onleave}
          >Leave as is</Button
        >
      {/if}
    </div>
  {:else if view.state === 'published' && view.url}
    <div class="c-work-state__actions">
      <Button size="sm" onclick={() => onopen(view.url ?? '')}>
        <Icon name="external" size={14} /> View the pull request
      </Button>
    </div>
  {/if}
</section>
