<script lang="ts">
  /**
   * One comment, drawn under the lines it is about inside the viewer: a box to write it in, or the
   * comment as saved with what can be done to it.
   *
   * Mounted by `CodeViewer` into a CodeMirror block widget rather than rendered by a parent, so it
   * is a component with its props fixed at mount — the widget is rebuilt when the comment changes,
   * which CodeMirror decides by comparing the widgets, not by Svelte noticing.
   */
  import type { CodeComment } from '../ipc/types';
  import Button from './ui/Button.svelte';

  const {
    comment,
    lines,
    outdated = false,
    onsave,
    oncancel,
    onresolve,
    ondelete,
  }: {
    /** Null while this is the box a new comment is being written in. */
    comment: CodeComment | null;
    /** `12` or `12–18`, for the placeholder and the label. */
    lines: string;
    outdated?: boolean;
    onsave: (text: string) => Promise<void> | void;
    oncancel: () => void;
    onresolve: (resolved: boolean) => void;
    ondelete: () => void;
  } = $props();

  // A new comment starts empty and an edit starts from the saved text; both are this box's after.
  // svelte-ignore state_referenced_locally
  let text = $state(comment?.text ?? '');
  // svelte-ignore state_referenced_locally
  let editing = $state(comment === null);
  let saving = $state(false);
  let failed = $state<string | null>(null);

  function focus(node: HTMLTextAreaElement): void {
    // After the widget is in the document, so the caret lands and the page does not jump.
    requestAnimationFrame(() => node.focus({ preventScroll: true }));
  }

  async function save(): Promise<void> {
    if (text.trim() === '' || saving) return;
    saving = true;
    failed = null;
    try {
      await onsave(text.trim());
      if (comment) editing = false;
    } catch (error) {
      failed = (error as { message?: string } | null)?.message ?? String(error);
    } finally {
      saving = false;
    }
  }

  function cancel(): void {
    if (comment) {
      text = comment.text;
      editing = false;
    } else {
      oncancel();
    }
  }

  function onKeydown(event: KeyboardEvent): void {
    if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      void save();
    } else if (event.key === 'Escape') {
      event.preventDefault();
      event.stopPropagation();
      cancel();
    }
  }
</script>

<div class="c-code-comment" class:is-resolved={comment?.status === 'resolved'}>
  {#if editing}
    <textarea
      class="c-code-comment__input"
      rows="3"
      placeholder={`Comment on line${lines.includes('–') ? 's' : ''} ${lines}…`}
      aria-label={`Comment on lines ${lines}`}
      bind:value={text}
      onkeydown={onKeydown}
      use:focus></textarea>
    {#if failed}<p class="c-code-comment__error">{failed}</p>{/if}
    <div class="c-code-comment__actions">
      <Button
        variant="accent"
        size="sm"
        disabled={saving || text.trim() === ''}
        onclick={save}>{comment ? 'Save' : 'Comment'}</Button
      >
      <Button variant="quiet" size="sm" onclick={cancel}>Cancel</Button>
      <span class="c-code-comment__hint">⌘↵ to save</span>
    </div>
  {:else if comment}
    <p class="c-code-comment__text">{comment.text}</p>
    <div class="c-code-comment__meta">
      <span>Lines {lines}</span>
      {#if comment.status === 'resolved'}<span class="c-code-comment__tag">Resolved</span
        >{/if}
      {#if comment.sentTo}<span class="c-code-comment__tag">Sent to {comment.sentTo}</span
        >{/if}
      {#if outdated}
        <span class="c-code-comment__tag c-code-comment__tag--warn">Outdated</span>
      {/if}
      <span class="c-code-comment__spacer"></span>
      <Button variant="link" size="sm" onclick={() => (editing = true)}>Edit</Button>
      <Button
        variant="link"
        size="sm"
        onclick={() => onresolve(comment.status !== 'resolved')}
        >{comment.status === 'resolved' ? 'Reopen' : 'Resolve'}</Button
      >
      <Button variant="link" size="sm" onclick={ondelete}>Delete</Button>
    </div>
    {#if comment.note}
      <p class="c-code-comment__note">{comment.note}</p>
    {/if}
  {/if}
</div>
