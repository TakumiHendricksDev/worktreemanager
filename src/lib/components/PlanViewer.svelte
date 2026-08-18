<script lang="ts">
  /**
   * A plan, rendered to be read.
   *
   * # Why this exists
   *
   * A plan was the one document in the app with no reading surface. Approving one showed its
   * markdown as literal source in a 14-line `<pre>` — `##` and `**` and fenced code all visible as
   * punctuation — and a *stored* plan was reachable only as a native `title=` tooltip truncated to
   * 400 characters. Both are the same failure: a plan is the longest and most structured thing an
   * agent produces, and it was the only thing not going through the markdown renderer that every
   * assistant message already used.
   *
   * # Why a dialog rather than a pane
   *
   * Reading a plan is a thing you do *once*, deciding something — approve it, or hand it to another
   * agent. A pane would compete for the tiling budget (`MAX_PANES_PER_WORKTREE`) with the sessions
   * doing the actual work, and it would still be there tomorrow. A modal is the shape of "give me
   * the whole document for a minute", and `Dialog` already owns the scrim, Escape, and the focus
   * trap.
   *
   * # Deliberately not an outline
   *
   * No table of contents, no checkbox tracking, no diffing against the previous revision. The
   * request was to read the plan cleanly; the parser already handles the headings, nested lists and
   * fences that plans are actually made of, and a navigation tree for a document that fits in three
   * screens is furniture. `markdown.ts`'s omissions apply — notably tables, which Claude plans do
   * use occasionally and which will render as literal pipes. Worth fixing there rather than here.
   */
  import type { Snippet } from 'svelte';

  import Markdown from './Markdown.svelte';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';

  const {
    title,
    markdown,
    provider = null,
    created = null,
    path = null,
    onclose,
    actions,
  }: {
    title: string;
    markdown: string;
    /** Which agent wrote it. Absent for a plan being approved — the pane already says. */
    provider?: string | null;
    /** ISO timestamp, as stored on a `Brief`. */
    created?: string | null;
    /** Where the provider wrote its own copy, when it said. */
    path?: string | null;
    onclose: () => void;
    /** Whatever the caller wants offered beside Close — a hand-off, a Forget. */
    actions?: Snippet;
  } = $props();

  /*
   * The date only, and in the user's own locale.
   *
   * A stored plan is dated in whole days as far as anyone cares — "which plan was this" is answered
   * by the title, not the minute — and a full timestamp in a footer is noise that pushes the
   * provider name off a narrow dialog.
   */
  const when = $derived.by(() => {
    if (!created) return null;
    const parsed = new Date(created);
    return Number.isNaN(parsed.getTime())
      ? null
      : new Intl.DateTimeFormat(undefined, { dateStyle: 'medium' }).format(parsed);
  });
</script>

<Dialog {title} {onclose} wide>
  {#snippet body()}
    <div class="c-planview">
      <Markdown source={markdown} />
    </div>
    {#if provider || when || path}
      <p class="c-planview__meta">
        {#if provider}<span>{provider}</span>{/if}
        {#if when}<span>{when}</span>{/if}
        {#if path}<code>{path}</code>{/if}
      </p>
    {/if}
  {/snippet}

  {#snippet footer()}
    {#if actions}{@render actions()}{/if}
    <Button variant="neutral" size="sm" onclick={onclose}>Close</Button>
  {/snippet}
</Dialog>
