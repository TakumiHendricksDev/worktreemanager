<script lang="ts">
  /**
   * The stack of cards about sessions you are not looking at.
   *
   * # Why this is not in `ui/`
   *
   * Because it is wired to a store and to the app's own navigation. `ui/` holds components that
   * take props and render, which is what lets them be reused without their call sites having to
   * agree about state — this one dismisses toasts and asks the shell to go somewhere.
   *
   * Navigation itself is a prop rather than done here, deliberately: `App.svelte` owns
   * `mainView` and its `goTo` recipe is shared with macOS notification clicks, which name the
   * same kind of target. Two routes to the same place must be the same code, or they drift —
   * this component used to select the worktree itself and never switched projects, which is
   * exactly the bug the shared recipe closed.
   */
  import { attention, type Toast } from '../state/attention.svelte';
  import type { PaneStatus } from '../status';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';
  import SessionDot from './ui/SessionDot.svelte';

  const {
    onnavigate,
  }: {
    /** Take the user to a toast's target — project, worktree, view and pane. */
    onnavigate?: (target: {
      projectId: string;
      worktreeId: string;
      paneId: string;
    }) => void;
  } = $props();

  /** Which dot a card shows. `ask` has no session behind it and renders none. */
  const DOT: Record<Exclude<Toast['kind'], 'ask'>, PaneStatus> = {
    attention: 'attention',
    failed: 'failed',
    done: 'done',
  };

  function go(toast: Toast) {
    if (!toast.target) return;
    onnavigate?.(toast.target);
    // Dismissed explicitly as well as by `markSeen`, because arriving at the worktree is what clears
    // the rest of its cards and this one has already been acted on.
    attention.dismiss(toast.id);
  }
</script>

<!--
  The list exists even when it is empty, and that is deliberate: a live region added to the document
  at the same moment as its first content is not announced, because there was nothing there to observe
  a change against. `pointer-events` on the empty box is handled in `_toast.scss` — read the note
  there, it sits over the Send button.
-->
<ul class="c-toasts" aria-live="polite" aria-label="Session alerts">
  {#each attention.toasts as toast (toast.id)}
    <li class="c-toast c-toast--{toast.kind}">
      {#if toast.kind === 'ask'}
        <!-- The one-time opt-in, asked here rather than in a dialog. It arrives on the first focus
             *after* something was actually missed, so it is a question about an event the user
             remembers — and a modal for it would interrupt whatever they came back to do. -->
        <div class="c-toast__body">
          <span class="c-toast__title">{toast.title}</span>
          <span class="c-toast__detail">{toast.detail}</span>
          <div class="c-toast__actions">
            <Button variant="accent" size="sm" onclick={() => void attention.enable()}>
              Turn on
            </Button>
            <Button variant="neutral" size="sm" onclick={() => void attention.disable()}>
              Not now
            </Button>
          </div>
        </div>
      {:else}
        <button class="c-toast__body" onclick={() => go(toast)}>
          <span class="c-toast__title">
            <SessionDot status={DOT[toast.kind]} />
            {toast.title}
          </span>
          <span class="c-toast__detail">{toast.detail}</span>
        </button>
        <!--
          A sibling of the body, never a child of it. A button inside a button is invalid HTML and
          would break both the outer target and its accessible name — the same note
          `WorktreeTab.svelte` carries about its star, for the same reason.
        -->
        <button
          class="c-toast__close"
          title="Dismiss"
          onclick={() => attention.dismiss(toast.id)}
        >
          <Icon name="close" size={12} />
          <span class="u-visually-hidden">Dismiss</span>
        </button>
      {/if}
    </li>
  {/each}
</ul>
