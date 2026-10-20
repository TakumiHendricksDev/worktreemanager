<script lang="ts">
  /**
   * The Work view's left column: what there is to review, by where it came from.
   *
   * It takes the sidebar's place while Work is up, the way Home's tree does and for the same reason:
   * it lists the same project's worktrees, and two lists of them side by side would be one too many.
   * The sidebar stays mounted underneath, hidden, and comes back as it was.
   *
   * Two sections, the review slice's: My worktrees, from the project's listing, and Reviews, which
   * stays empty until pull requests can be read. The plan's other sections — needs you, running,
   * the sprint — arrive with the runs and the tracker that fill them, not as empty headings now.
   *
   * Rows are buttons in a list rather than a `role="tree"` like the sidebar's. There is nothing to
   * fold and no second level, so a tree's keyboard model would be ceremony with nothing behind it.
   */
  import { myWorktrees, REVIEW_WORDS, subjectOf } from '../work';
  import { work } from '../state/work.svelte';
  import { workspace } from '../state/workspace.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

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
                class:is-selected={row.id === subject?.id}
                aria-current={row.id === subject?.id ? 'true' : undefined}
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
        Reviews <span class="c-work-queue__count">0</span>
      </h3>
      <p class="c-work-queue__empty">
        Nothing to review yet. Pull requests waiting on your review will be listed here.
      </p>
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
