<script lang="ts">
  /**
   * The Work view's left column: what there is to review, by where it came from.
   *
   * It takes the sidebar's place while Work is up, the way Home's tree does and for the same reason:
   * it lists the same project's worktrees, and two lists of them side by side would be one too many.
   * The sidebar stays mounted underneath, hidden, and comes back as it was.
   *
   * Two sections, the review slice's: My worktrees, from the project's listing, and Reviews, the
   * pull requests you added by URL or number. Adding one is a field at the foot of the section
   * rather than a dialog: it is one line of input, and its answer is a row appearing right above
   * it. The section offers no field at all for a project whose workflow names no forge, and says
   * what to configure instead, because wtm reads no pull request for such a project. The plan's
   * other sections — needs you, running, the sprint — arrive with the runs and the tracker that
   * fill them, not as empty headings now.
   *
   * Rows are buttons in a list rather than a `role="tree"` like the sidebar's. There is nothing to
   * fold and no second level, so a tree's keyboard model would be ceremony with nothing behind it.
   */
  import {
    myWorktrees,
    REVIEW_ROW_WORDS,
    REVIEW_WORDS,
    reviewRows,
    subjectOf,
  } from '../work';
  import { reviews } from '../state/reviews.svelte';
  import { work } from '../state/work.svelte';
  import { workspace } from '../state/workspace.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';
  import TextInput from './ui/TextInput.svelte';

  const {
    visible,
    onnewworktree,
  }: {
    visible: boolean;
    /** New Worktree, which opens the create form in the main column. */
    onnewworktree: () => void;
  } = $props();

  const rows = $derived(myWorktrees(workspace.worktrees));
  const subject = $derived(subjectOf(rows, work.picked, workspace.selectedWorktreeId));
  const queue = $derived(
    reviews.projectId === workspace.activeProjectId ? reviews.queue : null,
  );
  const pullRequests = $derived(reviewRows(queue?.items ?? [], workspace.worktrees));

  let pasted = $state('');

  // Read on arrival and on a project switch, never on a timer: the queue is local, and each pull
  // request is read from the forge only when it is opened.
  $effect(() => {
    const projectId = workspace.activeProjectId;
    if (visible && projectId && workspace.activeProject?.usable)
      void reviews.load(projectId);
  });

  async function addPullRequest(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const projectId = workspace.activeProjectId;
    const input = pasted.trim();
    if (!projectId || !input || reviews.adding) return;
    const id = await reviews.add(projectId, input);
    if (id !== null) {
      pasted = '';
      work.pickReview(id);
    }
  }
</script>

<nav class="c-work-queue" class:is-hidden={!visible} aria-label="Work queue">
  <h2 class="c-work-queue__title">Queue</h2>

  <div class="c-work-queue__scroll">
    <section class="c-work-queue__section" aria-labelledby="work-queue-mine">
      <h3 class="c-work-queue__heading" id="work-queue-mine">
        My worktrees <span class="c-work-queue__count">{rows.length}</span>
      </h3>
      {#if rows.length > 0}
        <ul class="o-plain-list">
          {#each rows as row (row.id)}
            <li>
              <button
                class="c-work-row"
                class:is-selected={work.review === null && row.id === subject?.id}
                aria-current={work.review === null && row.id === subject?.id
                  ? 'true'
                  : undefined}
                title={row.detail}
                onclick={() => work.pick(row.id)}
              >
                <!-- Hollow, the shape `c-dot` gives a thing doing nothing; the word beside it is
                     what says so, so the state is never in the shape alone. -->
                <span class="c-dot c-dot--idle c-work-row__mark" aria-hidden="true"></span>
                <span class="c-work-row__text">
                  <span class="c-work-row__name">
                    {#if row.key}<span class="c-work-row__key">{row.key}</span
                      >{#if row.name}{' · '}{/if}{/if}{row.name}
                  </span>
                  <span class="c-work-row__status">{REVIEW_WORDS[row.review]}</span>
                </span>
              </button>
            </li>
          {/each}
        </ul>
      {:else if !workspace.loadingWorktrees}
        <p class="c-work-queue__empty">
          {workspace.activeProject
            ? 'No worktrees in this project yet.'
            : 'Add a repository to see its worktrees here.'}
        </p>
      {/if}
    </section>

    <section class="c-work-queue__section" aria-labelledby="work-queue-reviews">
      <h3 class="c-work-queue__heading" id="work-queue-reviews">
        Reviews <span class="c-work-queue__count">{pullRequests.length}</span>
      </h3>
      {#if pullRequests.length > 0}
        <ul class="o-plain-list">
          {#each pullRequests as row (row.id)}
            <li>
              <button
                class="c-work-row"
                class:is-selected={work.review === row.id}
                aria-current={work.review === row.id ? 'true' : undefined}
                title={row.detail}
                onclick={() => work.pickReview(row.id)}
              >
                <span
                  class="c-dot c-work-row__mark"
                  class:c-dot--idle={row.status === 'no_worktree'}
                  class:c-dot--done={row.status === 'worktree'}
                  aria-hidden="true"
                ></span>
                <span class="c-work-row__text">
                  <span class="c-work-row__name">{row.name}</span>
                  <span class="c-work-row__status">{REVIEW_ROW_WORDS[row.status]}</span>
                </span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}

      {#if queue?.forge}
        {#if pullRequests.length === 0}
          <p class="c-work-queue__empty">
            Nothing to review yet. Add a pull request by its URL or number.
          </p>
        {/if}
        <form class="c-work-queue__add" onsubmit={addPullRequest}>
          <TextInput
            bind:value={pasted}
            mono
            placeholder="URL or #number"
            ariaLabel="Pull request URL or number"
            disabled={reviews.adding}
          />
          <Button type="submit" size="sm" disabled={reviews.adding || pasted.trim() === ''}>
            {reviews.adding ? 'Reading…' : 'Add'}
          </Button>
        </form>
        {#if reviews.addError}
          <p class="c-work-queue__error c-status--danger" role="alert">
            {reviews.addError}
          </p>
        {/if}
      {:else if queue}
        <p class="c-work-queue__empty">
          This project's workflow names no forge, so wtm reads no pull requests for it. Add
          <code>forge = "github"</code> to its <code>[workflow]</code> in your own config.
        </p>
      {/if}
      {#if reviews.queueError && reviews.projectId === workspace.activeProjectId}
        <p class="c-work-queue__error c-status--danger" role="alert">
          {reviews.queueError}
        </p>
      {/if}
    </section>
  </div>

  <div class="c-work-queue__foot">
    <!-- Neutral where the sidebar's is accent: here the view is for reviewing, and its one
         primary action belongs to the workspace. Same label and gate as the sidebar's. -->
    <Button
      variant="neutral"
      full
      onclick={onnewworktree}
      disabled={!workspace.activeProject?.usable}
    >
      <Icon name="plus" size={14} /> New Worktree
    </Button>
  </div>
</nav>
